use clap::{Parser, Subcommand};
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
    /// Show index statistics (file/symbol counts by language and kind)
    Stats {
        /// Path to the indexed project
        project: String,
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
                max_files: None,
                progress_token: None,
                peer: None,
                embed_config: None,
            };
            let mut result = tools::index_project::index_project(params).await?;

            // Run embeddings synchronously so the CLI waits for them to finish.
            if let Some(cfg) = embed_config {
                let canonical = std::path::Path::new(&path)
                    .canonicalize()
                    .unwrap_or_else(|_| std::path::PathBuf::from(&path));
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

        Command::Outline { project, depth } => {
            let params = tools::get_project_outline::GetProjectOutlineParams {
                project,
                depth: Some(depth),
                path: None,
                max_dirs: None,
                summary: None,
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

        Command::Stats { project } => {
            let params = tools::get_index_stats::GetIndexStatsParams { project };
            tools::get_index_stats::get_index_stats(params).await?
        }
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
