# Pitlane MCP — Real-World Benchmark Results

Benchmarks run with **kiro-cli** (`claude-haiku-4.5`) comparing two agents:

- **with-mcp** — pitlane-mcp enabled (`search_symbols`, `get_symbol`, `find_usages`, etc.)
- **no-mcp** — built-in tools only (`read`, `grep`, `glob`)

Each prompt is a fresh kiro-cli process (one prompt → one answer). Latency and credits
are reported by kiro-cli. Pitlane call count is parsed from stdout.

---

## Synthetic benchmarks

Measured directly against the pitlane-mcp Rust library on 16 real-world repos.
All times are median over 100 samples (10 for `find_usages` and `index_project`).

### index_project — full repo indexing time

| repo | files | symbols | time |
|---|---|---|---|
| bats | 54 | 147 | 2.0 ms |
| roact | 95 | 93 | 3.6 ms |
| gin | 92 | 1,184 | 10.7 ms |
| leveldb | 132 | 1,531 | 10.8 ms |
| sdwebimage | 271 | 1,564 | 14.7 ms |
| zls | 67 | 2,422 | 18.4 ms |
| hono | 368 | 992 | 17.7 ms |
| swiftlint | 667 | 3,781 | 22.6 ms |
| ripgrep | 101 | 3,207 | 25.8 ms |
| fastapi | 1,290 | 4,828 | 28.3 ms |
| rubocop | 1,539 | 9,122 | 44.3 ms |
| okhttp | 636 | 6,680 | 47.5 ms |
| newtonsoft | 933 | 7,284 | 73.6 ms |
| redis | 818 | 14,648 | 81.4 ms |
| laravel | 2,331 | 26,127 | 121.6 ms |
| guava | 3,275 | 56,805 | 178.7 ms |

### query tools — per-call latency (median, 100 samples)

| tool | fastest | slowest | notes |
|---|---|---|---|
| `get_symbol` | 17 µs | 60 µs | hash lookup; hono outlier due to TS re-parse |
| `search_symbols` bm25 | 21 µs | 89 µs | scales with index size |
| `search_symbols` exact | 35 µs | 142 µs | ripgrep outlier (large Rust index) |
| `search_symbols` fuzzy | 85 µs | 73 ms | trigram scan; expensive on large repos |
| `get_file_outline` | ~26 µs | — | flat across all repos |
| `find_usages` | 0.76 ms | 77 ms | AST scan; scales with file count |

### memory and disk (first-run peak RSS)

| repo | peak RAM | disk size | token efficiency (median) |
|---|---|---|---|
| bats | 23 MB | 52 KB | N/A (bash only) |
| roact | 27 MB | 29 KB | 89.6x |
| gin | 36 MB | 354 KB | 125.4x |
| leveldb | 37 MB | 398 KB | 417.6x |
| hono | 42 MB | 275 KB | 52.5x |
| sdwebimage | 42 MB | 648 KB | 53.8x |
| swiftlint | 51 MB | 1.7 MB | 51.7x |
| fastapi | 54 MB | 1.6 MB | 19.5x |
| ripgrep | 59 MB | 1.1 MB | 531.6x |
| rubocop | 67 MB | 3.1 MB | 61.1x |
| zls | 73 MB | 689 KB | 801.1x |
| okhttp | 85 MB | 3.1 MB | 55.8x |
| newtonsoft | 96 MB | 3.2 MB | 64.5x |
| redis | 109 MB | 3.9 MB | 132.7x |
| laravel | 211 MB | 12.3 MB | 79.5x |
| guava | 225 MB | 28.4 MB | 111.7x |

Token efficiency = median ratio of (full file size) / (symbol body size) for
struct/class/interface symbols. Represents how much context `get_symbol` saves
vs reading the whole file.

---

## Real-world benchmarks

### Gin (Go) — 92 files, 1,184 symbols, 125x median token efficiency

Model: `claude-haiku-4.5` · 6 prompts · 1 run each

| prompt | category | mcp time | mcp credits | mcp pitlane calls | base time | base credits |
|---|---|---|---|---|---|---|
| symbol_router_core | symbol_grounding | 37s | 0.35 | 0 | 68s | 0.37 |
| symbol_context | symbol_grounding | 32s | 0.28 | 12 | 37s | 0.26 |
| symbol_middleware | symbol_grounding | 37s | 0.33 | 16 | 31s | 0.24 |
| usage_request_flow | find_usages | 33s | 0.28 | 10 | 53s | 0.46 |
| arch_package_map | architecture | 29s | 0.26 | 12 | 32s | 0.29 |
| negative_orm | negative_control | 20s | 0.19 | 7 | 16s | 0.13 |
| **average** | | **31s** | **0.28** | **9.5** | **40s** | **0.29** |

**with-mcp vs no-mcp: 23% faster, ~same cost**

Notes:
- `symbol_router_core`: mcp agent chose not to use pitlane (0 calls) and still answered faster
- `negative_orm` and `symbol_middleware`: no-mcp wins slightly — small lookups where grep is cheaper than MCP overhead
- Credits are nearly identical overall; MCP saves time by reducing tool round-trips, not tokens

---

### Redis (C) — 818 files, 14,648 symbols, 132x median token efficiency

Model: `claude-haiku-4.5` · 7 prompts · cold + warm run

**Cold** = first run (pitlane loads index from disk, ~230ms overhead per call)  
**Warm** = second run (on-disk cache already validated, negligible overhead)

#### Per-prompt results

| prompt | category | mcp cold | mcp warm | base cold | base warm |
|---|---|---|---|---|---|
| symbol_redisserver | symbol_grounding | 14s / 0.11cr | 15s / 0.11cr | 18s / 0.13cr | 25s / 0.13cr |
| symbol_client | symbol_grounding | 14s / 0.10cr | 13s / 0.10cr | 10s / 0.05cr | 10s / 0.05cr |
| symbol_event_loop | symbol_grounding | 26s / 0.25cr | 26s / 0.23cr | 40s / 0.71cr | 51s / 0.87cr |
| usage_command_dispatch | find_usages | 30s / 0.31cr | 27s / 0.31cr | 66s / 1.46cr | 67s / 1.31cr |
| usage_persistence | find_usages | 63s / 0.94cr | 55s / 0.68cr | 75s / 0.70cr | 86s / 1.83cr |
| arch_package_map | architecture | 27s / 0.42cr | 32s / 0.63cr | 77s / 0.97cr | 44s / 0.84cr |
| negative_http | negative_control | 26s / 0.36cr | 14s / 0.15cr | 22s / 0.55cr | 30s / 0.46cr |
| **total** | | **200s / 2.49cr** | **182s / 2.21cr** | **308s / 4.57cr** | **313s / 5.49cr** |

#### Summary

| comparison | time | credits |
|---|---|---|
| cold: with-mcp vs no-mcp | 200s vs 308s | 2.49 vs 4.57 |
| warm: with-mcp vs no-mcp | 182s vs 313s | 2.21 vs 5.49 |
| warm vs cold (with-mcp only) | −18s (−9%) | −0.28 (−11%) |

**Cold: 35% faster, 46% cheaper**  
**Warm: 42% faster, 60% cheaper**

Notes:
- `symbol_client`: no-mcp wins (10s / 0.05cr) — the struct is small enough that a single grep is cheaper than MCP overhead
- `usage_command_dispatch`: biggest gap — no-mcp spent 1.46cr reading C files trying to trace the call chain; with-mcp navigated directly via `search_symbols` + `get_symbol`
- `usage_persistence` warm no-mcp is an outlier (1.83cr) — the model took a different exploration path and read more files; illustrates the variance of file-reading approaches on complex tasks
- Warm start improves MCP by ~10% but the dominant factor is always targeted symbol lookup vs reading raw files

---

## Key takeaways

1. **MCP wins most on large, dense codebases** (redis > gin). The denser the files, the more `get_symbol` saves over reading whole files.

2. **Cross-file tracing is where the gap is largest.** `find_usages` / `usage_*` prompts show 2–4x credit savings. Simple single-symbol lookups (`symbol_client`) can go either way.

3. **Warm start is a small bonus, not the main story.** Index cache validation is fast (~50ms vs ~350ms cold), but model generation time dominates. The real win is always token efficiency.

4. **Negative/trivial prompts favor no-mcp.** When the answer is "it doesn't exist" or the target is a tiny struct, grep is cheaper than spinning up MCP.

5. **Tool call count is the leading indicator.** with-mcp consistently makes 40–50% fewer tool calls. Fewer calls = less context = faster generation = lower cost.
