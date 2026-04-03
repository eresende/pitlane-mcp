/// Benchmarks for `Indexer::index_project` on real-world open-source projects.
///
/// Prerequisites: run `bench/setup.sh` first to clone the benchmark repositories.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode};
use pitlane_mcp::indexer::{registry::build_default_registry, Indexer};
use std::{path::Path, time::Duration};

const REPOS: &[(&str, &str)] = &[
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
    ("laravel", "bench/repos/laravel"),
    ("zls", "bench/repos/zls"),
];

fn bench_index_project(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_project");
    // Indexing a full repo is slow; use flat sampling with a small sample size.
    group.measurement_time(Duration::from_secs(60));
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);

    for (name, path) in REPOS {
        let repo = Path::new(path);
        if !repo.exists() {
            eprintln!("Skipping '{name}': {path} not found — run bench/setup.sh first");
            continue;
        }

        let indexer = Indexer::new(build_default_registry());
        let excludes: Vec<String> = vec![];

        // Print index stats once (outside the timed loop).
        match indexer.index_project(repo, &excludes) {
            Ok((idx, files)) => println!("[{name}] {files} files, {} symbols", idx.symbol_count()),
            Err(e) => {
                eprintln!("Failed to index {name}: {e}");
                continue;
            }
        }

        group.bench_with_input(
            BenchmarkId::new("index", name),
            &(repo, &excludes),
            |b, (repo, excludes)| {
                b.iter(|| indexer.index_project(repo, excludes).unwrap());
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_index_project);
criterion_main!(benches);
