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

#[derive(Debug, Subcommand)]
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
    /// Trace the strongest graph-backed path for a query
    TracePath {
        /// Path to the indexed project
        project: String,
        /// Flow or execution-path intent to trace
        query: String,
        /// Optional source symbol or source hint
        #[arg(long)]
        source: Option<String>,
        /// Optional sink symbol or sink hint
        #[arg(long)]
        sink: Option<String>,
        /// Restrict tracing to a language
        #[arg(long)]
        lang: Option<String>,
        /// Restrict tracing to files matching a glob pattern
        #[arg(long)]
        file: Option<String>,
        /// Maximum important symbols to retain
        #[arg(long)]
        max_symbols: Option<usize>,
        /// Maximum graph expansion depth
        #[arg(long)]
        max_depth: Option<usize>,
    },
    /// Analyze blast radius using weighted graph traversal
    AnalyzeImpact {
        /// Path to the indexed project
        project: String,
        /// Optional symbol or concept query
        #[arg(long)]
        query: Option<String>,
        /// Stable symbol ID to analyze directly
        #[arg(long)]
        symbol_id: Option<String>,
        /// File path to analyze when the target is file-centric
        #[arg(long)]
        file_path: Option<String>,
        /// Restrict callers/usages to a subtree or glob
        #[arg(long)]
        scope: Option<String>,
        /// Maximum traversal depth
        #[arg(long)]
        depth: Option<usize>,
        /// Maximum impacted symbols/files to return
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show token-efficiency statistics for get_symbol
    UsageStats {
        /// Path to the indexed project (omit for global totals)
        project: Option<String>,
    },
}

fn embeddings_count_in_store(path: &std::path::Path) -> usize {
    pitlane_mcp::embed::store::EmbedStore::load(path)
        .map(|store| store.vectors.len())
        .unwrap_or(0)
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

    match cli.command {
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
            Ok(())
        }
        command => {
            let result = run_command(command).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}

async fn run_command(command: Command) -> anyhow::Result<serde_json::Value> {
    let result = match command {
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
                let total_embeddings_stored = embeddings_count_in_store(&store_path);
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("embeddings".into(), serde_json::json!(embed_status));
                    obj.insert(
                        "embeddings_stored".into(),
                        serde_json::json!(total_embeddings_stored),
                    );
                    obj.insert(
                        "embeddings_newly_stored".into(),
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

        Command::Watch { .. } => unreachable!("watch is handled directly in main"),

        Command::Stats { project } => {
            let params = tools::get_index_stats::GetIndexStatsParams { project };
            tools::get_index_stats::get_index_stats(params).await?
        }

        Command::TracePath {
            project,
            query,
            source,
            sink,
            lang,
            file,
            max_symbols,
            max_depth,
        } => {
            let params = tools::orchestrator::TracePathParams {
                project,
                query,
                source,
                sink,
                language: lang,
                file,
                max_symbols,
                max_depth,
            };
            tools::orchestrator::trace_path(params).await?
        }

        Command::AnalyzeImpact {
            project,
            query,
            symbol_id,
            file_path,
            scope,
            depth,
            limit,
        } => {
            let params = tools::orchestrator::AnalyzeImpactParams {
                project,
                query,
                symbol_id,
                file_path,
                scope,
                depth,
                limit,
            };
            tools::orchestrator::analyze_impact(params).await?
        }

        Command::UsageStats { project } => {
            let params = tools::get_usage_stats::GetUsageStatsParams { project };
            tools::get_usage_stats::get_usage_stats(params).await?
        }
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup_project(dir: &TempDir) -> String {
        let project = dir.path().to_string_lossy().to_string();
        let params = tools::index_project::IndexProjectParams {
            path: project.clone(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        };
        tools::index_project::index_project(params).await.unwrap();
        project
    }

    #[test]
    fn embeddings_count_in_store_returns_saved_vector_count() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut store = pitlane_mcp::embed::store::EmbedStore::new();
        store.update("sym:a".to_string(), vec![1.0, 2.0]);
        store.update("sym:b".to_string(), vec![3.0, 4.0]);
        store.save(tmp.path()).unwrap();

        assert_eq!(embeddings_count_in_store(tmp.path()), 2);
    }

    #[test]
    fn clap_parses_trace_path_command() {
        let cli = Cli::try_parse_from([
            "pitlane",
            "trace-path",
            "/tmp/project",
            "config to sink",
            "--source",
            "bootstrap",
            "--sink",
            "handler",
            "--lang",
            "rust",
            "--file",
            "src/**/*.rs",
            "--max-symbols",
            "7",
            "--max-depth",
            "3",
        ])
        .unwrap();

        match cli.command {
            Command::TracePath {
                project,
                query,
                source,
                sink,
                lang,
                file,
                max_symbols,
                max_depth,
            } => {
                assert_eq!(project, "/tmp/project");
                assert_eq!(query, "config to sink");
                assert_eq!(source.as_deref(), Some("bootstrap"));
                assert_eq!(sink.as_deref(), Some("handler"));
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(file.as_deref(), Some("src/**/*.rs"));
                assert_eq!(max_symbols, Some(7));
                assert_eq!(max_depth, Some(3));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn clap_parses_analyze_impact_command() {
        let cli = Cli::try_parse_from([
            "pitlane",
            "analyze-impact",
            "/tmp/project",
            "--symbol-id",
            "sym::bootstrap",
            "--scope",
            "src/**/*.rs",
            "--depth",
            "2",
            "--limit",
            "5",
        ])
        .unwrap();

        match cli.command {
            Command::AnalyzeImpact {
                project,
                query,
                symbol_id,
                file_path,
                scope,
                depth,
                limit,
            } => {
                assert_eq!(project, "/tmp/project");
                assert_eq!(query, None);
                assert_eq!(symbol_id.as_deref(), Some("sym::bootstrap"));
                assert_eq!(file_path, None);
                assert_eq!(scope.as_deref(), Some("src/**/*.rs"));
                assert_eq!(depth, Some(2));
                assert_eq!(limit, Some(5));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_command_trace_path_includes_edge_metadata() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn leaf() {}\npub fn branch() { leaf(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = run_command(Command::TracePath {
            project,
            query: "branch to leaf".to_string(),
            source: Some("branch".to_string()),
            sink: Some("leaf".to_string()),
            lang: None,
            file: None,
            max_symbols: Some(5),
            max_depth: Some(2),
        })
        .await
        .unwrap();

        let edges = result["edges"].as_array().unwrap();
        assert!(!edges.is_empty());
        assert!(edges[0]["path_cost"].as_u64().is_some());
        assert!(edges[0]["evidence_quality"].as_f64().is_some());
    }

    #[tokio::test]
    async fn run_command_analyze_impact_includes_support_edges() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn leaf() {}\npub fn branch() { leaf(); }\npub fn wrapper(f: fn()) { f(); }\npub fn root() { wrapper(leaf); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = run_command(Command::AnalyzeImpact {
            project,
            query: Some("leaf".to_string()),
            symbol_id: None,
            file_path: None,
            scope: None,
            depth: Some(2),
            limit: Some(5),
        })
        .await
        .unwrap();

        let impact_symbols = result["impact_symbols"].as_array().unwrap();
        assert!(!impact_symbols.is_empty());
        let support_edges = impact_symbols[0]["support_edges"].as_array().unwrap();
        assert!(!support_edges.is_empty());
        assert_eq!(support_edges[0]["relation"], serde_json::json!("calls"));
        assert!(support_edges[0]["evidence_quality"].as_f64().is_some());
        assert!(support_edges[0]["score"].as_i64().is_some());
    }
}
