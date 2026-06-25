//! Server-side configuration, resolved from the environment.
//!
//! Precedence is env-var → built-in default (CONFIG-AND-STATE). The token/sync
//! settings live in `remarkable-core`'s [`remarkable_core::ClientConfig`]; this
//! struct holds only the MCP presentation knobs.

/// Presentation and safety configuration for the MCP server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Omit `_hint` fields to save tokens (`REMARKABLE_COMPACT`). From wavyrai.
    pub compact: bool,
    /// Soft response budget in estimated tokens (`REMARKABLE_MAX_RESPONSE_TOKENS`,
    /// default 4000). Oversized listings return a pagination hint. From lanej.
    pub max_response_tokens: usize,
    /// Hard character cap on any single response (`REMARKABLE_MAX_OUTPUT_CHARS`,
    /// default 50000). From wavyrai.
    pub max_output_chars: usize,
    /// Optional folder scope (`REMARKABLE_ROOT_PATH`): when set, tools operate
    /// relative to this folder. From SamMorrowDrums. Empty means whole library.
    pub root_path: Option<String>,
}

impl ServerConfig {
    /// Resolve configuration from the environment.
    pub fn from_env() -> Self {
        ServerConfig {
            compact: env_flag("REMARKABLE_COMPACT"),
            max_response_tokens: env_usize("REMARKABLE_MAX_RESPONSE_TOKENS", 4000).max(500),
            max_output_chars: env_usize("REMARKABLE_MAX_OUTPUT_CHARS", 50_000).max(1000),
            root_path: std::env::var("REMARKABLE_ROOT_PATH")
                .ok()
                .filter(|s| !s.is_empty() && s != "/"),
        }
    }
}

fn env_flag(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}
