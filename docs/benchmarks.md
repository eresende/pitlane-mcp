# Benchmarks

This repo includes benchmark data and a reproducible harness, but benchmark numbers should be read as release-specific snapshots, not universal guarantees.

Real-world outcomes vary with:

- host agent behavior
- prompt strategy
- model choice
- tool-calling discipline
- repository shape

## What The Benchmarks Measure

The benchmark material in this repo focuses on:

- token efficiency versus broader file reads
- navigation quality on grounded code questions
- latency and indexing throughput
- harness reproducibility for repeated runs

## Current Framing

The benchmark corpora in this repo show that symbol-level navigation can substantially reduce prompt footprint and often improve answer quality, but the exact win rate depends on the harness and model.

That is why the project should present benchmark results as:

- evidence that the navigation approach is effective
- release snapshots
- representative measurements, not guarantees

## Where To Look

- Benchmark harness usage: [../bench/harness/README.md](../bench/harness/README.md)
- Harness roadmap: [../bench/harness/ROADMAP.md](../bench/harness/ROADMAP.md)
- Repo-specific follow-up notes: [../bench/harness/TODO.md](../bench/harness/TODO.md)

## Recommended README Posture

For the top-level README:

- keep one short benchmark summary
- avoid turning the landing page into a methodology dump
- link to deeper benchmark material from here
