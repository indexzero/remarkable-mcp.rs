//! The MCP server: tool definitions and request handling over `rmcp`.
//!
//! Read-focused v1 surface, synthesized from the three reference servers:
//!
//! | tool | from | notes |
//! |------|------|-------|
//! | `remarkable_status`  | all      | auth + connectivity + document count |
//! | `remarkable_list`    | lanej    | folder listing, paginated + token-budgeted |
//! | `remarkable_tree`    | lanej    | ASCII tree with depth collapse |
//! | `remarkable_search`  | all      | name search with `did_you_mean` |
//! | `remarkable_recent`  | wavyrai  | most-recently-modified documents |
//! | `remarkable_get`     | lanej    | metadata for one item (path or id) |
//!
//! Write tools (mkdir/move/delete) and content/rendering are deliberately out of
//! v1 scope — see the PR's "Assumptions & scope" section.

use std::sync::Arc;

use remarkable_core::{CloudClient, ItemType, Library};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::config::ServerConfig;
use crate::response::Responder;
use crate::scope::Scope;

const INSTRUCTIONS: &str = "\
Access a reMarkable tablet's cloud library. Browse folders (remarkable_list), view \
the whole tree (remarkable_tree), search by name (remarkable_search), see recent \
documents (remarkable_recent), and inspect one item (remarkable_get). Paths look \
like /Folder/Document. Start with remarkable_status to confirm authentication.";

fn default_limit() -> usize {
    50
}

/// Parameters for `remarkable_list`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListParams {
    /// Folder path to list, e.g. `/Work`. Defaults to the library root.
    #[serde(default)]
    pub path: Option<String>,
    /// Include all nested items, not just immediate children.
    #[serde(default)]
    pub recursive: bool,
    /// Maximum items to return (default 50).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Number of items to skip, for pagination (default 0).
    #[serde(default)]
    pub offset: usize,
}

/// Parameters for `remarkable_tree`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TreeParams {
    /// Folder path to root the tree at. Defaults to the library root.
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum depth to render; 0 (default) means unlimited.
    #[serde(default)]
    pub depth: usize,
}

/// Parameters for `remarkable_search`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Case-insensitive substring to match against item names.
    pub query: String,
    /// Restrict to `document` or `folder`. Omit for both.
    #[serde(default)]
    pub kind: Option<String>,
    /// Maximum results to return (default 50).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Number of results to skip, for pagination (default 0).
    #[serde(default)]
    pub offset: usize,
}

/// Parameters for `remarkable_recent`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecentParams {
    /// Number of recent documents to return (default 10).
    #[serde(default = "default_recent")]
    pub limit: usize,
}

fn default_recent() -> usize {
    10
}

/// Parameters for `remarkable_get`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetParams {
    /// A document/folder path (`/Work/Notes`) or a raw UUID.
    pub target: String,
}

/// The reMarkable MCP server handler.
#[derive(Clone)]
pub struct RemarkableServer {
    client: Arc<CloudClient>,
    cfg: ServerConfig,
    scope: Scope,
    // Read by the `#[tool_handler]`-generated dispatch; dead-code analysis misses it.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl RemarkableServer {
    /// Build the server around a cloud client and resolved configuration.
    pub fn new(client: Arc<CloudClient>, cfg: ServerConfig) -> Self {
        let scope = Scope::new(cfg.root_path.clone());
        RemarkableServer {
            client,
            cfg,
            scope,
            tool_router: Self::tool_router(),
        }
    }

    fn responder(&self) -> Responder {
        Responder::new(&self.cfg)
    }

    fn text(&self, body: String) -> CallToolResult {
        CallToolResult::success(vec![Content::text(body)])
    }

    /// Load the library, mapping auth/transport failures to a structured error
    /// result the model can act on.
    async fn load_library(&self) -> std::result::Result<Library, CallToolResult> {
        match self.client.list_items().await {
            Ok(items) => Ok(Library::new(items)),
            Err(remarkable_core::Error::NotAuthenticated) => {
                Err(self.text(self.responder().error(
                    "not_authenticated",
                    "no reMarkable device token is stored",
                    "run `remarkable auth <one-time-code>` (get a code at \
                 https://my.remarkable.com/device/desktop/connect)",
                    vec![],
                )))
            }
            Err(e) => Err(self.text(self.responder().error(
                "api_error",
                e.to_string(),
                "verify network connectivity and that your token is still valid",
                vec![],
            ))),
        }
    }

    /// Serialize an item to the JSON shape used in tool responses (paths shown
    /// relative to the configured scope).
    fn item_json(&self, item: &remarkable_core::Item, path: &str) -> serde_json::Value {
        json!({
            "id": item.id,
            "name": item.name,
            "type": if item.is_folder() { "folder" } else { "document" },
            "path": self.scope.display(path),
            "parent": item.parent,
            "pinned": item.pinned,
            "modified": item.last_modified,
        })
    }
}

#[tool_router]
impl RemarkableServer {
    /// Authentication status, active configuration, and document count.
    #[tool(
        description = "Check reMarkable authentication status, configuration, and \
                       how many documents are accessible. Call this first."
    )]
    async fn remarkable_status(&self) -> Result<CallToolResult, McpError> {
        let status = self.client.auth_status().await;
        let mut payload = json!({
            "authenticated": status.authenticated,
            "device_id": status.device_id,
            "has_valid_user_token": status.has_valid_user_token,
            "user_token_expires": status.user_token_expires,
            "token_path": status.token_path,
            "transport": "cloud",
            "write_enabled": false,
            "scope": self.scope.root().unwrap_or("/"),
        });

        if status.authenticated {
            match self.client.list_items().await {
                Ok(items) => {
                    let count = if self.scope.is_unscoped() {
                        items.len()
                    } else {
                        let lib = Library::new(items.clone());
                        let paths = lib.path_map();
                        items
                            .iter()
                            .filter(|it| {
                                paths
                                    .get(&it.id)
                                    .map(|p| self.scope.contains(p))
                                    .unwrap_or(false)
                            })
                            .count()
                    };
                    payload["status"] = json!("connected");
                    payload["document_count"] = json!(count);
                }
                Err(e) => {
                    payload["status"] = json!(format!("error: {e}"));
                }
            }
        } else {
            payload["status"] = json!("unauthenticated");
        }

        Ok(self.text(self.responder().ok(
            payload,
            "if unauthenticated, run `remarkable auth <code>`; otherwise try \
             remarkable_list or remarkable_tree",
        )))
    }

    /// List the contents of a folder.
    #[tool(
        description = "List documents and folders in a path (default root). Supports \
                       recursive listing and limit/offset pagination."
    )]
    async fn remarkable_list(
        &self,
        Parameters(p): Parameters<ListParams>,
    ) -> Result<CallToolResult, McpError> {
        let lib = match self.load_library().await {
            Ok(l) => l,
            Err(result) => return Ok(result),
        };
        let user_path = p.path.unwrap_or_else(|| "/".to_string());
        let device_path = self.scope.resolve(&user_path);

        let items = match lib.list_folder(&device_path, p.recursive) {
            Ok(items) => items,
            Err(_) => {
                let suggestions = lib.did_you_mean(&user_path, 3);
                return Ok(self.text(self.responder().error(
                    "not_found",
                    format!("folder not found: {user_path}"),
                    "check the path with remarkable_tree, or list the root with path=/",
                    suggestions,
                )));
            }
        };

        let paths = lib.path_map();
        let total = items.len();
        let page: Vec<serde_json::Value> = items
            .iter()
            .skip(p.offset)
            .take(p.limit)
            .map(|it| {
                let path = paths.get(&it.id).cloned().unwrap_or_default();
                self.item_json(it, &path)
            })
            .collect();

        let returned = page.len();
        let has_more = p.offset + returned < total;
        let payload = json!({
            "path": user_path,
            "items": page,
            "total": total,
            "offset": p.offset,
            "limit": p.limit,
            "has_more": has_more,
        });
        Ok(self.text(self.responder().paginated(
            payload,
            total,
            p.offset,
            p.limit,
            if has_more {
                format!(
                    "more items available; call again with offset={}",
                    p.offset + returned
                )
            } else {
                "use remarkable_get for details on any item".to_string()
            },
        )))
    }

    /// Render the library as an ASCII tree.
    #[tool(
        description = "Render the document tree as ASCII art (📁 folders, 📄 documents). \
                       Optionally root it at a path and/or limit the depth."
    )]
    async fn remarkable_tree(
        &self,
        Parameters(p): Parameters<TreeParams>,
    ) -> Result<CallToolResult, McpError> {
        let lib = match self.load_library().await {
            Ok(l) => l,
            Err(result) => return Ok(result),
        };
        let user_path = p.path.clone().unwrap_or_else(|| "/".to_string());
        let device_path = self.scope.resolve(&user_path);
        let start = if device_path == "/" {
            None
        } else {
            Some(device_path.as_str())
        };

        match lib.render_tree(start, p.depth) {
            Ok(tree) => {
                let payload = json!({
                    "path": user_path,
                    "depth": p.depth,
                    "tree": tree,
                });
                Ok(self.text(self.responder().paginated(
                    payload,
                    lib.len(),
                    0,
                    lib.len(),
                    "pass depth=N to collapse, or path=/Folder to focus a subtree",
                )))
            }
            Err(_) => {
                let suggestions = lib.did_you_mean(&user_path, 3);
                Ok(self.text(self.responder().error(
                    "not_found",
                    format!("path not found: {user_path}"),
                    "render the full tree with no path argument first",
                    suggestions,
                )))
            }
        }
    }

    /// Search documents and folders by name.
    #[tool(
        description = "Search documents and folders by case-insensitive name match. \
                       Optionally filter by kind (document|folder)."
    )]
    async fn remarkable_search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let lib = match self.load_library().await {
            Ok(l) => l,
            Err(result) => return Ok(result),
        };
        let filter = match p.kind.as_deref() {
            Some("document") | Some("documents") => Some(ItemType::Document),
            Some("folder") | Some("folders") => Some(ItemType::Folder),
            _ => None,
        };

        let mut results: Vec<(&remarkable_core::Item, String)> = lib
            .search(&p.query, filter)
            .into_iter()
            .filter(|(_, path)| self.scope.contains(path))
            .collect();
        let total = results.len();
        results.truncate(p.offset + p.limit);
        let page: Vec<serde_json::Value> = results
            .iter()
            .skip(p.offset)
            .map(|(it, path)| self.item_json(it, path))
            .collect();

        if total == 0 {
            let suggestions = lib.did_you_mean(&p.query, 5);
            return Ok(self.text(self.responder().error(
                "no_results",
                format!("no items match \"{}\"", p.query),
                "try a shorter or different query",
                suggestions,
            )));
        }

        let returned = page.len();
        let has_more = p.offset + returned < total;
        let payload = json!({
            "query": p.query,
            "results": page,
            "total": total,
            "offset": p.offset,
            "limit": p.limit,
            "has_more": has_more,
        });
        Ok(self.text(self.responder().paginated(
            payload,
            total,
            p.offset,
            p.limit,
            "open a result with remarkable_get",
        )))
    }

    /// List the most recently modified documents.
    #[tool(description = "List the most recently modified documents (newest first).")]
    async fn remarkable_recent(
        &self,
        Parameters(p): Parameters<RecentParams>,
    ) -> Result<CallToolResult, McpError> {
        let lib = match self.load_library().await {
            Ok(l) => l,
            Err(result) => return Ok(result),
        };
        let paths = lib.path_map();
        let limit = p.limit.clamp(1, 50);
        let docs: Vec<serde_json::Value> = lib
            .recent(limit * 4) // over-fetch, then scope-filter
            .into_iter()
            .filter(|it| {
                paths
                    .get(&it.id)
                    .map(|p| self.scope.contains(p))
                    .unwrap_or(false)
            })
            .take(limit)
            .map(|it| {
                let path = paths.get(&it.id).cloned().unwrap_or_default();
                self.item_json(it, &path)
            })
            .collect();
        let payload = json!({
            "count": docs.len(),
            "documents": docs,
        });
        Ok(self.text(
            self.responder()
                .ok(payload, "read a document's details with remarkable_get"),
        ))
    }

    /// Get metadata for one item by path or id.
    #[tool(
        description = "Get metadata for a single document or folder, addressed by \
                       path (/Work/Notes) or UUID."
    )]
    async fn remarkable_get(
        &self,
        Parameters(p): Parameters<GetParams>,
    ) -> Result<CallToolResult, McpError> {
        let lib = match self.load_library().await {
            Ok(l) => l,
            Err(result) => return Ok(result),
        };
        // Resolve scoped path inputs; UUIDs pass through unchanged.
        let needle = if uuid::is_uuid(&p.target) {
            p.target.clone()
        } else {
            self.scope.resolve(&p.target)
        };

        match lib.by_path_or_id(&needle) {
            Some(item) => {
                let paths = lib.path_map();
                let path = paths.get(&item.id).cloned().unwrap_or_default();
                if !self.scope.contains(&path) {
                    return Ok(self.text(self.responder().error(
                        "not_found",
                        format!("item is outside the configured scope: {}", p.target),
                        "remove REMARKABLE_ROOT_PATH or target an item within scope",
                        vec![],
                    )));
                }
                let payload = self.item_json(item, &path);
                Ok(self.text(self.responder().ok(
                    payload,
                    if item.is_folder() {
                        "list its contents with remarkable_list"
                    } else {
                        "this is a document; content reading is not yet supported in v1"
                    },
                )))
            }
            None => {
                let suggestions = lib.did_you_mean(&p.target, 5);
                Ok(self.text(self.responder().error(
                    "not_found",
                    format!("no item at: {}", p.target),
                    "browse with remarkable_tree or remarkable_list to find the exact path",
                    suggestions,
                )))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for RemarkableServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        let mut imp = Implementation::default();
        imp.name = "remarkable-mcp".to_string();
        imp.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = imp;
        info.instructions = Some(INSTRUCTIONS.to_string());
        info
    }
}

/// Tiny UUID check used to decide path-vs-id without pulling `uuid` into the bin's
/// public surface.
mod uuid {
    pub fn is_uuid(s: &str) -> bool {
        let s = s.trim();
        // 8-4-4-4-12 hex.
        let parts: Vec<&str> = s.split('-').collect();
        parts.len() == 5
            && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(len, part)| {
                part.len() == *len && part.chars().all(|c| c.is_ascii_hexdigit())
            })
    }
}
