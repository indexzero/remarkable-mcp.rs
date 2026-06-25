//! Error types for the reMarkable core library.
//!
//! Per the rust-cli-aspects ARCHITECTURE guidance (lib uses `thiserror`, bin uses
//! `anyhow`), this library exposes a typed error enum so callers can match on
//! failure modes (notably [`Error::NotAuthenticated`] and [`Error::NotFound`],
//! which the MCP layer turns into actionable hints).

use std::path::PathBuf;

/// Result alias used throughout the core library.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by the reMarkable cloud client and document model.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No device token is stored — the user must run `remarkable auth <code>` first.
    #[error("not authenticated: no device token found (run `remarkable auth <code>`)")]
    NotAuthenticated,

    /// Device registration with the reMarkable cloud failed.
    #[error("device registration failed (HTTP {status}): {body}")]
    Registration { status: u16, body: String },

    /// A reMarkable cloud API request returned a non-success status.
    #[error("reMarkable API error at {endpoint} (HTTP {status}): {body}")]
    Api {
        endpoint: String,
        status: u16,
        body: String,
    },

    /// A document or folder path / id could not be resolved.
    #[error("not found: {0}")]
    NotFound(String),

    /// The requested operation is not supported by the active configuration.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The sync index or metadata payload could not be parsed.
    #[error("malformed sync data: {0}")]
    Malformed(String),

    /// Reading or writing the token store on disk failed.
    #[error("token store I/O error at {path}: {source}")]
    TokenStore {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The home / state directory could not be resolved for token storage.
    #[error("could not determine a state directory for token storage")]
    NoStateDir,

    /// An underlying HTTP transport error (DNS, TLS, timeout, connection).
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// A JSON (de)serialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A generic I/O error not tied to the token store.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
