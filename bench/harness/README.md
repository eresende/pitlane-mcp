# Benchmark Harness

`bench/harness` is the benchmark workspace for reproducible Pitlane vs baseline runs.

The canonical entrypoint is now:

```bash
python -m bench.harness.run
```

`bench_runner.py` remains as a compatibility wrapper for older local scripts, but new documentation and automation should target `bench.harness.run`.

## Current Scope

Patch Set 1 establishes the reliability base for the local harness:

- immutable `run_manifest.json` per benchmark invocation
- per-instance artifacts under `raw/<prompt_slug>/<mode>/run_<n>/`
- resumable runs with `--resume`
- explicit reruns with `--force`
- suite-manifest driven local runs

Patch Set 2 adds the execution/grading split:

- execution writes raw run artifacts
- grading writes `quality.json`, `results.csv`, and `claim_report.md`
- grading can be rerun independently from persisted `result.json` files

Patch Set 3 folds OpenCode onto the same canonical run contract:

- `local` and `opencode` both write `run_manifest.json`, `config.json`, per-instance `result.json`, and canonical `results.jsonl`
- grading works against persisted artifacts from either runtime
- `results.csv` and `claim_report.md` remain derived grading outputs, not execution outputs
- `--dry-run` leaves the run in execution-only state and skips grading

## Canonical Local Run

Use a suite manifest when the benchmark inputs are already defined:

```bash
python -m bench.harness.run \
  --suite bench/harness/suites/ripgrep-core-v1.json \
  --model qwen3:8b \
  --backend ollama \
  --out results/ripgrep-qwen3-1 \
  --max-iterations 1 \
  --runs 1 \
  --resume
```

Use ad hoc paths when you are iterating on prompts or a local repo checkout:

```bash
python -m bench.harness.run \
  --repo bench/repos/ripgrep \
  --prompts bench/harness/prompts.ripgrep.jsonl \
  --model qwen3:8b \
  --backend ollama \
  --out results/ripgrep-qwen3-adhoc \
  --max-iterations 1 \
  --runs 1
```

LM Studio is also supported as a local OpenAI-compatible backend:

```bash
LMSTUDIO_BASE_URL=http://127.0.0.1:1234/v1 \
python -m bench.harness.run \
  --repo bench/repos/ripgrep \
  --prompts bench/harness/prompts.ripgrep.jsonl \
  --model google/gemma-3-4b \
  --backend lmstudio \
  --out results/ripgrep-gemma3-lmstudio \
  --max-iterations 1 \
  --runs 1
```

To execute only and skip grading:

```bash
python -m bench.harness.run \
  --suite bench/harness/suites/ripgrep-core-v1.json \
  --model qwen3:8b \
  --backend ollama \
  --out results/ripgrep-qwen3-exec-only \
  --max-iterations 1 \
  --runs 1 \
  --skip-grade
```

To regrade an existing run directory:

```bash
python -m bench.harness.grade --run results/ripgrep-qwen3-exec-only
```

## Canonical OpenCode Run

Use the same entrypoint with `--runtime opencode` when you want to execute through OpenCode while keeping the same artifact layout and grading flow:

```bash
python -m bench.harness.run \
  --repo bench/repos/ripgrep \
  --prompts bench/harness/prompts.ripgrep.jsonl \
  --runtime opencode \
  --target with-mcp=http://127.0.0.1:4096 \
  --model openai/gpt-5.4-mini \
  --out results/ripgrep-opencode-1 \
  --runs 1
```

For a command rehearsal without execution:

```bash
python -m bench.harness.run \
  --repo bench/repos/ripgrep \
  --prompts bench/harness/prompts.ripgrep.jsonl \
  --runtime opencode \
  --target with-mcp=http://127.0.0.1:4096 \
  --model openai/gpt-5.4-mini \
  --out results/ripgrep-opencode-dry-run \
  --runs 1 \
  --dry-run
```

For a Bedrock-backed OpenCode smoke test using the checked-in config targets:

```bash
AWS_PROFILE=your-profile \
AWS_REGION=us-east-1 \
python -m bench.harness.run \
  --repo bench/repos/ripgrep \
  --prompts bench/harness/prompts.ripgrep.jsonl \
  --runtime opencode \
  --target with-mcp=CONFIG:bench/harness/sample.opencode.bedrock.with-mcp.json \
  --target no-mcp=CONFIG:bench/harness/sample.opencode.bedrock.no-mcp.json \
  --model amazon-bedrock/global.anthropic.claude-sonnet-4-5-20250929-v1:0 \
  --out results/ripgrep-opencode-bedrock-sonnet45-smoke \
  --max-iterations 15 \
  --runs 1 \
  --prompt-id symbol_regex_search_path \
  --prompt-id symbol_ignore_logic \
  --prompt-id symbol_cli_config_flow
```

This Bedrock path is currently the strongest validated OpenCode benchmark setup in this repo:

- `with-mcp` and `no-mcp` can be compared under the same provider/model settings
- OpenCode now preserves tool calls and multi-step assistant turns in canonical artifacts
- the sample Bedrock configs keep `pitlane-mcp` local and forward local embedding settings into the MCP process

The harness also injects a benchmark system prompt that is stricter in `with-mcp` mode:

- prefer one pitlane discovery/navigation call followed by a small number of focused reads
- avoid mixing `search_symbols`, `search_files`, and `search_content` for the same question unless the earlier call clearly failed
- keep architecture prompts to one orientation step plus a few focused follow-ups
- stop once there are 2 to 4 concrete files or symbols that answer the prompt

## Important Flags

- `--suite` selects a suite manifest from `bench/harness/suites/`
- `--repo` and `--prompts` override suite inputs or define an ad hoc run
- `--resume` skips instances that already have `result.json`
- `--force` reruns instances even when artifacts already exist
- `--prompt-id` can be repeated to run only selected prompt ids
- `--mode` chooses `mcp`, `baseline`, or `both`
- `--runtime` chooses `local` or `opencode`
- `--backend` chooses `ollama`, `openrouter`, or `lmstudio`
- `--skip-grade` stops after execution and leaves grading for a later `bench.harness.grade` call
- `--dry-run` is mainly useful with `--runtime opencode`; it writes execution artifacts but skips grading
- `--target` can be repeated for OpenCode target backends such as `with-mcp=http://127.0.0.1:4096`
- `LMSTUDIO_BASE_URL` overrides the default local LM Studio endpoint (`http://127.0.0.1:1234/v1`)
- `LMSTUDIO_COOLDOWN_SECONDS` controls the delay between LM Studio requests (default: `2.0`) to reduce stuck-processing issues seen with some models

## Output Layout

Each benchmark run now writes:

- `run_manifest.json` — immutable run identity and pinned inputs
- `config.json` — execution config and environment metadata
- `results.jsonl` — aggregate run results rebuilt from per-instance artifacts
- `results.csv` — derived flattened summary rows written by grading
- `claim_report.md` — derived markdown summary written by grading

Each benchmark instance writes:

- `raw/<prompt_slug>/<mode>/run_<n>/result.json`
- `raw/<prompt_slug>/<mode>/run_<n>/conversation.json`
- `raw/<prompt_slug>/<mode>/run_<n>/tool_calls.json`
- `raw/<prompt_slug>/<mode>/run_<n>/quality.json` after grading

`result.json` is the execution artifact and authoritative resume boundary. `quality.json`, `results.csv`, and `claim_report.md` are grading artifacts that can be regenerated later.

## Suite Manifests

The first suite manifest lives at:

- `bench/harness/suites/ripgrep-core-v1.json`

It pins:

- suite id
- repo path
- prompt set path
- default runs
- default max iterations
- default timeout
- scorer version

Add new suites under `bench/harness/suites/` instead of encoding benchmark semantics only in shell commands.

## OpenCode and AWS

These compatibility paths still exist, but `bench.harness.run` is now the system of record:

- `bench_opencode.py`
- `aws_bench.sh`

Use them only when you need a thin wrapper around the canonical runner or an external orchestration shell. The current refactor plan is documented in:

- `bench/harness/ROADMAP.md`

## OpenCode Notes

OpenCode benchmarking now writes the same canonical artifacts as the local runtime, but attached-server mode is still less reproducible than the local agentic runner because session state and target repo alignment are easier to get wrong.

The current OpenCode helper files are:

- `bench_opencode.py`
- `sample.opencode.with-mcp.json`
- `sample.opencode.no-mcp.json`
- `sample.opencode.bedrock.with-mcp.json`
- `sample.opencode.bedrock.no-mcp.json`

If you need that flow, keep both targets pinned to the same provider/model settings and treat it as an experiment, not the official benchmark path.
