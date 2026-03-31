use std::sync::Arc;

use pitlane_mcp::tools;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde::{Deserialize, Serialize};

use pitlane_mcp::tools::watch_project::WatcherRegistry;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IndexProjectRequest {
    /// Absolute or relative path to the project root
    pub path: String,
    /// Glob patterns to exclude (default: target/, .git/, __pycache__/)
    pub exclude: Option<Vec<String>>,
    /// Re-index even if an up-to-date index exists
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchSymbolsRequest {
    /// Project path previously indexed
    pub project: String,
    /// Substring or prefix match against symbol name or qualified name
    pub query: String,
    /// Filter by SymbolKind (e.g. "method", "trait")
    pub kind: Option<String>,
    /// Filter by language ("rust", "python", "javascript", "typescript", "c", "cpp", "go", "java")
    pub language: Option<String>,
    /// Glob pattern to restrict search to specific files
    pub file: Option<String>,
    /// Maximum results to return (default: 20)
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetSymbolRequest {
    /// Project path
    pub project: String,
    /// Stable symbol ID from search_symbols or get_file_outline
    pub symbol_id: String,
    /// Also return up to 3 lines before/after (default: false)
    pub include_context: Option<bool>,
    /// Return only the signature and docstring, skipping the full body (default: false)
    pub signature_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetFileOutlineRequest {
    /// Project path
    pub project: String,
    /// Path to the file, relative to project root
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetProjectOutlineRequest {
    /// Project path
    pub project: String,
    /// Directory depth to show (default: 2)
    pub depth: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FindUsagesRequest {
    /// Project path
    pub project: String,
    /// Symbol to find usages for
    pub symbol_id: String,
    /// Restrict search to a file or directory glob
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchProjectRequest {
    /// Project path to watch
    pub project: String,
    /// Pass true to stop an existing watcher (default: false)
    pub stop: Option<bool>,
}

#[derive(Clone)]
pub struct PitlaneMcp {
    watcher_registry: Arc<WatcherRegistry>,
    tool_router: ToolRouter<Self>,
}

impl Default for PitlaneMcp {
    fn default() -> Self {
        Self::new()
    }
}

impl PitlaneMcp {
    pub fn new() -> Self {
        let watcher_registry = Arc::new(WatcherRegistry::new());
        Self {
            watcher_registry,
            tool_router: Self::tool_router(),
        }
    }
}

fn value_to_text(value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

fn err_to_text(e: anyhow::Error) -> String {
    format!("Error: {}", e)
}

#[tool_router]
impl PitlaneMcp {
    #[tool(
        description = "Call first to parse and index a project's source files; subsequent calls are fast (cached). Returns symbol count, file count, and elapsed time.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn index_project(&self, Parameters(req): Parameters<IndexProjectRequest>) -> String {
        let params = tools::index_project::IndexProjectParams {
            path: req.path,
            exclude: req.exclude,
            force: req.force,
        };
        match tools::index_project::index_project(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Search indexed symbols by name; filter by kind, language, or file glob to narrow results. Returns matching symbols with IDs, kinds, and locations.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search_symbols(&self, Parameters(req): Parameters<SearchSymbolsRequest>) -> String {
        let params = tools::search_symbols::SearchSymbolsParams {
            project: req.project,
            query: req.query,
            kind: req.kind,
            language: req.language,
            file: req.file,
            limit: req.limit,
        };
        match tools::search_symbols::search_symbols(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Fetch the source of one symbol by its stable ID — more token-efficient than reading the whole file.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_symbol(&self, Parameters(req): Parameters<GetSymbolRequest>) -> String {
        let params = tools::get_symbol::GetSymbolParams {
            project: req.project,
            symbol_id: req.symbol_id,
            include_context: req.include_context,
            signature_only: req.signature_only,
        };
        match tools::get_symbol::get_symbol(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Explore a file's structure: lists all symbols with kinds and line numbers, without returning source code.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_file_outline(&self, Parameters(req): Parameters<GetFileOutlineRequest>) -> String {
        let params = tools::get_file_outline::GetFileOutlineParams {
            project: req.project,
            file_path: req.file_path,
        };
        match tools::get_file_outline::get_file_outline(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Orient yourself in a codebase: files grouped by directory with symbol counts per kind.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_project_outline(
        &self,
        Parameters(req): Parameters<GetProjectOutlineRequest>,
    ) -> String {
        let params = tools::get_project_outline::GetProjectOutlineParams {
            project: req.project,
            depth: req.depth,
        };
        match tools::get_project_outline::get_project_outline(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Find all call sites for a symbol before refactoring. Returns file, line, column, and surrounding snippet for each match.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn find_usages(&self, Parameters(req): Parameters<FindUsagesRequest>) -> String {
        let params = tools::find_usages::FindUsagesParams {
            project: req.project,
            symbol_id: req.symbol_id,
            scope: req.scope,
        };
        match tools::find_usages::find_usages(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Call after index_project to keep the index current as files change. Pass stop=true to stop the watcher.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn watch_project(&self, Parameters(req): Parameters<WatchProjectRequest>) -> String {
        let params = tools::watch_project::WatchProjectParams {
            project: req.project,
            stop: req.stop,
        };
        match tools::watch_project::watch_project(params, &self.watcher_registry).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }
}

#[tool_handler]
impl ServerHandler for PitlaneMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("pitlane-mcp: Token-efficient code intelligence using tree-sitter AST parsing. Use index_project first, then search_symbols, get_symbol, get_file_outline, get_project_outline, find_usages, and watch_project.")
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = PitlaneMcp::new();
    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let running = rmcp::serve_server(server, transport).await?;
    running.waiting().await?;
    Ok(())
}
