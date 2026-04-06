use std::sync::Arc;

use pitlane_mcp::embed::EmbedConfig;
use pitlane_mcp::tools;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Meta, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, Peer, RoleServer, ServerHandler,
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
    /// Maximum number of source files to index (default: 100 000). Raise for very large
    /// mono-repos. Set to 0 to use the default.
    pub max_files: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchSymbolsRequest {
    /// Project path previously indexed
    pub project: String,
    /// Substring or prefix match against symbol name or qualified name
    pub query: String,
    /// Filter by SymbolKind (e.g. "method", "trait")
    pub kind: Option<String>,
    /// Filter by language ("rust", "python", "javascript", "typescript", "c", "cpp", "go", "java", "bash", "csharp")
    pub language: Option<String>,
    /// Glob pattern to restrict search to specific files
    pub file: Option<String>,
    /// Maximum results to return (default: 20)
    pub limit: Option<usize>,
    /// Offset into results for pagination (default: 0)
    pub offset: Option<usize>,
    /// Search mode: "bm25" (default, BM25 ranked full-text over name/qualified/signature/doc), "exact" (substring on name/qualified), "fuzzy" (trigram similarity ranking), "semantic" (vector similarity search — requires PITLANE_EMBED_URL and PITLANE_EMBED_MODEL to be set and index_project to have been run with embeddings enabled)
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetSymbolRequest {
    /// Project path
    pub project: String,
    /// Stable symbol ID from search_symbols or get_file_outline
    pub symbol_id: String,
    /// Also return up to 3 lines before/after (default: false)
    pub include_context: Option<bool>,
    /// Return only the signature and docstring, skipping the full body (default: true for struct/class/interface/trait, false otherwise)
    pub signature_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetFileOutlineRequest {
    /// Project path
    pub project: String,
    /// Path to the file, relative to project root
    pub file_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetLinesRequest {
    /// Project path (used to resolve relative file paths)
    pub project: String,
    /// File path, relative to project root or absolute
    pub file_path: String,
    /// First line to return, 1-indexed inclusive
    pub line_start: u32,
    /// Last line to return, 1-indexed inclusive (capped at 500 lines per call)
    pub line_end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetProjectOutlineRequest {
    /// Project path
    pub project: String,
    /// Directory depth to show (default: 2)
    pub depth: Option<u32>,
    /// Only include files under this directory prefix (relative to project root), e.g. "kernel/sched"
    pub path: Option<String>,
    /// Maximum directory entries to return (default: 50, max: 500). Use with 'path' to drill into large codebases.
    pub max_dirs: Option<usize>,
    /// When true, return only directory names with file and symbol counts — no per-file items or kind breakdowns. Use for very large codebases (>10k files) where the full outline exceeds token limits.
    pub summary: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FindUsagesRequest {
    /// Project path
    pub project: String,
    /// Symbol to find usages for
    pub symbol_id: String,
    /// Restrict search to a file or directory glob
    pub scope: Option<String>,
    /// Maximum usages to return (default: 100)
    pub limit: Option<usize>,
    /// Offset into usages for pagination (default: 0)
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchProjectRequest {
    /// Project path to watch
    pub project: String,
    /// Pass true to stop an existing watcher (default: false)
    pub stop: Option<bool>,
    /// Pass true to query watcher status without starting or stopping (default: false)
    pub status_only: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetIndexStatsRequest {
    /// Project path previously indexed
    pub project: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetUsageStatsRequest {
    /// Filter to a single project path (default: return all projects + global total)
    pub project: Option<String>,
}

#[derive(Clone)]
pub struct PitlaneMcp {
    watcher_registry: Arc<WatcherRegistry>,
    embed_config: Option<Arc<EmbedConfig>>,
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
        let embed_config = EmbedConfig::from_env().map(Arc::new);
        Self {
            watcher_registry,
            embed_config,
            tool_router: Self::tool_router(),
        }
    }
}

/// Build the `_meta` object attached to each tool definition.
///
/// `alwaysLoad` is a vendor hint (used by some MCP hosts) that the tool should
/// always be included in the active toolset without explicit opt-in.
/// `searchHint` provides keywords the host can use for tool discovery matching.
fn tool_meta(search_hint: &'static str) -> Meta {
    let mut meta = Meta::new();
    meta.insert("alwaysLoad".to_string(), serde_json::Value::Bool(true));
    meta.insert(
        "searchHint".to_string(),
        serde_json::Value::String(search_hint.to_string()),
    );
    meta
}

fn value_to_text(value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

fn err_to_text(e: anyhow::Error) -> String {
    // If the error is (or wraps) a ToolError, emit structured JSON.
    if let Some(tool_err) = e.downcast_ref::<pitlane_mcp::error::ToolError>() {
        return serde_json::to_string_pretty(&tool_err.to_json())
            .unwrap_or_else(|_| tool_err.to_json().to_string());
    }
    // Fallback: wrap in a generic INTERNAL_ERROR envelope.
    let fallback = serde_json::json!({
        "error": {
            "code": "INTERNAL_ERROR",
            "message": e.to_string(),
            "hint": "Check the project path and try again.",
        }
    });
    serde_json::to_string_pretty(&fallback).unwrap_or_else(|_| fallback.to_string())
}

#[tool_router]
impl PitlaneMcp {
    #[tool(
        description = "Call first to parse and index a project's source files; subsequent calls are fast (cached). Returns symbol count, file count, and elapsed time. Also generates vector embeddings for semantic search (mode=\"semantic\") when PITLANE_EMBED_URL and PITLANE_EMBED_MODEL are configured — no separate step needed.",
        meta = tool_meta("index parse cache project"),
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn index_project(
        &self,
        Parameters(req): Parameters<IndexProjectRequest>,
        peer: Peer<RoleServer>,
        meta: rmcp::model::Meta,
    ) -> String {
        let params = tools::index_project::IndexProjectParams {
            path: req.path,
            exclude: req.exclude,
            force: req.force,
            max_files: req.max_files,
            progress_token: meta.get_progress_token(),
            peer: Some(peer),
            embed_config: self.embed_config.clone(),
        };
        match tools::index_project::index_project(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Search indexed symbols by name or concept. Mode selection: use mode=\"semantic\" when you know what a symbol does but not its name (describe intent, e.g. \"retry logic for failed HTTP requests\"); use mode=\"bm25\" (default) when you know the name or a distinctive keyword. Filter by kind, language, or file glob to narrow results. Returns matching symbols with IDs, kinds, and locations.",
        meta = tool_meta("search find symbol function method class type semantic"),
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
            offset: req.offset,
            mode: req.mode,
            embed_config: self.embed_config.clone(),
        };
        match tools::search_symbols::search_symbols(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Fetch the source of one symbol by its stable ID — more token-efficient than reading the whole file. Full-source responses include a references field listing symbols directly used by this symbol (calls, type references); no separate find_usages call needed to understand dependencies. Structs/classes/interfaces/traits default to signature-only (no body); pass signature_only=false to get full source and references.",
        meta = tool_meta("symbol implementation source code definition"),
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
        meta = tool_meta("file outline structure symbols"),
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
        description = "Fetch a slice of a file by line range — use when you need a specific block that isn't a named symbol (e.g. a macro invocation, initializer table, or inline comment block). Returns source lines with total_file_lines so you can paginate. Capped at 500 lines per call; response includes truncated and next offset when the cap is hit.",
        meta = tool_meta("lines file slice range source"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_lines(&self, Parameters(req): Parameters<GetLinesRequest>) -> String {
        let params = tools::get_lines::GetLinesParams {
            project: req.project,
            file_path: req.file_path,
            line_start: req.line_start,
            line_end: req.line_end,
        };
        match tools::get_lines::get_lines(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Orient yourself in a codebase: files grouped by directory with symbol counts per kind. For very large projects (>10k files), if the result exceeds token limits, retry with summary=true to get a lightweight directory-only view (just dir names with file/symbol counts). Use 'path' to drill into a specific subtree for full detail.",
        meta = tool_meta("project overview codebase directory structure"),
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
            path: req.path,
            max_dirs: req.max_dirs,
            summary: req.summary,
        };
        match tools::get_project_outline::get_project_outline(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Find all call sites for a symbol before refactoring. Returns file, line, column, and surrounding snippet for each match.",
        meta = tool_meta("usages references callers refactor"),
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
            limit: req.limit,
            offset: req.offset,
        };
        match tools::find_usages::find_usages(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Call after index_project to keep the index current as files change. Pass stop=true to stop the watcher. Pass status_only=true to check whether a watcher is already running without starting or stopping it.",
        meta = tool_meta("watch monitor file changes live"),
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
            status_only: req.status_only,
            embed_config: self.embed_config.clone(),
        };
        match tools::watch_project::watch_project(params, &self.watcher_registry).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Return symbol counts by language and kind for an indexed project — lightweight orientation tool. Use instead of get_project_outline when you only need aggregate numbers, not the file tree.",
        meta = tool_meta("stats symbols language kind count index"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_index_stats(&self, Parameters(req): Parameters<GetIndexStatsRequest>) -> String {
        let params = tools::get_index_stats::GetIndexStatsParams {
            project: req.project,
        };
        match tools::get_index_stats::get_index_stats(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Return token-efficiency statistics for get_symbol calls — how many tokens were saved by signature-only responses. Pass project to filter to one repo; omit for global totals across all projects.",
        meta = tool_meta("tokens saved statistics usage efficiency"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_usage_stats(&self, Parameters(req): Parameters<GetUsageStatsRequest>) -> String {
        let params = tools::get_usage_stats::GetUsageStatsParams {
            project: req.project,
        };
        match tools::get_usage_stats::get_usage_stats(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }
}

#[tool_handler]
impl ServerHandler for PitlaneMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "pitlane-mcp: AST-based code intelligence. \
                ALWAYS call index_project first — all other tools require an up-to-date index. \
                Discovery: search_symbols (find by name), get_file_outline (file structure), get_project_outline (repo overview). \
                Retrieval: get_symbol (fetch one implementation by ID). \
                Analysis: find_usages (all call sites for a symbol). \
                Maintenance: watch_project (keep index current as files change).",
            )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_env("RUST_LOG")
                .add_directive("pitlane_mcp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let server = PitlaneMcp::new();
    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let running = rmcp::serve_server(server, transport).await?;
    running.waiting().await?;
    Ok(())
}
