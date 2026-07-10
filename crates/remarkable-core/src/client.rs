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
//! ## Protocol (verified against a live account — 41-document library)
//!
//! 1. `GET {sync_host}{root_path}` → JSON containing a `hash` (the library root).
//! 2. `GET {sync_host}/sync/v3/files/{root_hash}` with header
//!    `rm-filename: root.docSchema` → a line-delimited index; line 0 is the schema
//!    version, the rest are `hash:type:id:subfiles:size`.
//! 3. For each entry, `GET .../files/{entry_hash}` with `rm-filename:
//!    {id}.docSchema` → a per-document blob index; find the `{uuid}.metadata`
//!    line, then `GET .../files/{meta_hash}` with `rm-filename: {uuid}.metadata`
//!    → the metadata JSON. Only the small `.metadata` blob is fetched, never page data.
//!
//! ## Provenance of the network shapes
//!
//! Parsing/caching/tree logic is unit-tested with fixtures. The **`rm-filename`
//! header** requirement and the **single-schema-line index format** are the parts
//! lanej's older code got wrong (it returned HTTP 400 against today's API); both
//! were corrected from SamMorrowDrums' live fix and confirmed end-to-end against a
//! real reMarkable cloud account.

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
    /// Device kind sent at registration (`REMARKABLE_DEVICE_DESC`). This — not a
    /// free-text name — is what the reMarkable web "devices" view labels the app
    /// from. Must be one of reMarkable's recognized values (see [`KNOWN_DEVICE_DESCS`]);
    /// defaults to the current platform so a Mac shows as a Mac app, not "Linux app".
    pub device_desc: String,
}

/// The device-kind descriptors the reMarkable cloud recognizes. reMarkable rejects
/// anything else, and there is **no** free-text custom-name field — the web view
/// label is derived from this value.
pub const KNOWN_DEVICE_DESCS: &[&str] = &[
    "desktop-windows",
    "desktop-macos",
    "desktop-linux",
    "mobile-android",
    "mobile-ios",
    "browser-chrome",
    "remarkable",
];

/// The platform-appropriate default device descriptor.
fn default_device_desc() -> &'static str {
    if cfg!(target_os = "macos") {
        "desktop-macos"
    } else if cfg!(target_os = "windows") {
        "desktop-windows"
    } else {
        "desktop-linux"
    }
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
            device_desc: env_or("REMARKABLE_DEVICE_DESC", default_device_desc()),
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

    /// The device-kind descriptor this client registers with (see [`KNOWN_DEVICE_DESCS`]).
    pub fn device_desc(&self) -> &str {
        &self.config.device_desc
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
        let device_desc = self.config.device_desc.as_str();

        // reMarkable rejects unrecognized descriptors; warn early rather than fail opaque.
        if !KNOWN_DEVICE_DESCS.contains(&device_desc) {
            tracing::warn!(
                device_desc,
                "REMARKABLE_DEVICE_DESC is not a recognized reMarkable device kind; \
                 registration may be rejected. Known values: {}",
                KNOWN_DEVICE_DESCS.join(", ")
            );
        }

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
                device_desc,
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
    ///
    /// The current reMarkable sync v3 API **requires** an `rm-filename` header on
    /// every `/sync/v3/files/{hash}` GET (omitting it returns HTTP 400
    /// `unexpected 'rm-filename' http header`). The value is the logical name of
    /// the blob being fetched: `root.docSchema` for the root index,
    /// `{id}.docSchema` for a per-document blob index, or the blob's own filename
    /// (e.g. `{uuid}.metadata`) for a leaf file. Verified against SamMorrowDrums'
    /// live fix (their PR #120).
    async fn get_file_text(&self, hash: &str, rm_filename: &str, bearer: &str) -> Result<String> {
        let url = format!("{}{}{}", self.config.sync_host, FILES_PATH, hash);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(bearer)
            .header("rm-filename", rm_filename)
            .send()
            .await?;
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

    /// Parse the root sync index into entries.
    ///
    /// Format (sync v3 / sync15): line 0 is the schema version, then each line is
    /// `hash:type:id:subfiles:size`, where `id` is the item's UUID. Ported from
    /// SamMorrowDrums' `_parse_index` (which skips only the schema line).
    fn parse_index(body: &str) -> Vec<IndexEntry> {
        body.lines()
            .skip(1)
            .filter_map(|line| {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() < 5 {
                    return None;
                }
                // Root-level ids are UUIDs; skip anything that isn't one.
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

    /// Locate the `.metadata` blob within a per-document blob index, returning its
    /// `(hash, filename)` — the filename is needed as the `rm-filename` header.
    /// The blob index shares the root index format (`hash:type:id:subfiles:size`),
    /// but here `id` is a filename like `{uuid}.metadata`.
    fn find_metadata_blob(blob_index: &str) -> Option<(String, String)> {
        for line in blob_index.lines().skip(1) {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 5 && parts[2].ends_with(".metadata") {
                return Some((parts[0].to_string(), parts[2].to_string()));
            }
        }
        None
    }

    /// Fetch and parse one document's metadata, returning `None` for trashed items
    /// or items whose metadata cannot be located/parsed (logged at debug).
    async fn fetch_item(&self, entry: &IndexEntry, bearer: &str) -> Option<Item> {
        // The per-document blob index is addressed by `{id}.docSchema`.
        let blob_filename = format!("{}.docSchema", entry.id);
        let blob_index = match self
            .get_file_text(&entry.hash, &blob_filename, bearer)
            .await
        {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(id = %entry.id, error = %e, "skipping: blob index fetch failed");
                return None;
            }
        };
        let (meta_hash, meta_name) = Self::find_metadata_blob(&blob_index)?;
        let meta_text = self
            .get_file_text(&meta_hash, &meta_name, bearer)
            .await
            .ok()?;
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

        // The root index is addressed by the special name `root.docSchema`.
        let index_body = self.get_file_text(&root, "root.docSchema", &bearer).await?;
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
    fn parse_index_skips_schema_line_and_keeps_valid_entries() {
        // sync v3 shape: schema version, then `hash:type:id:subfiles:size`.
        let body = format!(
            "3\n\
             aaaaaaaa:80000000:{UUID_A}:8:1024\n\
             bbbbbbbb:80000000:{UUID_B}:4:2048\n"
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
             short:line\n\
             cccccccc:80000000:not-a-uuid:1:10\n\
             dddddddd:80000000:{UUID_A}:1:10\n"
        );
        let entries = CloudClient::parse_index(&body);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, UUID_A);
    }

    #[test]
    fn find_metadata_blob_locates_hash_and_filename() {
        // Per-doc blob index: schema, then `hash:type:filename:subfiles:size`.
        let blob = format!(
            "3\n\
             eeeeeeee:0:{UUID_A}.content:0:50\n\
             ffffffff:0:{UUID_A}.metadata:0:120\n\
             99999999:0:{UUID_A}.pagedata:0:5\n"
        );
        let found = CloudClient::find_metadata_blob(&blob);
        assert_eq!(
            found,
            Some(("ffffffff".to_string(), format!("{UUID_A}.metadata")))
        );
    }

    #[test]
    fn find_metadata_blob_returns_none_when_absent() {
        let blob = format!("3\neeeeeeee:0:{UUID_A}.content:0:50\n");
        assert_eq!(CloudClient::find_metadata_blob(&blob), None);
    }

    #[test]
    fn config_from_env_clamps_workers() {
        // Defaults are sane without any env set.
        let cfg = ClientConfig::from_env().unwrap();
        assert!(cfg.parallel_workers >= 1 && cfg.parallel_workers <= 64);
        assert!(cfg.sync_host.starts_with("https://"));
    }

    #[test]
    fn default_device_desc_is_recognized_and_platform_appropriate() {
        // The platform default must be a value reMarkable accepts.
        let d = default_device_desc();
        assert!(KNOWN_DEVICE_DESCS.contains(&d), "unknown default: {d}");
        assert!(d.starts_with("desktop-"));
        #[cfg(target_os = "macos")]
        assert_eq!(d, "desktop-macos");
    }
}
