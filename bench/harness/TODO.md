# Pitlane Benchmark Follow-Up TODO

This document captures the state of Pitlane benchmark optimization work.

## Summary of Results

Full 19-prompt run on GLM 4.7 Flash (Bedrock) with n=1:

- **Token ratio: 0.74x** — MCP uses 26% fewer tokens than baseline
- **Quality: 0.87 MCP vs 0.81 baseline** — MCP produces better answers
- **MCP wins on tokens: 12/19 prompts**
- **Pitlane tool adoption: 43-100%** depending on prompt

Best results on Sonnet 4.5 (single-prompt):
- `symbol_ignore_logic`: **0.43x** (57% cheaper), quality 1.0 vs 0.62 baseline
- `symbol_cli_config_flow`: **0.64x** (36% cheaper), quality 1.0

## Completed Work

### Tool Layer (Rust)

- [x] Query normalization in `locate_code` for vague patterns
- [x] Semantic-to-BM25 fallback when embedding server is unavailable
- [x] `looks_like_text_snippet` fix — no longer misroutes concept queries
- [x] Symbol body size cap (120 lines) in `read_code_unit`
- [x] Directory-path rejection in `read_code_unit`
- [x] References made opt-in in `get_symbol` (saves ~2.4KB/call)
- [x] `fallback_candidates` removed from steering (saves ~1-2KB/call)
- [x] Prose summaries in `locate_code`, `trace_path`, and file outlines
- [x] `recommended_action` field with exact tool calls
- [x] Struct signature responses include `members` list
- [x] `trace_path` seed discovery broadened (all symbol kinds, per-term BM25)
- [x] `investigate` composite tool — single-call code question answering
- [x] Session-aware duplicate detection in `investigate`
- [x] Strong stop signal in `investigate` responses

### Harness (Python)

- [x] Tool-mix metrics (`tool_mix.py`) with CSV columns and reports
- [x] Subprocess timeout in opencode runtime (prevents Bedrock throttle hangs)
- [x] Config files reorganized into `bench/harness/configs/`
- [x] Prompt files reorganized into `bench/harness/prompts/`
- [x] GLM 4.7 Flash set as default benchmark model
- [x] Sonnet 4.5 configs preserved as premium option

### Steering (AGENTS.md)

- [x] Updated to reference public-tier tools only
- [x] Explicit DO/DO NOT rules for tool selection
- [x] `investigate` positioned as first-call tool
- [x] Strong anti-bash/grep/glob/read language

## Remaining Work

### High Impact (would move the needle)

- [ ] Fix the 7 prompts where MCP is still more expensive than baseline.
  - `tests_ignore_behavior` (1.32x) — model calls locate_code 10+ times
  - `tests_hidden_files` (1.01x) — nearly at parity, minor over-exploration
  - `usage_cli_to_search` (1.40x) — model explores too broadly
  - `arch_search_subsystem` (2.84x) — low Pitlane adoption on some runs
  - `fully_local_probe` (3.02x) — model answers incorrectly with MCP
  - `negative_search_session` (2.35x) — over-explores for negative answer
  - `token_efficiency_probe` (1.60x) — model reads too many symbols

- [ ] Improve `investigate` to return more targeted results for test-related
  prompts (tests_hidden_files, tests_ignore_behavior).
  - These prompts ask about test behavior, not implementation — investigate
    finds implementation symbols but the model needs test files.

- [ ] Add a `max_calls` or iteration budget hint in the MCP tool descriptions
  so models know when to stop exploring.

### Medium Impact

- [ ] Run the full 19-prompt suite with n=3 on GLM Flash to get stable averages.
  - Current results are n=1 which has high variance.

- [ ] Make `trace_path` more useful for CLI/config flow prompts.
  - Seed selection still weak for configuration-flow questions.

- [ ] Evaluate whether `investigate` should also search test files when the
  query mentions "test" or "behavior".

### Low Impact / Deferred

- [ ] Consider returning a lighter default for container-heavy files in
  `read_code_unit`.

- [ ] Build a focused "Pitlane must win" prompt slice for rapid iteration.

- [ ] Evaluate tool behavior metrics (not just final answer) per prompt category.

## Benchmark Commands

Default (GLM 4.7 Flash, cheap):
```bash
python -m bench.harness.run \
  --repo bench/repos/ripgrep \
  --prompts bench/harness/prompts/ripgrep.jsonl \
  --runtime opencode \
  --target with-mcp=CONFIG:bench/harness/configs/bedrock.glm-flash.with-mcp.json \
  --target no-mcp=CONFIG:bench/harness/configs/bedrock.glm-flash.no-mcp.json \
  --model amazon-bedrock/zai.glm-4.7-flash \
  --out results/ripgrep-glm-flash-$(date +%Y%m%d-%H%M) \
  --max-iterations 15 \
  --runs 1 \
  --resume
```

Premium (Sonnet 4.5, best quality):
```bash
python -m bench.harness.run \
  --repo bench/repos/ripgrep \
  --prompts bench/harness/prompts/ripgrep.jsonl \
  --runtime opencode \
  --target with-mcp=CONFIG:bench/harness/configs/bedrock.sonnet45.with-mcp.json \
  --target no-mcp=CONFIG:bench/harness/configs/bedrock.sonnet45.no-mcp.json \
  --model amazon-bedrock/global.anthropic.claude-sonnet-4-5-20250929-v1:0 \
  --out results/ripgrep-sonnet45-$(date +%Y%m%d-%H%M) \
  --max-iterations 15 \
  --runs 1 \
  --resume
```
