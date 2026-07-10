//! Token storage and the authentication state machine.
//!
//! The reMarkable cloud uses a two-token scheme, consistent across all three
//! reference implementations:
//!
//! * a **device token** — minted once from a one-time pairing code, effectively
//!   permanent, and
//! * a **user token** — a short-lived (~24h) bearer token exchanged from the
//!   device token before each sync session.
//!
//! ## Storage decision (documented assumption)
//!
//! rust-cli-aspects CONFIG-AND-STATE forbids plaintext secrets in *config* files
//! and prefers the OS keyring. The de-facto reMarkable convention (rmapi, and all
//! three reference servers) is a `0600` token file. We follow that convention for
//! v1 — a single `0600` file under the XDG **state** dir — and wrap tokens in
//! [`SecretToken`] so they never leak into logs/`Debug`. Keyring storage is a
//! tracked enhancement, not a v1 requirement.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Schema version for the on-disk token store (CONFIG-AND-STATE §6).
const SCHEMA_VERSION: u32 = 1;

/// A secret string that never reveals itself via `Debug` / `Display`.
///
/// Serializes transparently (so it can persist to the token file) but redacts in
/// any diagnostic output, satisfying the "never log secrets" discipline without
/// taking on the `secrecy` crate's churn.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretToken(String);

impl SecretToken {
    /// Wrap a raw token value.
    pub fn new(value: impl Into<String>) -> Self {
        SecretToken(value.into())
    }
    /// Borrow the underlying token. Use only where the value is actually needed
    /// (e.g. building an `Authorization` header).
    pub fn expose(&self) -> &str {
        &self.0
    }
    /// `true` if no token value is present.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            f.write_str("SecretToken(<empty>)")
        } else {
            f.write_str("SecretToken(***)")
        }
    }
}

/// Persistent authentication state for the reMarkable cloud.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStore {
    /// On-disk schema version, refused if unrecognized on load.
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    /// Permanent device token (minted from a pairing code).
    #[serde(default)]
    pub device_token: SecretToken,
    /// The UUID generated for this device at registration time.
    #[serde(default)]
    pub device_id: String,
    /// The device-kind descriptor this device registered as (for display; empty on
    /// stores written before this field existed).
    #[serde(default)]
    pub device_desc: String,
    /// Short-lived user token used as the sync bearer.
    #[serde(default)]
    pub user_token: SecretToken,
    /// RFC3339 expiry for `user_token`.
    #[serde(default)]
    pub user_token_expires: Option<chrono::DateTime<chrono::Utc>>,
}

fn default_schema() -> u32 {
    SCHEMA_VERSION
}

impl Default for TokenStore {
    fn default() -> Self {
        TokenStore {
            schema_version: SCHEMA_VERSION,
            device_token: SecretToken::default(),
            device_id: String::new(),
            device_desc: String::new(),
            user_token: SecretToken::default(),
            user_token_expires: None,
        }
    }
}

impl TokenStore {
    /// `true` once a device token is present.
    pub fn is_registered(&self) -> bool {
        !self.device_token.is_empty()
    }

    /// `true` if a non-expired user token is cached. A 60-second skew guards
    /// against using a token that will expire mid-request.
    pub fn has_valid_user_token(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        if self.user_token.is_empty() {
            return false;
        }
        match self.user_token_expires {
            Some(exp) => now + chrono::Duration::seconds(60) < exp,
            None => false,
        }
    }

    /// Load a token store from `path`, returning an empty (unregistered) store if
    /// the file does not exist. Refuses to load an unrecognized schema version.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let store: TokenStore = serde_json::from_slice(&bytes)?;
                if store.schema_version > SCHEMA_VERSION {
                    return Err(Error::Malformed(format!(
                        "token store schema_version {} is newer than supported {}",
                        store.schema_version, SCHEMA_VERSION
                    )));
                }
                Ok(store)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TokenStore::default()),
            Err(source) => Err(Error::TokenStore {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Atomically persist the token store to `path` with `0600` permissions.
    ///
    /// Writes to a sibling temp file, fsyncs, then renames into place — the
    /// crash-safety discipline from CONFIG-AND-STATE §5.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().ok_or(Error::NoStateDir)?;
        fs::create_dir_all(dir).map_err(|source| Error::TokenStore {
            path: dir.to_path_buf(),
            source,
        })?;

        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| Error::TokenStore {
            path: dir.to_path_buf(),
            source,
        })?;
        serde_json::to_writer_pretty(&mut tmp, self)?;
        use std::io::Write;
        tmp.flush()?;
        tmp.as_file().sync_all()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }

        tmp.persist(path).map_err(|e| Error::TokenStore {
            path: path.to_path_buf(),
            source: e.error,
        })?;
        Ok(())
    }
}

/// The default token-store path, honoring the XDG state directory.
///
/// * `$REMARKABLE_TOKEN_PATH` — explicit override (highest precedence)
/// * `$XDG_STATE_HOME/remarkable-mcp/tokens.json`
/// * platform state dir via `directories` (e.g. `~/.local/state/...` on Linux,
///   `~/Library/Application Support/...` on macOS)
pub fn default_token_path() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("REMARKABLE_TOKEN_PATH") {
        if !explicit.is_empty() {
            return Ok(PathBuf::from(explicit));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg)
                .join("remarkable-mcp")
                .join("tokens.json"));
        }
    }
    let dirs = directories::ProjectDirs::from("rs", "indexzero", "remarkable-mcp")
        .ok_or(Error::NoStateDir)?;
    let base = dirs.state_dir().unwrap_or_else(|| dirs.data_dir());
    Ok(base.join("tokens.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_token_redacts_in_debug() {
        let t = SecretToken::new("super-secret-jwt");
        assert_eq!(format!("{t:?}"), "SecretToken(***)");
        assert_eq!(t.expose(), "super-secret-jwt");
        assert_eq!(
            format!("{:?}", SecretToken::default()),
            "SecretToken(<empty>)"
        );
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("tokens.json");
        let mut store = TokenStore::default();
        store.device_token = SecretToken::new("dev");
        store.device_id = "device-123".into();
        store.device_desc = "browser-chrome".into();
        store.user_token = SecretToken::new("usr");
        store.user_token_expires = Some(chrono::Utc::now() + chrono::Duration::hours(1));

        store.save(&path).unwrap();
        let loaded = TokenStore::load(&path).unwrap();
        assert_eq!(loaded.device_token.expose(), "dev");
        assert_eq!(loaded.device_id, "device-123");
        assert_eq!(loaded.device_desc, "browser-chrome");
        assert!(loaded.is_registered());
        assert!(loaded.has_valid_user_token(chrono::Utc::now()));
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        TokenStore::default().save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn missing_file_yields_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::load(&dir.path().join("absent.json")).unwrap();
        assert!(!store.is_registered());
    }

    #[test]
    fn newer_schema_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, r#"{"schema_version": 999}"#).unwrap();
        assert!(matches!(TokenStore::load(&path), Err(Error::Malformed(_))));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn token_validity_respects_expiry_and_skew() {
        let now = chrono::Utc::now();
        let mut store = TokenStore::default();
        store.user_token = SecretToken::new("usr");
        // Expires in 30s — inside the 60s skew guard, so treated as invalid.
        store.user_token_expires = Some(now + chrono::Duration::seconds(30));
        assert!(!store.has_valid_user_token(now));
        // Expires comfortably in the future — valid.
        store.user_token_expires = Some(now + chrono::Duration::hours(2));
        assert!(store.has_valid_user_token(now));
    }
}
