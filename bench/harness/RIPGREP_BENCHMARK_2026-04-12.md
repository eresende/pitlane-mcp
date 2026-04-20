# Ripgrep Benchmark Summary — 2026-04-12

This note summarizes the latest `bench/harness` comparison on the ripgrep prompt set after the claim-report and executor fixes landed.

## Setup

- Prompt set: `bench/harness/prompts/ripgrep.jsonl`
- Prompt set SHA-256: `d0ff8ebe7022a4770feef22a178a52bf8baf5c951c544f05b7873702df8f5216`
- Prompt count: `19`
- Runs per prompt: `3`
- Max iterations: `25`
- Timeout: `300s`
- Target repo commit: `4649aa9700619f94cf9c66876e9549d83420e16c`

Compared runs:

- Local run
  - Model: `qwen3:8b`
  - Hardware: AMD Radeon RX 6800 XT, Ryzen 9 9950X, 30 GB RAM
- AWS run
  - Model: `qwen3:14b`
  - Hardware: NVIDIA A10G, AMD EPYC 7R32, 15 GB RAM

## System Specs

| Environment | GPU | CPU | RAM |
|---|---|---|---|
| Local | AMD Radeon RX 6800 XT 16 GB | AMD Ryzen 9 9950X 16-Core Processor | 30.2 GB |
| AWS | NVIDIA A10G 22 GB | AMD EPYC 7R32 | 15.4 GB |

## Aggregate Results

| Environment | Model | MCP Quality | Baseline Quality | MCP Lift | Avg MCP Time | Avg MCP Tokens |
|---|---|---:|---:|---:|---:|---:|
| Local | `qwen3:8b` | `0.326` | `0.115` | `+0.211` | `51.5s` | `21.2k` |
| AWS | `qwen3:14b` | `0.277` | `0.143` | `+0.134` | `90.1s` | `27.7k` |

## Main Takeaways

- MCP improved quality over the non-MCP baseline in both environments.
- The local run showed the larger MCP gain: `+0.211` vs `+0.134` on AWS.
- In this benchmark, the AWS `qwen3:14b` run did not outperform the local `qwen3:8b` run overall.
- MCP gains were strongest on repository-grounded navigation and tracing tasks, but not uniform across every prompt.

## Notable Prompt-Level Differences

AWS MCP was clearly stronger on:

- `graph_nav_call_chain`: `0.600` vs local `0.095`
- `symbol_regex_search_path`: `0.333` vs local `0.000`
- `token_efficiency_probe`: `0.296` vs local `0.000`
- `semantic_search_probe`: `0.194` vs local `0.000`

Local MCP was clearly stronger on:

- `usage_json_output`: `0.933` vs AWS `0.306`
- `smart_exclusions_probe`: `0.611` vs AWS `0.146`
- `symbol_cli_config_flow`: `0.339` vs AWS `0.032`
- `tests_ignore_behavior`: `0.583` vs AWS `0.314`
- `negative_http_server`: `0.333` vs AWS `0.000`

## Claim Report Snapshot

Local claim report:

- `bm25_search_quality`: `0.45`
- `graph_navigation`: `0.28`
- `smart_exclusions`: `0.61`

AWS claim report:

- `bm25_search_quality`: `0.34`
- `graph_navigation`: `0.39`
- `smart_exclusions`: `0.15`

## Caveats

- The local run had one infrastructure failure: `fully_local_probe` MCP run 1 failed because Ollama was temporarily unreachable. That slightly understates the local aggregate.
- The AWS run still recorded `harness_commit` as `null`, so harness provenance is weaker there than on the local run.
- These results are from the ripgrep benchmark only. They are strong evidence for this workflow, but not a universal claim across all repositories.

## Artifact Policy

This summary is intended to be committed to the repository. The full benchmark output directories (`results.csv`, `results.jsonl`, and `raw/` conversation traces) are better treated as local or CI artifacts instead of version-controlled documentation.
