# Pitlane Benchmark Follow-Up TODO

This document captures the concrete follow-up work surfaced by the recent
Bedrock/OpenCode benchmark runs against ripgrep.

The important point is that the harness is now measuring the current Pitlane
behavior correctly. The remaining problems are mostly in Pitlane tool ergonomics
and model steering, not in startup/config wiring.

## Current Findings

### Confirmed fixes

- [x] OpenCode config-path bug fixed.
  - `OPENCODE_CONFIG` is now resolved to an absolute path before launch.
  - The benchmark harness now talks to the intended workspace `pitlane-mcp`
    binary instead of silently falling back to another config.

- [x] `read_code_unit` file-outline compaction added.
  - Large file-level outline reads are now capped to a small set of stronger
    symbols.
  - This reduced `with-mcp` context bytes significantly in the smoke reruns.

### What the benchmark now shows clearly

- [ ] `with-mcp` is still inconsistent across prompt types.
  - It helps on execution-path prompts such as `symbol_regex_search_path`.
  - It still regresses on subsystem/symbol-grounding prompts such as
    `symbol_cli_config_flow` and `symbol_ignore_logic`.

- [ ] Quality is flat while efficiency moves around.
  - Current smoke slices keep scoring `1.0` quality for both `with-mcp` and
    `no-mcp`.
  - This means the next improvements must focus on reducing tool thrash, token
    usage, and broad reads, not just preserving answer correctness.

- [ ] The main remaining issue is agent-loop ergonomics.
  - The model still escapes into generic `read`, `glob`, and sometimes `bash`
    even when Pitlane has already identified the relevant subsystem.
  - Stricter harness prompt guidance alone did not solve this reliably.

## Product Work

### 1. Improve `locate_code` for vague symbol/subsystem queries

- [ ] Add query normalization for common vague patterns observed in real runs.
  - Examples:
    - `main function`
    - `entry point`
    - `args clap`
    - `directory traversal walker`
    - `printer print results`
    - `ignore handling`

- [ ] Rewrite weak natural-language discovery queries into better server-side
  symbol/file discovery intents before routing.
  - Example:
    - `main function` in a CLI repo should bias toward entrypoint files and
      exported `main`/`run` symbols.
    - `args clap` should bias toward config/flags parsing files instead of
      literal text search.

- [ ] Use repo-role priors more aggressively in `locate_code`.
  - Promote `entrypoint`, `cli`, `config`, `handler`, `service`, and
    `bootstrap` roles when the query matches those concepts implicitly.

- [ ] Return better query-sharpening guidance on weak `locate_code` results.
  - Instead of generic fallback advice, suggest one concrete sharper query.
  - Example:
    - `Try HiArgs / LowArgs / ParseResult in crates/core/flags`
    - `Try WalkBuilder / Ignore::matched in crates/ignore`

- [ ] Make `locate_code` more willing to answer with one strong candidate plus a
  clearer next step instead of weak low-confidence dumps.

### 2. Improve `trace_path` / path-first behavior

- [ ] Make `trace_path` more useful for CLI/config flow prompts.
  - Current traces can still return empty or weak chains for configuration-flow
    questions.
  - Add better seed selection for parse/config/orchestration terms.

- [ ] Bias `trace_path` toward compact path narratives that can replace multiple
  follow-up discovery calls.

- [ ] Surface stronger evidence when a path seed is weak.
  - If no call chain is found, explicitly point the model to the likely entry
    file or config subsystem instead of just recommending another search.

### 3. Tighten `read_code_unit` further

- [ ] Keep the current file-outline compaction.

- [ ] Add a second-level cap for very large symbol bodies returned by
  `read_code_unit(symbol_id=...)`.
  - Some method/function reads are still large enough to trigger follow-up
    branching.

- [ ] Consider returning a lighter default for container-heavy files.
  - Example:
    - show top-level declarations plus the first few relevant methods
    - require narrower follow-up reads for the rest

- [ ] Add a server-side hint when a file outline was compacted.
  - Tell the model how to narrow within the same file using `locate_code`
    without escaping to generic reads.

### 4. Reduce generic-tool escape conditions

- [ ] Use Pitlane tool responses to more explicitly discourage generic reads.
  - If `locate_code`, `trace_path`, or `get_index_stats` already found the
    subsystem, the next-step guidance should explicitly say not to glob/list the
    directory.

- [ ] Consider adding stronger `recommended_target` payloads in weak-but-useful
  cases.
  - Example:
    - direct a model from `get_index_stats` into `crates/core/main.rs`
    - direct a model from a weak `locate_code` into a specific file path and
      narrower symbol query

- [ ] Review whether `read_code_unit(file_path=directory-ish string)` should be
  rejected or normalized more strictly.
  - One benchmark trace showed `read_code_unit(file_path="crates/core/flags")`,
    which is not the intended usage pattern.

## Harness Work

### 5. Keep prompt guidance pragmatic, not over-constrained

- [ ] Keep the useful prompt guidance that improves path-style prompts.

- [ ] Avoid continuing to ratchet up prompt strictness as the primary fix.
  - Recent runs showed that more prompt rules can make the model spend extra
    turns trying to comply before escaping anyway.

- [ ] Treat harness steering as a multiplier for a good tool layer, not a
  substitute for product-side discovery quality.

### 6. Add benchmark checks for tool-mix regressions

- [ ] Track non-Pitlane tool escapes in `with-mcp` runs.
  - Count `read`, `glob`, `bash`, and similar generic tools separately.

- [ ] Add a derived metric for:
  - generic-tool calls per `with-mcp` run
  - percent of tool calls that are Pitlane tools
  - first generic-tool escape iteration

- [ ] Use those metrics to catch regressions even when answer quality remains
  flat at `1.0`.

- [ ] Store a compact per-run tool-mix summary in derived outputs.

### 7. Build a focused prompt slice for Pitlane iteration

- [ ] Keep using the current ripgrep slice, but classify prompts by why Pitlane
  should win:
  - execution path
  - subsystem location
  - config flow
  - exclusion/ignore mapping

- [ ] Create a small “Pitlane must win here” slice for local iteration.
  - Start with prompts where graph/native discovery should outperform broad
    reading:
    - regex execution path
    - CLI/config flow
    - ignore-rule skip path
    - JSON output path

- [ ] Evaluate tool behavior, not just final answer text, on that slice.

## Immediate Next Steps

- [ ] Improve `locate_code` query normalization and reranking for vague
  subsystem queries.
- [ ] Re-run the same ripgrep shallow smoke slice after that product change.
- [ ] Compare against:
  - `...post-config-fix-smoke-20260419`
  - `...post-config-fix-smoke-20260419-1723`
  - `...post-prompt-tightening-smoke-20260419`
- [ ] If `with-mcp` still loses on symbol/subsystem prompts, inspect whether the
  remaining issue is:
  - weak discovery routing
  - large symbol reads
  - or insufficient graph/path hints

