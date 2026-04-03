/// Measures peak RAM consumption, indexing latency, index disk size, and token
/// efficiency for a given project path.
///
/// Indexing time is reported as min and median over TIMING_RUNS runs so that
/// single-sample noise does not skew the result.
///
/// Peak RAM is the absolute VmHWM (high-water mark of resident set size) read
/// from /proc/self/status after the first cold indexing run.  This includes
/// rayon thread-pool startup and tokio runtime — real costs for the tool.
/// Subsequent runs reuse those resources, so later VmHWM readings would
/// undercount the true first-run peak.
///
/// Usage:
///   cargo run --features memory-bench --bin memory_bench              # run all known repos
///   cargo run --features memory-bench --bin memory_bench -- ripgrep   # by name
///   cargo run --features memory-bench --bin memory_bench -- ripgrep fastapi bats  # multiple
///   cargo run --features memory-bench --bin memory_bench -- bench/repos/ripgrep   # by path
use pitlane_mcp::{
    indexer::language::SymbolKind,
    tools::index_project::{index_project, load_project_index, IndexProjectParams},
};
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
const TIMING_RUNS: usize = 5;

/// Known benchmark repos. Used when resolving short names and when running all repos.
const KNOWN_REPOS: &[(&str, &str)] = &[
    ("ripgrep", "bench/repos/ripgrep"),
    ("fastapi", "bench/repos/fastapi"),
    ("hono", "bench/repos/hono"),
    ("redis", "bench/repos/redis"),
    ("leveldb", "bench/repos/leveldb"),
    ("gin", "bench/repos/gin"),
    ("guava", "bench/repos/guava"),
    ("bats", "bench/repos/bats"),
    ("newtonsoft", "bench/repos/newtonsoft"),
    ("rubocop", "bench/repos/rubocop"),
    ("swiftlint", "bench/repos/swiftlint"),
    ("sdwebimage", "bench/repos/sdwebimage"),
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
    let paths: Vec<String> = if args.is_empty() {
        KNOWN_REPOS.iter().map(|(_, p)| p.to_string()).collect()
    } else {
        args.iter().map(|a| resolve_path(a)).collect()
    };

    for path in paths {
        run_bench(&path);
    }
}

fn run_bench(path: &str) {
    let rt = Runtime::new().expect("Failed to create tokio runtime");

    let mut times_ms: Vec<u128> = Vec::with_capacity(TIMING_RUNS);
    let mut peak_ram_kb: Option<u64> = None;
    let mut last_result = None;

    for i in 0..TIMING_RUNS {
        let wall_start = Instant::now();
        let result = rt
            .block_on(index_project(IndexProjectParams {
                path: path.to_string(),
                exclude: None,
                force: Some(true), // always re-index for a clean measurement
                max_files: None,
                progress_token: None,
                peer: None,
            }))
            .unwrap_or_else(|e| {
                eprintln!("Indexing failed: {e}");
                std::process::exit(1);
            });
        times_ms.push(wall_start.elapsed().as_millis());

        // Capture peak RAM after the first run: rayon pool + tokio runtime are
        // now fully started, giving the true worst-case RSS.  Later runs reuse
        // those resources, so their VmHWM readings would undercount.
        if i == 0 {
            peak_ram_kb = peak_rss_kb();
            last_result = Some(result);
        }
    }

    let result = last_result.unwrap();

    times_ms.sort_unstable();
    let min_ms = times_ms[0];
    let median_ms = times_ms[TIMING_RUNS / 2];

    let index_path = result
        .get("index_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let disk_bytes = fs::metadata(index_path).map(|m| m.len()).unwrap_or(0);
    let symbol_count = result
        .get("symbol_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let file_count = result
        .get("file_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Token efficiency: for each struct/class/interface/type-alias symbol, compute
    // the ratio of its containing file size to the symbol's own byte span. Report
    // both the median (representative) and the largest-symbol value (worst case).
    // Interface and TypeAlias are included so TypeScript repos produce numbers.
    let efficiency_line = match load_project_index(&path) {
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
                // Median efficiency.
                let mut ratios: Vec<f64> = candidates
                    .iter()
                    .map(|(_, sym_bytes, file_bytes)| {
                        *file_bytes as f64 / (*sym_bytes).max(1) as f64
                    })
                    .collect();
                ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let median = ratios[ratios.len() / 2];

                // Largest symbol (worst-case for efficiency).
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
    };

    println!("────────────────────────────────────────");
    println!("Project:          {path}");
    println!("────────────────────────────────────────");
    println!("Files indexed:    {file_count}");
    println!("Symbols indexed:  {symbol_count}");
    println!(
        "Indexing time:    min {} ms  median {} ms  ({TIMING_RUNS} runs)",
        min_ms, median_ms
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
