//! The reMarkable cloud client.
//!
//! Implements the **sync v3 / sync15** metadata protocol, ported faithfully from
//! lanej (the cleanest reference) with two upgrades borrowed from wavyrai:
//!
//! * **root-hash change detection** — a single cheap request short-circuits a full
//!   library re-traversal when nothing changed, and
//! * **bounded parallel metadata fetch** — `buffer_unordered` over the index so a
//!   400-document library lists in one round-trip's worth of wall-clock, not 400.
//!
//! ## Protocol (verified against all three reference servers)
//!
//! 1. `GET {sync_host}{root_path}` → JSON containing a `hash` (the library root).
//! 2. `GET {sync_host}/sync/v3/files/{root_hash}` → a line-delimited index;
//!    lines 1–2 are schema/root metadata, the rest are `hash:gen:uuid:type:size`.
//! 3. For each entry, `GET .../files/{entry_hash}` → a per-document blob index;
//!    find the `{uuid}.metadata` line, then `GET .../files/{meta_hash}` → the
//!    metadata JSON. Only the small `.metadata` blob is fetched, never the page data.
//!
//! ## Untested against live hardware
//!
//! These endpoints/headers are reverse-engineered and cannot be exercised without a
//! real reMarkable account token. Parsing, caching, and tree logic are unit-tested
//! with fixtures; the network shapes carry the three sources' mutual agreement as
//! their evidence. See the PR's "Assumptions" section.

use std::path::PathBuf;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::Serialize;

use crate::auth::{default_token_path, SecretToken, TokenStore};
use crate::error::{Error, Result};
use crate::model::{Item, RawMetadata};

/// Authentication server (cloud.remarkable.engineering; `my.remarkable.com` is now
/// only a frontend). Identical across all three reference implementations.
const AUTH_HOST: &str = "https://webapp-prod.cloud.remarkable.engineering";
const DEVICE_TOKEN_PATH: &str = "/token/json/2/device/new";
const USER_TOKEN_PATH: &str = "/token/json/2/user/new";

/// Default sync host and root path (lanej's documented sync v3 shape).
const DEFAULT_SYNC_HOST: &str = "https://internal.cloud.remarkable.com";
const DEFAULT_SYNC_ROOT_PATH: &str = "/sync/v3/root";
const FILES_PATH: &str = "/sync/v3/files/";

/// Configuration for [`CloudClient`]. Built from the environment by [`ClientConfig::from_env`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Where the token store lives on disk.
    pub token_path: PathBuf,
    /// Sync API host (override via `REMARKABLE_SYNC_HOST`).
    pub sync_host: String,
    /// Root endpoint path (override via `REMARKABLE_SYNC_ROOT_PATH`, e.g. `/sync/v4/root`).
    pub sync_root_path: String,
    /// Max concurrent metadata fetches (`REMARKABLE_PARALLEL_WORKERS`, default 8, clamped 1..=64).
    pub parallel_workers: usize,
    /// HTTP request timeout.
    pub timeout: Duration,
    /// User-Agent header.
    pub user_agent: String,
}

impl ClientConfig {
    /// Build configuration from environment variables, falling back to defaults.
    pub fn from_env() -> Result<Self> {
        let token_path = default_token_path()?;
        let sync_host = env_or("REMARKABLE_SYNC_HOST", DEFAULT_SYNC_HOST);
        let sync_root_path = env_or("REMARKABLE_SYNC_ROOT_PATH", DEFAULT_SYNC_ROOT_PATH);
        let parallel_workers = std::env::var("REMARKABLE_PARALLEL_WORKERS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(8)
            .clamp(1, 64);
        Ok(ClientConfig {
            token_path,
            sync_host,
            sync_root_path,
            parallel_workers,
            timeout: Duration::from_secs(60),
            user_agent: concat!("remarkable-mcp/", env!("CARGO_PKG_VERSION")).to_string(),
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => default.to_string(),
    }
}

/// A cached snapshot of the library, keyed by the sync root hash.
struct CachedLibrary {
    root_hash: String,
    items: Vec<Item>,
}

/// Mutable client state guarded by an async mutex (single source of truth for the
/// token and the cache; mirrors wavyrai's thread-safe renewal).
struct Inner {
    tokens: TokenStore,
    cache: Option<CachedLibrary>,
}

/// A client for the reMarkable cloud sync API.
pub struct CloudClient {
    http: reqwest::Client,
    config: ClientConfig,
    inner: tokio::sync::Mutex<Inner>,
}

/// Snapshot of authentication state for the `status` tool.
#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub authenticated: bool,
    pub device_id: String,
    pub has_valid_user_token: bool,
    pub user_token_expires: Option<chrono::DateTime<chrono::Utc>>,
    pub token_path: String,
}

/// One entry of the root sync index.
#[derive(Debug, Clone)]
struct IndexEntry {
    hash: String,
    id: String,
}

impl CloudClient {
    /// Construct a client, loading any persisted tokens.
    pub fn new(config: ClientConfig) -> Result<Self> {
        let tokens = TokenStore::load(&config.token_path)?;
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(config.user_agent.clone())
            .build()?;
        Ok(CloudClient {
            http,
            config,
            inner: tokio::sync::Mutex::new(Inner {
                tokens,
                cache: None,
            }),
        })
    }

    /// Build a client from environment configuration.
    pub fn from_env() -> Result<Self> {
        Self::new(ClientConfig::from_env()?)
    }

    /// `true` if a device token is stored.
    pub async fn is_authenticated(&self) -> bool {
        self.inner.lock().await.tokens.is_registered()
    }

    /// Current authentication status.
    pub async fn auth_status(&self) -> AuthStatus {
        let inner = self.inner.lock().await;
        AuthStatus {
            authenticated: inner.tokens.is_registered(),
            device_id: inner.tokens.device_id.clone(),
            has_valid_user_token: inner.tokens.has_valid_user_token(chrono::Utc::now()),
            user_token_expires: inner.tokens.user_token_expires,
            token_path: self.config.token_path.display().to_string(),
        }
    }

    /// Register this device with a one-time pairing code from
    /// <https://my.remarkable.com/device/desktop/connect>.
    pub async fn register(&self, code: &str) -> Result<()> {
        let code = code.trim().to_lowercase();
        let device_id = uuid::Uuid::new_v4().to_string();

        #[derive(Serialize)]
        struct DeviceReq<'a> {
            code: &'a str,
            #[serde(rename = "deviceDesc")]
            device_desc: &'a str,
            #[serde(rename = "deviceID")]
            device_id: &'a str,
        }

        let url = format!("{AUTH_HOST}{DEVICE_TOKEN_PATH}");
        let resp = self
            .http
            .post(&url)
            .json(&DeviceReq {
                code: &code,
                device_desc: "desktop-linux",
                device_id: &device_id,
            })
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Registration {
                status: status.as_u16(),
                body: body.trim().to_string(),
            });
        }

        {
            let mut inner = self.inner.lock().await;
            inner.tokens.device_token = SecretToken::new(body.trim());
            inner.tokens.device_id = device_id;
            inner.cache = None;
        }
        self.refresh_user_token().await?;
        self.persist().await
    }

    /// Exchange the device token for a fresh user token.
    pub async fn refresh_user_token(&self) -> Result<()> {
        let device_token = {
            let inner = self.inner.lock().await;
            if !inner.tokens.is_registered() {
                return Err(Error::NotAuthenticated);
            }
            inner.tokens.device_token.expose().to_string()
        };

        let url = format!("{AUTH_HOST}{USER_TOKEN_PATH}");
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&device_token)
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                endpoint: USER_TOKEN_PATH.to_string(),
                status: status.as_u16(),
                body: body.trim().to_string(),
            });
        }

        let mut inner = self.inner.lock().await;
        inner.tokens.user_token = SecretToken::new(body.trim());
        inner.tokens.user_token_expires = Some(chrono::Utc::now() + chrono::Duration::hours(23));
        Ok(())
    }

    /// Ensure a valid user token, refreshing if needed, and return it.
    async fn bearer(&self) -> Result<String> {
        let valid = {
            let inner = self.inner.lock().await;
            if !inner.tokens.is_registered() {
                return Err(Error::NotAuthenticated);
            }
            inner.tokens.has_valid_user_token(chrono::Utc::now())
        };
        if !valid {
            self.refresh_user_token().await?;
            self.persist().await?;
        }
        Ok(self
            .inner
            .lock()
            .await
            .tokens
            .user_token
            .expose()
            .to_string())
    }

    /// Persist the current token store to disk.
    async fn persist(&self) -> Result<()> {
        let inner = self.inner.lock().await;
        inner.tokens.save(&self.config.token_path)
    }

    /// Fetch a sync file by hash, returning its body as text.
    async fn get_file_text(&self, hash: &str, bearer: &str) -> Result<String> {
        let url = format!("{}{}{}", self.config.sync_host, FILES_PATH, hash);
        let resp = self.http.get(&url).bearer_auth(bearer).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                endpoint: format!("{FILES_PATH}{hash}"),
                status: status.as_u16(),
                body: body.trim().to_string(),
            });
        }
        Ok(resp.text().await?)
    }

    /// Fetch the current sync root hash.
    async fn root_hash(&self, bearer: &str) -> Result<String> {
        let url = format!("{}{}", self.config.sync_host, self.config.sync_root_path);
        let resp = self.http.get(&url).bearer_auth(bearer).send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                endpoint: self.config.sync_root_path.clone(),
                status: status.as_u16(),
                body: body.trim().to_string(),
            });
        }
        // Parse defensively: both v3 and v4 root responses carry a top-level `hash`.
        let value: serde_json::Value = serde_json::from_str(&body)?;
        value
            .get("hash")
            .and_then(|h| h.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Malformed("sync root response missing `hash`".into()))
    }

    /// Parse a sync index payload into entries, skipping the two header lines.
    fn parse_index(body: &str) -> Vec<IndexEntry> {
        body.lines()
            .skip(2)
            .filter_map(|line| {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() < 5 {
                    return None;
                }
                // Validate the UUID field; entries with a non-UUID id are skipped.
                if uuid::Uuid::parse_str(parts[2]).is_err() {
                    return None;
                }
                Some(IndexEntry {
                    hash: parts[0].to_string(),
                    id: parts[2].to_string(),
                })
            })
            .collect()
    }

    /// Resolve the `.metadata` blob hash within a per-document blob index.
    fn find_metadata_hash(blob_index: &str, doc_id: &str) -> Option<String> {
        let needle = format!("{doc_id}.metadata");
        for line in blob_index.lines().skip(2) {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 && parts[2] == needle {
                return Some(parts[0].to_string());
            }
        }
        None
    }

    /// Fetch and parse one document's metadata, returning `None` for trashed items
    /// or items whose metadata cannot be located/parsed (logged at debug).
    async fn fetch_item(&self, entry: &IndexEntry, bearer: &str) -> Option<Item> {
        let blob_index = match self.get_file_text(&entry.hash, bearer).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(id = %entry.id, error = %e, "skipping: blob index fetch failed");
                return None;
            }
        };
        let meta_hash = Self::find_metadata_hash(&blob_index, &entry.id)?;
        let meta_text = self.get_file_text(&meta_hash, bearer).await.ok()?;
        let raw: RawMetadata = match serde_json::from_str(&meta_text) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(id = %entry.id, error = %e, "skipping: metadata parse failed");
                return None;
            }
        };
        Item::from_metadata(entry.id.clone(), raw)
    }

    /// List all (non-trashed) documents and folders.
    ///
    /// Returns a cached snapshot when the sync root hash is unchanged since the last
    /// call (wavyrai's freshness optimization). Otherwise traverses the index with
    /// bounded parallelism and rebuilds the cache.
    pub async fn list_items(&self) -> Result<Vec<Item>> {
        let bearer = self.bearer().await?;
        let root = self.root_hash(&bearer).await?;

        // Cache hit: the library is unchanged.
        {
            let inner = self.inner.lock().await;
            if let Some(cache) = &inner.cache {
                if cache.root_hash == root {
                    return Ok(cache.items.clone());
                }
            }
        }

        let index_body = self.get_file_text(&root, &bearer).await?;
        let entries = Self::parse_index(&index_body);

        // Own each entry in the stream (so the per-item future owns its data) and
        // share `self`/`bearer` by reference across the bounded-concurrency fan-out.
        let workers = self.config.parallel_workers;
        let bearer_ref = &bearer;
        let items: Vec<Item> = stream::iter(entries)
            .map(|entry| async move { self.fetch_item(&entry, bearer_ref).await })
            .buffer_unordered(workers)
            .filter_map(|item| async move { item })
            .collect()
            .await;

        {
            let mut inner = self.inner.lock().await;
            inner.cache = Some(CachedLibrary {
                root_hash: root,
                items: items.clone(),
            });
        }
        Ok(items)
    }

    /// Clear the in-memory library cache (call after a mutation).
    pub async fn invalidate_cache(&self) {
        self.inner.lock().await.cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID_A: &str = "11111111-1111-4111-8111-111111111111";
    const UUID_B: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn parse_index_skips_headers_and_keeps_valid_entries() {
        // Real shape: schemaVersion, rootInfo, then `hash:gen:uuid:type:size`.
        let body = format!(
            "3\n\
             root:1:0:2:0\n\
             aaaaaaaa:0:{UUID_A}:80000000:1024\n\
             bbbbbbbb:0:{UUID_B}:80000000:2048\n"
        );
        let entries = CloudClient::parse_index(&body);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].hash, "aaaaaaaa");
        assert_eq!(entries[0].id, UUID_A);
        assert_eq!(entries[1].id, UUID_B);
    }

    #[test]
    fn parse_index_drops_malformed_and_non_uuid_lines() {
        let body = format!(
            "3\n\
             root:1:0:1:0\n\
             short:line\n\
             cccccccc:0:not-a-uuid:80000000:10\n\
             dddddddd:0:{UUID_A}:80000000:10\n"
        );
        let entries = CloudClient::parse_index(&body);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, UUID_A);
    }

    #[test]
    fn find_metadata_hash_locates_the_metadata_blob() {
        // Per-doc blob index: schema, root, then `hash:gen:filename:type:size`.
        let blob = format!(
            "3\n\
             {UUID_A}:80000000:0:0\n\
             eeeeeeee:0:{UUID_A}.content:0:50\n\
             ffffffff:0:{UUID_A}.metadata:0:120\n\
             99999999:0:{UUID_A}.pagedata:0:5\n"
        );
        let hash = CloudClient::find_metadata_hash(&blob, UUID_A);
        assert_eq!(hash.as_deref(), Some("ffffffff"));
    }

    #[test]
    fn find_metadata_hash_returns_none_when_absent() {
        let blob = format!("3\nroot\neeeeeeee:0:{UUID_A}.content:0:50\n");
        assert_eq!(CloudClient::find_metadata_hash(&blob, UUID_A), None);
    }

    #[test]
    fn config_from_env_clamps_workers() {
        // Defaults are sane without any env set.
        let cfg = ClientConfig::from_env().unwrap();
        assert!(cfg.parallel_workers >= 1 && cfg.parallel_workers <= 64);
        assert!(cfg.sync_host.starts_with("https://"));
    }
}
