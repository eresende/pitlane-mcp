/// Benchmarks for query tools (search_symbols, get_symbol, find_usages,
/// get_file_outline, get_project_outline) on real-world repos.
///
/// Each iteration includes loading the index from disk, reflecting end-to-end
/// latency a client experiences per tool call.
///
/// Prerequisites: run `bench/setup.sh` first to clone the benchmark repositories.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use pitlane_mcp::{
    indexer::language::SymbolKind,
    tools::{
        find_usages::{find_usages, FindUsagesParams},
        get_file_outline::{get_file_outline, GetFileOutlineParams},
        get_project_outline::{get_project_outline, GetProjectOutlineParams},
        get_symbol::{get_symbol, GetSymbolParams},
        index_project::{index_project, load_project_index, IndexProjectParams},
        search_symbols::{search_symbols, SearchSymbolsParams},
    },
};
use std::{fs, time::Duration};
use tokio::runtime::Runtime;

const REPOS: &[&str] = &[
    "bench/repos/ripgrep",
    "bench/repos/fastapi",
    "bench/repos/hono",
    "bench/repos/svelte.dev",
    "bench/repos/redis",
    "bench/repos/leveldb",
    "bench/repos/gin",
    "bench/repos/guava",
    "bench/repos/bats",
    "bench/repos/newtonsoft",
    "bench/repos/rubocop",
    "bench/repos/swiftlint",
    "bench/repos/sdwebimage",
    "bench/repos/laravel",
    "bench/repos/zls",
    "bench/repos/okhttp",
    "bench/repos/roact",
    "bench/repos/openzeppelin-contracts",
];

struct Setup {
    project: String,
    symbol_id: String,
    search_query: String,
    /// The search_query split into lowercase words at camelCase/digit boundaries,
    /// joined with spaces. Used to benchmark BM25 with partial-word queries.
    search_query_split: String,
    /// Relative path of the file containing the benchmark target symbol.
    /// Derived from the symbol_id prefix (format: `path/to/file::Name#kind`).
    file_path: String,
}

/// Split a camelCase/snake_case identifier into lowercase words, mirroring the
/// `CamelCaseTokenizer` in `src/index/bm25.rs`. Used to build split-word BM25
/// queries that exercise the tokenizer's camelCase decomposition.
fn split_identifier(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let n = chars.len();
    let mut words: Vec<String> = Vec::new();
    let mut start = 0;
    let mut i = 0;

    while i <= n {
        let split = if i == n || !chars[i].is_alphanumeric() {
            true
        } else if i > start {
            let prev = chars[i - 1];
            let cur = chars[i];
            let lower_to_upper =
                (prev.is_lowercase() || prev.is_ascii_digit()) && cur.is_uppercase();
            let caps_run_end = i + 1 < n
                && prev.is_uppercase()
                && cur.is_uppercase()
                && chars[i + 1].is_lowercase();
            let digit_letter = (prev.is_ascii_digit() && cur.is_alphabetic())
                || (prev.is_alphabetic() && cur.is_ascii_digit());
            lower_to_upper || caps_run_end || digit_letter
        } else {
            false
        };

        if split {
            if i > start {
                let word: String = chars[start..i]
                    .iter()
                    .map(|c| c.to_lowercase().next().unwrap())
                    .collect();
                words.push(word);
            }
            if i < n && !chars[i].is_alphanumeric() {
                i += 1;
            }
            start = i;
            if i == n {
                break;
            }
        } else {
            i += 1;
        }
    }

    words.join(" ")
}

/// representative struct/class as the benchmark target symbol.
///
/// Also reports the median token efficiency across all struct/class symbols,
/// which is more representative than the largest-symbol outlier.
fn prepare(path: &str, rt: &Runtime) -> Option<Setup> {
    if !std::path::Path::new(path).exists() {
        eprintln!("Skipping {path}: not found — run bench/setup.sh first");
        return None;
    }

    // Ensure the on-disk index exists (uses cache if already up-to-date).
    rt.block_on(index_project(IndexProjectParams {
        path: path.to_string(),
        exclude: None,
        force: Some(false),
        max_files: None,
        progress_token: None,
        peer: None,
        embed_config: None,
    }))
    .ok()?;

    let index = load_project_index(path).ok()?;
    let label = path.split('/').next_back().unwrap_or(path);

    // Collect all struct/class/interface/type-alias symbols with their file sizes
    // for efficiency stats. Including TS kinds ensures TypeScript repos produce
    // meaningful numbers rather than "N/A".
    let mut candidates: Vec<(&pitlane_mcp::indexer::language::Symbol, usize)> = index
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
            Some((s, file_bytes))
        })
        .collect();

    // Fall back to functions for languages without struct/class (e.g. Bash).
    if candidates.is_empty() {
        candidates = index
            .symbols
            .values()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .filter_map(|s| {
                let sym_bytes = s.byte_end.saturating_sub(s.byte_start);
                if sym_bytes == 0 {
                    return None;
                }
                let file_bytes = fs::metadata(&*s.file)
                    .map(|m| m.len() as usize)
                    .unwrap_or(0);
                Some((s, file_bytes))
            })
            .collect();
    }

    if candidates.is_empty() {
        eprintln!("No symbols found in {path}");
        return None;
    }

    // Median token efficiency across all candidates.
    let mut ratios: Vec<f64> = candidates
        .iter()
        .map(|(s, file_bytes)| {
            let sym_bytes = s.byte_end.saturating_sub(s.byte_start);
            *file_bytes as f64 / sym_bytes.max(1) as f64
        })
        .collect();
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = ratios[ratios.len() / 2];

    // Pick the largest symbol as the benchmark target (most demanding workload).
    candidates.sort_by_key(|(s, _)| s.byte_end.saturating_sub(s.byte_start));
    let (target, file_bytes) = candidates.last().unwrap();
    let sym_bytes = target.byte_end.saturating_sub(target.byte_start);

    // Extract relative file path from the symbol_id (format: "rel/path::Name#kind").
    let file_path = target.id.split("::").next().unwrap_or("").to_string();

    println!("[{label}] benchmark symbol: {}", target.id);
    println!("[{label}] benchmark file:   {file_path}");
    println!(
        "[{label}] token efficiency — largest: {:.1}x  (symbol {} B vs file {} B)  |  median: {:.1}x",
        *file_bytes as f64 / sym_bytes.max(1) as f64,
        sym_bytes,
        file_bytes,
        median
    );

    Some(Setup {
        project: path.to_string(),
        symbol_id: target.id.clone(),
        search_query: target.name.clone(),
        search_query_split: split_identifier(&target.name),
        file_path,
    })
}

fn bench_search_symbols(c: &mut Criterion, setups: &[(&str, Setup)]) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("search_symbols");
    for (label, setup) in setups {
        group.bench_with_input(
            BenchmarkId::new("bm25", label),
            &setup.search_query,
            |b, query| {
                b.iter(|| {
                    rt.block_on(search_symbols(SearchSymbolsParams {
                        project: setup.project.clone(),
                        query: query.clone(),
                        kind: None,
                        language: None,
                        file: None,
                        limit: Some(20),
                        offset: None,
                        mode: Some("bm25".to_string()),
                        embed_config: None,
                    }))
                    .unwrap()
                })
            },
        );
    }
    group.finish();
}

fn bench_search_symbols_exact(c: &mut Criterion, setups: &[(&str, Setup)]) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("search_symbols");
    for (label, setup) in setups {
        group.bench_with_input(
            BenchmarkId::new("exact", label),
            &setup.search_query,
            |b, query| {
                b.iter(|| {
                    rt.block_on(search_symbols(SearchSymbolsParams {
                        project: setup.project.clone(),
                        query: query.clone(),
                        kind: None,
                        language: None,
                        file: None,
                        limit: Some(20),
                        offset: None,
                        mode: Some("exact".to_string()),
                        embed_config: None,
                    }))
                    .unwrap()
                })
            },
        );
    }
    group.finish();
}

fn bench_search_symbols_bm25_split(c: &mut Criterion, setups: &[(&str, Setup)]) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("search_symbols");
    for (label, setup) in setups {
        // Only meaningful when the name actually splits into multiple words.
        if !setup.search_query_split.contains(' ') {
            continue;
        }
        group.bench_with_input(
            BenchmarkId::new("bm25_split", label),
            &setup.search_query_split,
            |b, query| {
                b.iter(|| {
                    rt.block_on(search_symbols(SearchSymbolsParams {
                        project: setup.project.clone(),
                        query: query.clone(),
                        kind: None,
                        language: None,
                        file: None,
                        limit: Some(20),
                        offset: None,
                        mode: Some("bm25".to_string()),
                        embed_config: None,
                    }))
                    .unwrap()
                })
            },
        );
    }
    group.finish();
}

fn bench_search_symbols_fuzzy(c: &mut Criterion, setups: &[(&str, Setup)]) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("search_symbols");
    for (label, setup) in setups {
        group.bench_with_input(
            BenchmarkId::new("fuzzy", label),
            &setup.search_query,
            |b, query| {
                b.iter(|| {
                    rt.block_on(search_symbols(SearchSymbolsParams {
                        project: setup.project.clone(),
                        query: query.clone(),
                        kind: None,
                        language: None,
                        file: None,
                        limit: Some(20),
                        offset: None,
                        mode: Some("fuzzy".to_string()),
                        embed_config: None,
                    }))
                    .unwrap()
                })
            },
        );
    }
    group.finish();
}

fn bench_get_symbol(c: &mut Criterion, setups: &[(&str, Setup)]) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("get_symbol");
    for (label, setup) in setups {
        group.bench_with_input(BenchmarkId::new("get", label), &setup.symbol_id, |b, id| {
            b.iter(|| {
                rt.block_on(get_symbol(GetSymbolParams {
                    project: setup.project.clone(),
                    symbol_id: id.clone(),
                    include_context: None,
                    signature_only: None,
                }))
                .unwrap()
            })
        });
    }
    group.finish();
}

fn bench_get_file_outline(c: &mut Criterion, setups: &[(&str, Setup)]) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("get_file_outline");
    for (label, setup) in setups {
        group.bench_with_input(
            BenchmarkId::new("outline", label),
            &setup.file_path,
            |b, file_path| {
                b.iter(|| {
                    rt.block_on(get_file_outline(GetFileOutlineParams {
                        project: setup.project.clone(),
                        file_path: file_path.clone(),
                    }))
                    .unwrap()
                })
            },
        );
    }
    group.finish();
}

fn bench_get_project_outline(c: &mut Criterion, setups: &[(&str, Setup)]) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("get_project_outline");
    for (label, setup) in setups {
        group.bench_with_input(
            BenchmarkId::new("outline", label),
            &setup.project,
            |b, project| {
                b.iter(|| {
                    rt.block_on(get_project_outline(GetProjectOutlineParams {
                        project: project.clone(),
                        depth: Some(2),
                        path: None,
                        max_dirs: None,
                        summary: None,
                    }))
                    .unwrap()
                })
            },
        );
    }
    group.finish();
}

fn bench_find_usages(c: &mut Criterion, setups: &[(&str, Setup)]) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("find_usages");
    // find_usages traverses all project files via AST; needs more time per sample.
    group.measurement_time(Duration::from_secs(45));
    group.sample_size(10);
    for (label, setup) in setups {
        group.bench_with_input(
            BenchmarkId::new("find", label),
            &setup.symbol_id,
            |b, id| {
                b.iter(|| {
                    rt.block_on(find_usages(FindUsagesParams {
                        project: setup.project.clone(),
                        symbol_id: id.clone(),
                        scope: None,
                        limit: None,
                        offset: None,
                    }))
                    .unwrap()
                })
            },
        );
    }
    group.finish();
}

fn query_benchmarks(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    // Prepare all repos once, up front.
    let setups: Vec<(&str, Setup)> = REPOS
        .iter()
        .filter_map(|path| {
            let label = path.split('/').next_back().unwrap_or(path);
            prepare(path, &rt).map(|s| (label, s))
        })
        .collect();

    if setups.is_empty() {
        eprintln!("No repos available — run bench/setup.sh first");
        return;
    }

    bench_search_symbols(c, &setups);
    bench_search_symbols_exact(c, &setups);
    bench_search_symbols_fuzzy(c, &setups);
    bench_search_symbols_bm25_split(c, &setups);
    bench_get_symbol(c, &setups);
    bench_get_file_outline(c, &setups);
    bench_get_project_outline(c, &setups);
    bench_find_usages(c, &setups);
}

criterion_group!(benches, query_benchmarks);
criterion_main!(benches);
