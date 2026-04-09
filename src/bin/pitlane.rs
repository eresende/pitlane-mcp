use std::sync::Arc;

use clap::{Parser, Subcommand};
use pitlane_mcp::path_policy::resolve_project_path;
use pitlane_mcp::tools;

#[derive(Parser)]
#[command(name = "pitlane", about = "pitlane-mcp code intelligence CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index a project (or load from cache if up to date)
    Index {
        /// Path to the project root
        path: String,
        /// Re-index even if an up-to-date index exists
        #[arg(long)]
        force: bool,
        /// Glob patterns to exclude (repeatable)
        #[arg(long = "exclude", value_name = "GLOB")]
        exclude: Vec<String>,
        /// Maximum source files to index (0 uses the default limit)
        #[arg(long)]
        max_files: Option<usize>,
    },
    /// Search for symbols by name
    Search {
        /// Path to the indexed project
        project: String,
        /// Substring or prefix to match against symbol name
        query: String,
        /// Filter by kind (function, method, struct, enum, trait, …)
        #[arg(long)]
        kind: Option<String>,
        /// Filter by language (rust, python, cpp, go, …)
        #[arg(long)]
        lang: Option<String>,
        /// Restrict to files matching a glob pattern
        #[arg(long)]
        file: Option<String>,
        /// Maximum results (default: 20)
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Skip first N results
        #[arg(long, default_value = "0")]
        offset: usize,
        /// Search mode: bm25 (default), exact, fuzzy, semantic
        #[arg(long)]
        mode: Option<String>,
    },
    /// Show a project's directory/symbol overview
    Outline {
        /// Path to the indexed project
        project: String,
        /// Directory depth to show (default: 2)
        #[arg(long, default_value = "2")]
        depth: u32,
        /// Only include files under this directory prefix
        #[arg(long)]
        path: Option<String>,
        /// Maximum directory entries to return
        #[arg(long)]
        max_dirs: Option<usize>,
        /// Show only directory summary counts
        #[arg(long)]
        summary: bool,
    },
    /// List all symbols in a file
    File {
        /// Path to the indexed project
        project: String,
        /// File path (relative to project root)
        file_path: String,
    },
    /// Fetch the source of a single symbol by its ID
    Symbol {
        /// Path to the indexed project
        project: String,
        /// Symbol ID (from search or file outline)
        symbol_id: String,
        /// Include 3 lines of context before/after
        #[arg(long)]
        context: bool,
        /// Return only signature and doc, skip the body
        #[arg(long)]
        sig_only: bool,
    },
    /// Show direct outgoing references for a symbol
    Callees {
        /// Path to the indexed project
        project: String,
        /// Symbol ID (from search or file outline)
        symbol_id: String,
        /// Maximum results (default: 100)
        #[arg(long, default_value = "100")]
        limit: usize,
        /// Skip first N results
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Show direct incoming references for a symbol
    Callers {
        /// Path to the indexed project
        project: String,
        /// Symbol ID (from search or file outline)
        symbol_id: String,
        /// Restrict callers to files matching a glob pattern
        #[arg(long)]
        scope: Option<String>,
        /// Maximum results (default: 100)
        #[arg(long, default_value = "100")]
        limit: usize,
        /// Skip first N results
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Find all call sites for a symbol
    Usages {
        /// Path to the indexed project
        project: String,
        /// Symbol ID (from search or file outline)
        symbol_id: String,
        /// Restrict matches to files matching a glob pattern
        #[arg(long)]
        scope: Option<String>,
        /// Maximum results (default: 100)
        #[arg(long, default_value = "100")]
        limit: usize,
        /// Skip first N results
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Fetch a specific line range from a file
    Lines {
        /// Path to the indexed project
        project: String,
        /// File path (relative to project root)
        file_path: String,
        /// First line to return, 1-indexed inclusive
        line_start: u32,
        /// Last line to return, 1-indexed inclusive
        line_end: u32,
    },
    /// Wait for embedding generation to finish
    WaitEmbeddings {
        /// Path to the indexed project
        project: String,
        /// Poll interval in milliseconds
        #[arg(long)]
        poll_interval_ms: Option<u64>,
        /// Maximum time to wait in seconds
        #[arg(long)]
        timeout_secs: Option<u64>,
    },
    /// Keep a project index updated until interrupted
    Watch {
        /// Path to the indexed project
        project: String,
    },
    /// Show index statistics (file/symbol counts by language and kind)
    Stats {
        /// Path to the indexed project
        project: String,
    },
    /// Show token-efficiency statistics for get_symbol
    UsageStats {
        /// Path to the indexed project (omit for global totals)
        project: Option<String>,
    },
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

    let cli = Cli::parse();

    let result = match cli.command {
        Command::Index {
            path,
            force,
            exclude,
            max_files,
        } => {
            let embed_config = pitlane_mcp::embed::EmbedConfig::from_env();

            // Pass embed_config=None so index_project doesn't spawn a background
            // embedding task that would be killed when the CLI process exits.
            let params = tools::index_project::IndexProjectParams {
                path: path.clone(),
                exclude: if exclude.is_empty() {
                    None
                } else {
                    Some(exclude)
                },
                force: if force { Some(true) } else { None },
                max_files,
                progress_token: None,
                peer: None,
                embed_config: None,
            };
            let mut result = tools::index_project::index_project(params).await?;

            // Run embeddings synchronously so the CLI waits for them to finish.
            if let Some(cfg) = embed_config {
                let canonical = resolve_project_path(&path)?;
                let idx_dir = pitlane_mcp::index::format::index_dir(&canonical)?;
                let store_path = idx_dir.join("embeddings.bin");
                let index = tools::index_project::load_project_index(&path)?;
                let force_embed = force;
                let embed_result = pitlane_mcp::embed::generate_embeddings(
                    &index,
                    &cfg,
                    &store_path,
                    force_embed,
                    None,
                    Some(&canonical),
                )
                .await;
                let embed_status = if embed_result.error.is_some() {
                    "error"
                } else {
                    "ok"
                };
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("embeddings".into(), serde_json::json!(embed_status));
                    obj.insert(
                        "embeddings_stored".into(),
                        serde_json::json!(embed_result.stored),
                    );
                    if let Some(err) = embed_result.error {
                        obj.insert("embeddings_error".into(), serde_json::json!(err));
                    }
                }
            }

            result
        }

        Command::Search {
            project,
            query,
            kind,
            lang,
            file,
            limit,
            offset,
            mode,
        } => {
            let embed_config = pitlane_mcp::embed::EmbedConfig::from_env().map(std::sync::Arc::new);
            let params = tools::search_symbols::SearchSymbolsParams {
                project,
                query,
                kind,
                language: lang,
                file,
                limit: Some(limit),
                offset: Some(offset),
                mode,
                embed_config,
            };
            tools::search_symbols::search_symbols(params).await?
        }

        Command::Outline {
            project,
            depth,
            path,
            max_dirs,
            summary,
        } => {
            let params = tools::get_project_outline::GetProjectOutlineParams {
                project,
                depth: Some(depth),
                path,
                max_dirs,
                summary: if summary { Some(true) } else { None },
            };
            tools::get_project_outline::get_project_outline(params).await?
        }

        Command::File { project, file_path } => {
            let params = tools::get_file_outline::GetFileOutlineParams { project, file_path };
            tools::get_file_outline::get_file_outline(params).await?
        }

        Command::Symbol {
            project,
            symbol_id,
            context,
            sig_only,
        } => {
            let params = tools::get_symbol::GetSymbolParams {
                project,
                symbol_id,
                include_context: if context { Some(true) } else { None },
                signature_only: if sig_only { Some(true) } else { None },
            };
            tools::get_symbol::get_symbol(params).await?
        }

        Command::Callees {
            project,
            symbol_id,
            limit,
            offset,
        } => {
            let params = tools::find_callees::FindCalleesParams {
                project,
                symbol_id,
                limit: Some(limit),
                offset: Some(offset),
            };
            tools::find_callees::find_callees(params).await?
        }

        Command::Callers {
            project,
            symbol_id,
            scope,
            limit,
            offset,
        } => {
            let params = tools::find_callers::FindCallersParams {
                project,
                symbol_id,
                scope,
                limit: Some(limit),
                offset: Some(offset),
            };
            tools::find_callers::find_callers(params).await?
        }

        Command::Usages {
            project,
            symbol_id,
            scope,
            limit,
            offset,
        } => {
            let params = tools::find_usages::FindUsagesParams {
                project,
                symbol_id,
                scope,
                limit: Some(limit),
                offset: Some(offset),
            };
            tools::find_usages::find_usages(params).await?
        }

        Command::Lines {
            project,
            file_path,
            line_start,
            line_end,
        } => {
            let params = tools::get_lines::GetLinesParams {
                project,
                file_path,
                line_start,
                line_end,
            };
            tools::get_lines::get_lines(params).await?
        }

        Command::WaitEmbeddings {
            project,
            poll_interval_ms,
            timeout_secs,
        } => {
            let embed_config = pitlane_mcp::embed::EmbedConfig::from_env().map(Arc::new);
            let params = tools::wait_for_embeddings::WaitForEmbeddingsParams {
                project,
                poll_interval_ms,
                timeout_secs,
                progress_token: None,
                peer: None,
                embed_config,
            };
            tools::wait_for_embeddings::wait_for_embeddings(params).await?
        }

        Command::Watch { project } => {
            let embed_config = pitlane_mcp::embed::EmbedConfig::from_env().map(Arc::new);
            let registry = tools::watch_project::WatcherRegistry::new();
            let params = tools::watch_project::WatchProjectParams {
                project: project.clone(),
                stop: None,
                status_only: None,
                embed_config,
            };
            let result = tools::watch_project::watch_project(params, &registry).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            tokio::signal::ctrl_c().await?;
            let _ = registry.stop(&project);
            return Ok(());
        }

        Command::Stats { project } => {
            let params = tools::get_index_stats::GetIndexStatsParams { project };
            tools::get_index_stats::get_index_stats(params).await?
        }

        Command::UsageStats { project } => {
            let params = tools::get_usage_stats::GetUsageStatsParams { project };
            tools::get_usage_stats::get_usage_stats(params).await?
        }
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
