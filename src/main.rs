use std::{future::Future, sync::Arc};

use pitlane_mcp::embed::EmbedConfig;
use pitlane_mcp::tools;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ListToolsResult, Meta, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router, Peer, RoleServer, ServerHandler,
};
use serde::{Deserialize, Serialize};

use pitlane_mcp::tools::watch_project::WatcherRegistry;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnsureProjectReadyRequest {
    /// Absolute or relative path to the project root
    pub path: String,
    /// Glob patterns to exclude (default: target/, .git/, __pycache__/)
    pub exclude: Option<Vec<String>>,
    /// Re-index even if an up-to-date index exists
    pub force: Option<bool>,
    /// Maximum number of source files to index (default: 100 000). Omit this field, or set it to 0, to use the default.
    pub max_files: Option<usize>,
    /// Accepted for compatibility but currently ignored; ensure_project_ready no longer waits for embeddings.
    pub poll_interval_ms: Option<u64>,
    /// Accepted for compatibility but currently ignored; ensure_project_ready no longer waits for embeddings.
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IndexProjectRequest {
    /// Absolute or relative path to the project root
    pub path: String,
    /// Glob patterns to exclude (default: target/, .git/, __pycache__/)
    pub exclude: Option<Vec<String>>,
    /// Re-index even if an up-to-date index exists
    pub force: Option<bool>,
    /// Maximum number of source files to index (default: 100 000). Raise for very large
    /// mono-repos. Omit this field, or set it to 0, to use the default.
    pub max_files: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchSymbolsRequest {
    /// Project path previously indexed
    pub project: String,
    /// Symbol name or intent description. For behavior/path questions, prefer an intent phrase
    /// and mode="semantic". For known names, use mode="exact" or mode="bm25".
    pub query: String,
    /// Filter by SymbolKind (e.g. "method", "trait")
    pub kind: Option<String>,
    /// Filter by language ("rust", "python", "javascript", "typescript", "svelte", "c", "cpp", "go", "java", "bash", "csharp", "solidity")
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
pub struct SearchContentRequest {
    /// Project path previously indexed
    pub project: String,
    /// Literal text or regex pattern to search for inside source files
    pub query: String,
    /// Treat query as a regular expression (default: false)
    pub regex: Option<bool>,
    /// Case-sensitive match (default: false)
    pub case_sensitive: Option<bool>,
    /// Filter by language ("rust", "python", "javascript", "typescript", "svelte", "c", "cpp", "go", "java", "bash", "csharp", "ruby", "swift", "objc", "php", "zig", "kotlin", "lua", "solidity")
    pub language: Option<String>,
    /// Glob pattern to restrict search to specific files
    pub file: Option<String>,
    /// Maximum matches to return (default: 20)
    pub limit: Option<usize>,
    /// Offset into matches for pagination (default: 0)
    pub offset: Option<usize>,
    /// Include up to this many lines before each match (default: 0, max: 5)
    pub before_context: Option<usize>,
    /// Include up to this many lines after each match (default: 0, max: 5)
    pub after_context: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchFilesRequest {
    /// Project path previously indexed
    pub project: String,
    /// File name, path fragment, or glob pattern to search for
    pub query: String,
    /// Search mode: "substring" (default, case-insensitive path/name substring), "exact" (exact file name or exact relative path), "fuzzy" (trigram similarity on file name/path), or "glob" (glob pattern over relative paths)
    pub mode: Option<String>,
    /// Filter by language extension ("rust", "python", "javascript", "typescript", "svelte", "c", "cpp", "go", "java", "bash", "csharp", "ruby", "swift", "objc", "php", "zig", "kotlin", "lua", "solidity")
    pub language: Option<String>,
    /// Glob pattern to restrict the search to a subtree or path set
    pub file: Option<String>,
    /// Maximum matches to return (default: 20)
    pub limit: Option<usize>,
    /// Offset into matches for pagination (default: 0)
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TraceExecutionPathRequest {
    /// Project path previously indexed
    pub project: String,
    /// Behavior or execution-path intent to trace, e.g. "main regex search execution path"
    pub query: String,
    /// Optional source hint for source-to-sink tracing.
    pub source: Option<String>,
    /// Optional sink hint for source-to-sink tracing.
    pub sink: Option<String>,
    /// Filter by language ("rust", "python", "javascript", "typescript", "svelte", "c", "cpp", "go", "java", "bash", "csharp", "ruby", "swift", "objc", "php", "zig", "kotlin", "lua", "solidity")
    pub language: Option<String>,
    /// Glob pattern to restrict tracing to specific files
    pub file: Option<String>,
    /// Maximum important symbols/files to return (default: 6)
    pub max_symbols: Option<usize>,
    /// Maximum call-graph expansion depth from the discovered seeds (default: 2)
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InvestigateRequest {
    /// Project path previously indexed
    pub project: String,
    /// The question to investigate — can be a behavior question, subsystem query, or execution-path question.
    pub query: String,
    /// Filter by language
    pub language: Option<String>,
    /// Restrict investigation to a subtree or file glob
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LocateCodeRequest {
    /// Project path previously indexed
    pub project: String,
    /// Code lookup intent or code-path fragment. The server routes this to the most likely discovery primitive.
    pub query: String,
    /// Optional routing hint such as "symbol", "file", "content", or "project".
    pub intent: Option<String>,
    /// Filter by SymbolKind when looking for a symbol candidate.
    pub kind: Option<String>,
    /// Filter by language ("rust", "python", "javascript", "typescript", "svelte", "c", "cpp", "go", "java", "bash", "csharp", "ruby", "swift", "objc", "php", "zig", "kotlin", "lua", "solidity")
    pub language: Option<String>,
    /// Restrict lookup to a subtree or file glob when relevant.
    pub scope: Option<String>,
    /// Maximum results to return (default: 5)
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReadCodeUnitRequest {
    /// Project path previously indexed
    pub project: String,
    /// Stable symbol ID from locate_code/search_symbols.
    pub symbol_id: Option<String>,
    /// Path to a file when reading file-level structure or line slices.
    pub file_path: Option<String>,
    /// First line to return, 1-indexed inclusive.
    pub line_start: Option<u32>,
    /// Last line to return, 1-indexed inclusive.
    pub line_end: Option<u32>,
    /// Also include nearby lines when reading a symbol body.
    pub include_context: Option<bool>,
    /// Return only the signature and docstring for symbol reads.
    pub signature_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TracePathRequest {
    /// Project path previously indexed
    pub project: String,
    /// Flow or execution-path intent to trace.
    pub query: String,
    /// Optional source symbol or source hint.
    pub source: Option<String>,
    /// Optional sink symbol or sink hint.
    pub sink: Option<String>,
    /// Filter by language ("rust", "python", "javascript", "typescript", "svelte", "c", "cpp", "go", "java", "bash", "csharp", "ruby", "swift", "objc", "php", "zig", "kotlin", "lua", "solidity")
    pub language: Option<String>,
    /// Glob pattern to restrict tracing to specific files
    pub file: Option<String>,
    /// Maximum important symbols/files to return (default: 6)
    pub max_symbols: Option<usize>,
    /// Maximum call-graph expansion depth from the discovered seeds (default: 2)
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnalyzeImpactRequest {
    /// Project path previously indexed
    pub project: String,
    /// Symbol, file, or concept describing the change target.
    pub query: Option<String>,
    /// Stable symbol ID to analyze directly.
    pub symbol_id: Option<String>,
    /// File path to analyze when the change target is file-centric.
    pub file_path: Option<String>,
    /// Restrict caller/usage traversal to a subtree or file glob.
    pub scope: Option<String>,
    /// Maximum traversal depth (default: 2)
    pub depth: Option<usize>,
    /// Maximum impacted symbols/files to return (default: 8)
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NavigateCodeRequest {
    /// Project path previously indexed
    pub project: String,
    /// High-level navigation intent or question.
    pub query: String,
    /// Optional routing hint such as "locate", "read", "trace", or "impact".
    pub intent: Option<String>,
    /// Stable symbol ID when already known.
    pub symbol_id: Option<String>,
    /// File path when already known.
    pub file_path: Option<String>,
    /// First line when reading a slice.
    pub line_start: Option<u32>,
    /// Last line when reading a slice.
    pub line_end: Option<u32>,
    /// Also include nearby lines when reading a symbol body.
    pub include_context: Option<bool>,
    /// Return only the signature and docstring for symbol reads.
    pub signature_only: Option<bool>,
    /// Optional source symbol or source hint for trace requests.
    pub source: Option<String>,
    /// Optional sink symbol or sink hint for trace requests.
    pub sink: Option<String>,
    /// Optional kind filter used when locating code.
    pub kind: Option<String>,
    /// Optional language filter used when locating code or tracing.
    pub language: Option<String>,
    /// Optional subtree or glob scope.
    pub scope: Option<String>,
    /// Maximum results to return for locate/impact operations.
    pub limit: Option<usize>,
    /// Maximum symbols to retain for trace operations.
    pub max_symbols: Option<usize>,
    /// Maximum depth for trace operations.
    pub max_depth: Option<usize>,
    /// Maximum depth for impact analysis.
    pub depth: Option<usize>,
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
pub struct FindCalleesRequest {
    /// Project path
    pub project: String,
    /// Symbol to find direct outgoing references for
    pub symbol_id: String,
    /// Maximum callees to return (default: 100)
    pub limit: Option<usize>,
    /// Offset into callees for pagination (default: 0)
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FindCallersRequest {
    /// Project path
    pub project: String,
    /// Symbol to find direct incoming references for
    pub symbol_id: String,
    /// Restrict callers to a file or directory glob
    pub scope: Option<String>,
    /// Maximum callers to return (default: 100)
    pub limit: Option<usize>,
    /// Offset into callers for pagination (default: 0)
    pub offset: Option<usize>,
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

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaitForEmbeddingsRequest {
    /// Project path previously indexed
    pub project: String,
    /// Poll interval in milliseconds (default: 2000)
    pub poll_interval_ms: Option<u64>,
    /// Maximum seconds to wait before returning a timeout status (default: 300)
    pub timeout_secs: Option<u64>,
}

#[cfg(test)]
const DEFAULT_PUBLIC_TOOL_NAMES: &[&str] = &[
    "ensure_project_ready",
    "investigate",
    "locate_code",
    "read_code_unit",
    "trace_path",
    "analyze_impact",
    "get_index_stats",
    "search_content",
];

const ADVANCED_TOOL_NAMES: &[&str] = &[
    "index_project",
    "search_symbols",
    "search_files",
    "navigate_code",
    "trace_execution_path",
    "get_symbol",
    "get_file_outline",
    "get_lines",
    "get_project_outline",
    "find_callees",
    "find_callers",
    "find_usages",
    "watch_project",
    "get_usage_stats",
    "wait_for_embeddings",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolExposureTier {
    Default,
    All,
}

impl ToolExposureTier {
    fn from_env() -> Self {
        match std::env::var("PITLANE_MCP_TOOL_TIER") {
            Ok(value) if value.eq_ignore_ascii_case("all") => Self::All,
            _ => Self::Default,
        }
    }
}

#[derive(Clone)]
pub struct PitlaneMcp {
    watcher_registry: Arc<WatcherRegistry>,
    embed_config: Option<Arc<EmbedConfig>>,
    tool_router: ToolRouter<Self>,
    public_tool_router: ToolRouter<Self>,
    tool_exposure_tier: ToolExposureTier,
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
        let tool_router = Self::tool_router();
        let public_tool_router = build_public_tool_router(tool_router.clone());
        Self {
            watcher_registry,
            embed_config,
            tool_router,
            public_tool_router,
            tool_exposure_tier: ToolExposureTier::from_env(),
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

fn build_public_tool_router(mut tool_router: ToolRouter<PitlaneMcp>) -> ToolRouter<PitlaneMcp> {
    for name in ADVANCED_TOOL_NAMES {
        tool_router.remove_route(name);
    }
    tool_router
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
        description = "Advanced startup tool that parses and indexes a project. Prefer ensure_project_ready for normal use.",
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
        description = "Prepare a repo for navigation and report indexing or embedding readiness. Use this once at startup.",
        meta = tool_meta("ready startup initialize index embeddings semantic setup"),
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn ensure_project_ready(
        &self,
        Parameters(req): Parameters<EnsureProjectReadyRequest>,
        peer: Peer<RoleServer>,
        meta: rmcp::model::Meta,
    ) -> String {
        let params = tools::ensure_project_ready::EnsureProjectReadyParams {
            path: req.path,
            exclude: req.exclude,
            force: req.force,
            max_files: req.max_files,
            poll_interval_ms: req.poll_interval_ms,
            timeout_secs: req.timeout_secs,
            progress_token: meta.get_progress_token(),
            peer: Some(peer),
            embed_config: self.embed_config.clone(),
        };
        match tools::ensure_project_ready::ensure_project_ready(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Advanced symbol lookup by exact name or intent. Prefer locate_code unless you already know the target is a symbol.",
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
        description = "Search indexed source text for a known snippet, log string, import path, or regex fragment. Use this only when you know text but not the owning symbol.",
        meta = tool_meta("content text grep regex snippet string file search"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search_content(&self, Parameters(req): Parameters<SearchContentRequest>) -> String {
        let params = tools::search_content::SearchContentParams {
            project: req.project,
            query: req.query,
            regex: req.regex,
            case_sensitive: req.case_sensitive,
            language: req.language,
            file: req.file,
            limit: req.limit,
            offset: req.offset,
            before_context: req.before_context,
            after_context: req.after_context,
        };
        match tools::search_content::search_content(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Advanced file-path lookup by name, path fragment, or glob. Prefer locate_code unless you already know the target is a file.",
        meta = tool_meta("files path filename glob tests search discover"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search_files(&self, Parameters(req): Parameters<SearchFilesRequest>) -> String {
        let params = tools::search_files::SearchFilesParams {
            project: req.project,
            query: req.query,
            mode: req.mode,
            language: req.language,
            file: req.file,
            limit: req.limit,
            offset: req.offset,
        };
        match tools::search_files::search_files(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Investigate a code question in one call. Discovers relevant symbols, reads their source, and returns a prose answer with code inlined. Use this instead of multiple locate_code + read_code_unit calls when you need to understand a subsystem, execution path, or implementation.",
        meta = tool_meta("investigate question answer code subsystem path flow"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn investigate(&self, Parameters(req): Parameters<InvestigateRequest>) -> String {
        let params = tools::investigate::InvestigateParams {
            project: req.project,
            query: req.query,
            language: req.language,
            scope: req.scope,
        };
        match tools::investigate::investigate(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Find the most likely code target for an ambiguous query such as a symbol, file, or snippet. Use this when you do not yet know which lower-level lookup fits.",
        meta = tool_meta("locate navigate discover symbol file snippet project"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn locate_code(&self, Parameters(req): Parameters<LocateCodeRequest>) -> String {
        let params = tools::orchestrator::LocateCodeParams {
            project: req.project,
            query: req.query,
            intent: req.intent,
            kind: req.kind,
            language: req.language,
            scope: req.scope,
            limit: req.limit,
        };
        match tools::orchestrator::locate_code(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Read the smallest useful code unit for a known target, such as a symbol, file outline, or line slice. Use this after discovery or tracing identifies what to inspect.",
        meta = tool_meta("read unit symbol file lines slice"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn read_code_unit(&self, Parameters(req): Parameters<ReadCodeUnitRequest>) -> String {
        let params = tools::orchestrator::ReadCodeUnitParams {
            project: req.project,
            symbol_id: req.symbol_id,
            file_path: req.file_path,
            line_start: req.line_start,
            line_end: req.line_end,
            include_context: req.include_context,
            signature_only: req.signature_only,
        };
        match tools::orchestrator::read_code_unit(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Trace a likely execution or data-flow path from a behavior question or source and sink hints. Use this for call chains, config-to-effect, and entrypoint-to-output questions.",
        meta = tool_meta("trace path flow call chain source sink execution"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn trace_path(&self, Parameters(req): Parameters<TracePathRequest>) -> String {
        let params = tools::orchestrator::TracePathParams {
            project: req.project,
            query: req.query,
            source: req.source,
            sink: req.sink,
            language: req.language,
            file: req.file,
            max_symbols: req.max_symbols,
            max_depth: req.max_depth,
        };
        match tools::orchestrator::trace_path(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Estimate the blast radius of changing a symbol, file, or concept. Use this before edits or refactors.",
        meta = tool_meta("impact blast radius callers usages refactor change"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn analyze_impact(&self, Parameters(req): Parameters<AnalyzeImpactRequest>) -> String {
        let params = tools::orchestrator::AnalyzeImpactParams {
            project: req.project,
            query: req.query,
            symbol_id: req.symbol_id,
            file_path: req.file_path,
            scope: req.scope,
            depth: req.depth,
            limit: req.limit,
        };
        match tools::orchestrator::analyze_impact(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Advanced umbrella router across locate, read, trace, and impact. Prefer locate_code or trace_path unless you want the server to choose the workflow.",
        meta = tool_meta("navigate locate read trace impact route intent"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn navigate_code(&self, Parameters(req): Parameters<NavigateCodeRequest>) -> String {
        let params = tools::orchestrator::NavigateCodeParams {
            project: req.project,
            query: req.query,
            intent: req.intent,
            symbol_id: req.symbol_id,
            file_path: req.file_path,
            line_start: req.line_start,
            line_end: req.line_end,
            include_context: req.include_context,
            signature_only: req.signature_only,
            source: req.source,
            sink: req.sink,
            kind: req.kind,
            language: req.language,
            scope: req.scope,
            limit: req.limit,
            max_symbols: req.max_symbols,
            max_depth: req.max_depth,
            depth: req.depth,
        };
        match tools::orchestrator::navigate_code(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Advanced behavior-level tracer for one-step execution-path discovery. Prefer trace_path for the default path-tracing surface.",
        meta = tool_meta("trace execution path architecture pipeline call graph flow"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn trace_execution_path(
        &self,
        Parameters(req): Parameters<TraceExecutionPathRequest>,
    ) -> String {
        let params = tools::trace_execution_path::TraceExecutionPathParams {
            project: req.project,
            query: req.query,
            source: req.source,
            sink: req.sink,
            language: req.language,
            file: req.file,
            max_symbols: req.max_symbols,
            max_depth: req.max_depth,
            embed_config: self.embed_config.clone(),
        };
        match tools::trace_execution_path::trace_execution_path(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Advanced symbol read by stable ID. Prefer read_code_unit for the default read surface.",
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
            include_references: None,
        };
        match tools::get_symbol::get_symbol(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Advanced file-structure read without source text. Prefer read_code_unit unless you specifically need just the outline.",
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
        description = "Advanced line-range read for exact file slices. Prefer read_code_unit unless you already know the needed line span.",
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
        description = "Advanced repo outline grouped by directory. Prefer get_index_stats or locate_code for default orientation.",
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
        description = "Advanced direct outgoing-reference view for one symbol. Prefer trace_path or analyze_impact for ranked graph navigation.",
        meta = tool_meta("callees outgoing calls dependencies symbol"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn find_callees(&self, Parameters(req): Parameters<FindCalleesRequest>) -> String {
        let params = tools::find_callees::FindCalleesParams {
            project: req.project,
            symbol_id: req.symbol_id,
            limit: req.limit,
            offset: req.offset,
        };
        match tools::find_callees::find_callees(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Advanced direct incoming-reference view for one symbol. Prefer analyze_impact for ranked change analysis.",
        meta = tool_meta("callers incoming calls references impact symbol"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn find_callers(&self, Parameters(req): Parameters<FindCallersRequest>) -> String {
        let params = tools::find_callers::FindCallersParams {
            project: req.project,
            symbol_id: req.symbol_id,
            scope: req.scope,
            limit: req.limit,
            offset: req.offset,
        };
        match tools::find_callers::find_callers(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }

    #[tool(
        description = "Advanced usage listing for one symbol. Use this when you need raw call sites before refactoring.",
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
        description = "Advanced maintenance tool that keeps the index updated as files change. Use this only for long-lived sessions.",
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
        description = "Show lightweight repo orientation data such as indexed languages and symbol counts. Use this before broader repo exploration.",
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
        description = "Advanced diagnostics for token savings from signature-only symbol reads.",
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

    #[tool(
        description = "Advanced blocking wait for semantic-search embeddings. Prefer ensure_project_ready unless you explicitly need semantic readiness before continuing.",
        meta = tool_meta("embeddings progress wait semantic search ready"),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn wait_for_embeddings(
        &self,
        Parameters(req): Parameters<WaitForEmbeddingsRequest>,
        peer: Peer<RoleServer>,
        meta: rmcp::model::Meta,
    ) -> String {
        let params = tools::wait_for_embeddings::WaitForEmbeddingsParams {
            project: req.project,
            poll_interval_ms: req.poll_interval_ms,
            timeout_secs: req.timeout_secs,
            progress_token: meta.get_progress_token(),
            peer: Some(peer),
            embed_config: self.embed_config.clone(),
        };
        match tools::wait_for_embeddings::wait_for_embeddings(params).await {
            Ok(v) => value_to_text(v),
            Err(e) => err_to_text(e),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PitlaneMcp {
    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + '_ {
        let tools = match self.tool_exposure_tier {
            ToolExposureTier::Default => self.public_tool_router.list_all(),
            ToolExposureTier::All => self.tool_router.list_all(),
        };
        std::future::ready(Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        }))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        match self.tool_exposure_tier {
            ToolExposureTier::Default => self.public_tool_router.get(name).cloned(),
            ToolExposureTier::All => self.tool_router.get(name).cloned(),
        }
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_logging().build())
            .with_instructions(
                "pitlane-mcp: token-efficient code navigation. \
                Default tool tier: ensure_project_ready, locate_code, read_code_unit, trace_path, analyze_impact, get_index_stats, and search_content. \
                Suggested flow: start with ensure_project_ready; use locate_code for ambiguous discovery; use read_code_unit to inspect a chosen target; use trace_path for flow questions; use analyze_impact before edits; use get_index_stats for lightweight orientation; use search_content only when you know a text fragment. \
                Advanced primitive tools are hidden from tools/list by default to reduce agent branching. Set PITLANE_MCP_TOOL_TIER=all to expose the full primitive surface.",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_names(router: &ToolRouter<PitlaneMcp>) -> Vec<String> {
        router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect()
    }

    #[test]
    fn default_public_tool_set_matches_expected_names() {
        let public = build_public_tool_router(PitlaneMcp::tool_router());
        let mut names = tool_names(&public);
        names.sort();
        let mut expected = DEFAULT_PUBLIC_TOOL_NAMES
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        expected.sort();

        assert_eq!(names, expected);
    }

    #[test]
    fn advanced_tools_are_hidden_from_default_public_router() {
        let public = build_public_tool_router(PitlaneMcp::tool_router());
        let names = tool_names(&public);

        for advanced in ADVANCED_TOOL_NAMES {
            assert!(
                !names.iter().any(|name| name == advanced),
                "advanced tool {advanced} should not be listed in the default tier"
            );
        }
    }

    #[test]
    #[ignore = "env-var mutation races with parallel tests"]
    fn tool_exposure_tier_defaults_to_default() {
        let prev = std::env::var("PITLANE_MCP_TOOL_TIER").ok();
        std::env::remove_var("PITLANE_MCP_TOOL_TIER");

        assert_eq!(ToolExposureTier::from_env(), ToolExposureTier::Default);

        match prev {
            Some(value) => std::env::set_var("PITLANE_MCP_TOOL_TIER", value),
            None => std::env::remove_var("PITLANE_MCP_TOOL_TIER"),
        }
    }

    #[test]
    #[ignore = "env-var mutation races with parallel tests"]
    fn tool_exposure_tier_all_enables_full_surface() {
        let prev = std::env::var("PITLANE_MCP_TOOL_TIER").ok();
        std::env::set_var("PITLANE_MCP_TOOL_TIER", "all");

        assert_eq!(ToolExposureTier::from_env(), ToolExposureTier::All);

        match prev {
            Some(value) => std::env::set_var("PITLANE_MCP_TOOL_TIER", value),
            None => std::env::remove_var("PITLANE_MCP_TOOL_TIER"),
        }
    }
}
