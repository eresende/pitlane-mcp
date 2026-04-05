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
            let params = tools::index_project::IndexProjectParams {
                path,
                exclude: if exclude.is_empty() {
                    None
                } else {
                    Some(exclude)
                },
                force: if force { Some(true) } else { None },
                max_files: None,
                progress_token: None,
                peer: None,
            };
            tools::index_project::index_project(params).await?
        }

        Command::Search {
            project,
            query,
            kind,
            lang,
            file,
            limit,
            offset,
        } => {
            let params = tools::search_symbols::SearchSymbolsParams {
                project,
                query,
                kind,
                language: lang,
                file,
                limit: Some(limit),
                offset: Some(offset),
                mode: None,
            };
            tools::search_symbols::search_symbols(params).await?
        }

        Command::Outline { project, depth } => {
            let params = tools::get_project_outline::GetProjectOutlineParams {
                project,
                depth: Some(depth),
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

        Command::Stats { project } => {
            let params = tools::get_index_stats::GetIndexStatsParams { project };
            tools::get_index_stats::get_index_stats(params).await?
        }
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
