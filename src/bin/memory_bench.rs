/// Measures peak RAM consumption, indexing latency, index disk size, and token
/// efficiency for a given project path.
///
/// Indexing time is reported as min and median over the requested run count so
/// that single-sample noise does not skew the result.
///
/// Peak RAM is the absolute VmHWM (high-water mark of resident set size) read
/// from /proc/self/status after the first cold indexing run.  This includes
/// rayon thread-pool startup and tokio runtime — real costs for the tool.
/// Subsequent runs reuse those resources, so later VmHWM readings would
/// undercount the true first-run peak.
///
/// Usage:
///   cargo run --features memory-bench --bin memory_bench                       # run all known repos
///   cargo run --features memory-bench --bin memory_bench -- ripgrep            # by name
///   cargo run --features memory-bench --bin memory_bench -- ripgrep fastapi    # multiple
///   cargo run --features memory-bench --bin memory_bench -- bench/repos/ripgrep # by path
///   cargo run --features memory-bench --bin memory_bench -- --single guava     # one sample only
///   cargo run --features memory-bench --bin memory_bench -- --runs 2 guava     # custom run count
use pitlane_mcp::{
    indexer::language::SymbolKind,
    tools::index_project::{index_project, load_project_index, IndexProjectParams},
};
use serde::{Deserialize, Serialize};
use std::{fs, time::Instant};
use tokio::runtime::Runtime;

/// Returns peak RSS in kilobytes, or `None` on unsupported platforms.
///
/// - Linux: reads `VmHWM` from `/proc/self/status`
/// - macOS: calls `getrusage(RUSAGE_SELF)` — `ru_maxrss` is in bytes on macOS
/// - Windows and others: not supported, returns `None`
fn peak_rss_kb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if line.starts_with("VmHWM:") {
                return line.split_whitespace().nth(1)?.parse().ok();
            }
        }
        None
    }

    #[cfg(target_os = "macos")]
    {
        // SAFETY: getrusage is always safe to call with RUSAGE_SELF and a
        // valid output pointer. ru_maxrss on macOS is in bytes (unlike Linux).
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
        if ret == 0 {
            Some(ru.ru_maxrss as u64 / 1024)
        } else {
            None
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Number of indexing runs used for timing statistics.
const DEFAULT_TIMING_RUNS: usize = 5;

#[derive(Debug, Serialize, Deserialize)]
struct BenchRunOutput {
    time_ms: u128,
    peak_ram_kb: Option<u64>,
    index_path: String,
    symbol_count: u64,
    file_count: u64,
    efficiency_line: Option<String>,
}

#[derive(Debug, Clone)]
struct BenchCli {
    paths: Vec<String>,
    runs: usize,
}

/// Known benchmark repos. Used when resolving short names and when running all repos.
const KNOWN_REPOS: &[(&str, &str)] = &[
    ("ripgrep", "bench/repos/ripgrep"),
    ("fastapi", "bench/repos/fastapi"),
    ("hono", "bench/repos/hono"),
    ("svelte.dev", "bench/repos/svelte.dev"),
    ("redis", "bench/repos/redis"),
    ("leveldb", "bench/repos/leveldb"),
    ("gin", "bench/repos/gin"),
    ("guava", "bench/repos/guava"),
    ("bats", "bench/repos/bats"),
    ("newtonsoft", "bench/repos/newtonsoft"),
    ("rubocop", "bench/repos/rubocop"),
    ("swiftlint", "bench/repos/swiftlint"),
    ("sdwebimage", "bench/repos/sdwebimage"),
    ("laravel", "bench/repos/laravel"),
    ("zls", "bench/repos/zls"),
    ("okhttp", "bench/repos/okhttp"),
    ("roact", "bench/repos/roact"),
    (
        "openzeppelin-contracts",
        "bench/repos/openzeppelin-contracts",
    ),
];

fn resolve_path(arg: &str) -> String {
    // If the arg contains a path separator it's already a path.
    if arg.contains('/') || arg.contains('\\') {
        return arg.to_string();
    }
    // Otherwise look it up in the known repos list.
    KNOWN_REPOS
        .iter()
        .find(|(name, _)| *name == arg)
        .map(|(_, path)| path.to_string())
        .unwrap_or_else(|| arg.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // When spawned as a child process by ourselves, run the bench directly so
    // each corpus gets its own isolated VmHWM reading.
    if args.first().map(|s| s.as_str()) == Some("--_bench-child") {
        let path = args
            .get(1)
            .expect("--_bench-child requires a path argument");
        let runs = parse_runs_flag(&args[2..]).unwrap_or(DEFAULT_TIMING_RUNS);
        run_bench(path, runs);
        return;
    }
    if args.first().map(|s| s.as_str()) == Some("--_bench-once") {
        let path = args.get(1).expect("--_bench-once requires a path argument");
        let include_efficiency = args.get(2).is_some_and(|arg| arg == "--efficiency");
        run_single_bench(path, include_efficiency);
        return;
    }

    let cli = parse_cli(&args);

    // Spawn a fresh subprocess per corpus so memory does not accumulate across
    // runs and each VmHWM reading reflects only that corpus.
    let exe = std::env::current_exe().expect("cannot determine executable path");
    for path in cli.paths {
        let status = std::process::Command::new(&exe)
            .arg("--_bench-child")
            .arg(&path)
            .arg("--runs")
            .arg(cli.runs.to_string())
            .status()
            .unwrap_or_else(|e| {
                eprintln!("Failed to spawn child for {path}: {e}");
                std::process::exit(1);
            });
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}

fn run_bench(path: &str, runs: usize) {
    let runs = runs.max(1);
    let mut times_ms: Vec<u128> = Vec::with_capacity(runs);
    let mut peak_ram_kb: Option<u64> = None;
    let mut summary = None;

    for i in 0..runs {
        let output = spawn_bench_once(path, i == 0);
        times_ms.push(output.time_ms);
        if i == 0 {
            peak_ram_kb = output.peak_ram_kb;
            summary = Some(output);
        }
    }

    let summary = summary.expect("first bench sample should have produced a summary");

    times_ms.sort_unstable();
    let min_ms = times_ms[0];
    let median_ms = times_ms[runs / 2];

    let disk_bytes = fs::metadata(&summary.index_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let efficiency_line = summary
        .efficiency_line
        .unwrap_or_else(|| "N/A (not computed)".to_string());

    println!("────────────────────────────────────────");
    println!("Project:          {path}");
    println!("────────────────────────────────────────");
    println!("Files indexed:    {}", summary.file_count);
    println!("Symbols indexed:  {}", summary.symbol_count);
    println!(
        "Indexing time:    min {} ms  median {} ms  ({} runs)",
        min_ms, median_ms, runs
    );
    match peak_ram_kb {
        Some(kb) => println!(
            "Peak RAM (VmHWM): {} KB  ({:.1} MB)  [first-run absolute peak]",
            kb,
            kb as f64 / 1024.0
        ),
        None => println!("Peak RAM (VmHWM): N/A (unsupported platform)"),
    };
    println!(
        "Index disk size:  {} bytes  ({:.1} KB)",
        disk_bytes,
        disk_bytes as f64 / 1024.0
    );
    println!("Token efficiency: {efficiency_line}");
    println!("────────────────────────────────────────");
}

fn parse_cli(args: &[String]) -> BenchCli {
    let mut runs = DEFAULT_TIMING_RUNS;
    let mut raw_paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--single" => {
                runs = 1;
                i += 1;
            }
            "--runs" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("--runs requires a positive integer");
                    std::process::exit(2);
                };
                runs = parse_runs_value(value);
                i += 2;
            }
            arg => {
                raw_paths.push(arg.to_string());
                i += 1;
            }
        }
    }

    let paths = if raw_paths.is_empty() {
        KNOWN_REPOS.iter().map(|(_, p)| p.to_string()).collect()
    } else {
        raw_paths.iter().map(|a| resolve_path(a)).collect()
    };

    BenchCli { paths, runs }
}

fn parse_runs_flag(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--runs" {
            let value = args.get(i + 1)?;
            return Some(parse_runs_value(value));
        }
        i += 1;
    }
    None
}

fn parse_runs_value(value: &str) -> usize {
    let runs = value.parse::<usize>().unwrap_or_else(|_| {
        eprintln!("invalid --runs value '{value}': expected a positive integer");
        std::process::exit(2);
    });
    if runs == 0 {
        eprintln!("invalid --runs value '{value}': expected a positive integer");
        std::process::exit(2);
    }
    runs
}

fn spawn_bench_once(path: &str, include_efficiency: bool) -> BenchRunOutput {
    let exe = std::env::current_exe().expect("cannot determine executable path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--_bench-once").arg(path);
    if include_efficiency {
        cmd.arg("--efficiency");
    }
    let output = cmd.output().unwrap_or_else(|e| {
        eprintln!("Failed to spawn bench sample for {path}: {e}");
        std::process::exit(1);
    });
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(output.status.code().unwrap_or(1));
    }
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        eprintln!(
            "Failed to decode bench sample output for {path}: {e}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        std::process::exit(1);
    })
}

fn run_single_bench(path: &str, include_efficiency: bool) {
    let rt = Runtime::new().expect("Failed to create tokio runtime");
    let wall_start = Instant::now();
    let result = rt
        .block_on(index_project(IndexProjectParams {
            path: path.to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
            on_index_progress: None,
            on_phase3_progress: None,
        }))
        .unwrap_or_else(|e| {
            eprintln!("Indexing failed: {e}");
            std::process::exit(1);
        });

    let output = BenchRunOutput {
        time_ms: wall_start.elapsed().as_millis(),
        peak_ram_kb: peak_rss_kb(),
        index_path: result
            .get("index_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        symbol_count: result
            .get("symbol_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        file_count: result
            .get("file_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        efficiency_line: include_efficiency.then(|| compute_efficiency_line(path)),
    };

    println!("{}", serde_json::to_string(&output).unwrap());
}

fn compute_efficiency_line(path: &str) -> String {
    match load_project_index(path) {
        Ok(index) => {
            let mut candidates: Vec<_> = index
                .symbols
                .values()
                .filter(|s| {
                    matches!(
                        s.kind,
                        SymbolKind::Struct
                            | SymbolKind::Class
                            | SymbolKind::Interface
                            | SymbolKind::TypeAlias
                    )
                })
                .filter_map(|s| {
                    let sym_bytes = s.byte_end.saturating_sub(s.byte_start);
                    if sym_bytes == 0 {
                        return None;
                    }
                    let file_bytes = fs::metadata(&*s.file)
                        .map(|m| m.len() as usize)
                        .unwrap_or(0);
                    Some((s, sym_bytes, file_bytes))
                })
                .collect();

            if candidates.is_empty() {
                "N/A (no struct/class found)".to_string()
            } else {
                let mut ratios: Vec<f64> = candidates
                    .iter()
                    .map(|(_, sym_bytes, file_bytes)| {
                        *file_bytes as f64 / (*sym_bytes).max(1) as f64
                    })
                    .collect();
                ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let median = ratios[ratios.len() / 2];

                candidates.sort_by_key(|(_, sym_bytes, _)| *sym_bytes);
                let (largest, sym_bytes, file_bytes) = candidates.last().unwrap();

                format!(
                    "median {:.1}x  |  largest: {:.1}x  (symbol '{}': {} B  vs  full file: {} B)",
                    median,
                    *file_bytes as f64 / (*sym_bytes).max(1) as f64,
                    largest.name,
                    sym_bytes,
                    file_bytes,
                )
            }
        }
        Err(e) => format!("N/A ({e})"),
    }
}
