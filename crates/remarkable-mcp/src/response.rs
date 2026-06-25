//! Response shaping: hints, structured errors, and token budgeting.
//!
//! Every tool returns a JSON string. The conventions are the union of the three
//! reference servers:
//!
//! * a trailing `_hint` suggesting the next action (omitted in compact mode),
//! * structured errors `{ _error: { type, message, suggestion, did_you_mean } }`
//!   so a model can recover instead of giving up, and
//! * **token budgeting** — listings estimated to exceed the soft token budget come
//!   back as a `_pagination_needed` hint (with a suggested `limit`) rather than
//!   flooding the context window (lanej's idea).

use serde_json::{json, Value};

use crate::config::ServerConfig;

/// Builds tool response strings according to [`ServerConfig`].
pub struct Responder {
    compact: bool,
    max_response_tokens: usize,
    max_output_chars: usize,
}

impl Responder {
    /// Create a responder from server configuration.
    pub fn new(cfg: &ServerConfig) -> Self {
        Responder {
            compact: cfg.compact,
            max_response_tokens: cfg.max_response_tokens,
            max_output_chars: cfg.max_output_chars,
        }
    }

    /// Rough token estimate: ~4 characters per token (lanej's heuristic).
    fn estimate_tokens(text: &str) -> usize {
        text.len() / 4
    }

    /// Serialize a success payload, attaching `_hint` unless compact, and enforcing
    /// the hard character cap as a last resort.
    pub fn ok(&self, mut payload: Value, hint: impl Into<String>) -> String {
        if !self.compact {
            if let Value::Object(map) = &mut payload {
                map.insert("_hint".into(), Value::String(hint.into()));
            }
        }
        let text = serde_json::to_string_pretty(&payload)
            .unwrap_or_else(|e| format!("{{\"_error\":{{\"message\":\"serialize: {e}\"}}}}"));
        if text.len() > self.max_output_chars {
            // Truncation is a failure of pagination upstream; say so explicitly.
            let note = json!({
                "_error": {
                    "type": "response_too_large",
                    "message": format!(
                        "response exceeded REMARKABLE_MAX_OUTPUT_CHARS ({} > {})",
                        text.len(), self.max_output_chars
                    ),
                    "suggestion": "narrow the query, pass a smaller limit, or browse a subfolder",
                }
            });
            return serde_json::to_string_pretty(&note).unwrap_or_default();
        }
        text
    }

    /// A structured error response.
    pub fn error(
        &self,
        error_type: &str,
        message: impl Into<String>,
        suggestion: impl Into<String>,
        did_you_mean: Vec<String>,
    ) -> String {
        let mut err = json!({
            "type": error_type,
            "message": message.into(),
            "suggestion": suggestion.into(),
        });
        if !did_you_mean.is_empty() {
            err["did_you_mean"] = json!(did_you_mean);
        }
        serde_json::to_string_pretty(&json!({ "_error": err })).unwrap_or_default()
    }

    /// Token-budget guard for paginated listings.
    ///
    /// Renders `payload` and, if its estimated token count exceeds the budget,
    /// returns a `_pagination_needed` response suggesting a smaller `limit` instead
    /// of the oversized body. `total`/`offset` echo back so the caller can page.
    pub fn paginated(
        &self,
        payload: Value,
        total: usize,
        offset: usize,
        limit: usize,
        hint: impl Into<String>,
    ) -> String {
        let rendered = self.ok(payload, hint);
        if Self::estimate_tokens(&rendered) <= self.max_response_tokens {
            return rendered;
        }
        // Suggest a limit that should fit the budget: scale the current limit down
        // by the overage ratio, floor at 1.
        let est = Self::estimate_tokens(&rendered).max(1);
        let suggested = ((limit.max(1) * self.max_response_tokens) / est).max(1);
        let body = json!({
            "_pagination_needed": true,
            "message": format!(
                "result (~{est} tokens) exceeds the {} token budget",
                self.max_response_tokens
            ),
            "total": total,
            "offset": offset,
            "current_limit": limit,
            "suggested_limit": suggested,
            "suggestion": format!(
                "retry with limit={suggested}, then increase offset by {suggested} to page"
            ),
        });
        serde_json::to_string_pretty(&body).unwrap_or_default()
    }
}
