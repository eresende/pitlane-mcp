# Navigation skill experiment

This opt-in experiment measures whether an indexed-tool MCP server, and a navigation
skill on top of it, help an agent answer repository questions compared with plain
file tools. It reuses `AgenticLoop`, `BaselineExecutor`, the MCP stdio transport, the
existing ripgrep prompts and `QualityScorer`. Existing `bench.harness.run` commands
and artifact schemas are unchanged.

## Treatments and isolation

`../configs/navigation-skill.json` defines:

| Arm | Native read/search/list | MCP default-tier tools | Navigation skill |
|---|---|---|---|
| A | yes | unavailable; no server process | absent |
| B | yes | yes | absent |
| C | yes | yes, identical to B | exact SKILL.md in system prompt |

A/B receive byte-identical neutral system prompts. C appends the entire skill,
including front matter, before the first model request. All arms receive identical
task prompts, including the prompt suffix. No legacy MCP-specific or task-specific
benchmark guidance is injected. Server tool descriptions are part of the MCP
treatment in B and C, so they may reduce the marginal effect of the skill.

Every attempt uses a fresh conversation, tool executor and (B/C) MCP process, and
the model only sees tools restricted to a tracked-HEAD snapshot. Untracked files,
`.git`, agent configuration and instruction files are absent; escaping or broken
symlinks are rejected, and native and MCP paths cannot escape the snapshot. The MCP
process gets its own HOME/cache and receives only PATH plus explicit
`PITLANE_EMBED_*` settings. A separate preparation process indexes the snapshot, so
cold setup and embedding time are excluded from navigation time.

## Provider

Runs use a hosted OpenAI-compatible endpoint; no local accelerator is involved.
`base_url` and `api_key_env` live in the config, so the CLI needs no endpoint
flags. The provider must support tool calling and report
`prompt_tokens`/`completion_tokens`/`total_tokens`; the preflight below enforces
both. There are no automatic HTTP retries: a 429 or 5xx fails that run and stops
the pilot fail-fast rather than being silently retried. The harness never logs
credentials, and a gateway that wraps the payload as `{"data": {...}}` is accepted.

The endpoint advertises no context length, so `context_window` is the model family's
documented ceiling rather than an observed one. It still gates every request through
`prompt_tokens + max_output_tokens <= context_window`, and the requests this suite
produces stay far below it: the largest prompt observed is 116,857 tokens. The output
allowance is 16,384 because a synthesis turn was observed to truncate at 8,192.

## Setup and preflight

```sh
export CLINEPASS_API_KEY=...      # or pass --env-file pointing at a private file
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase preflight --capture-traces --out results/skill-preflight
```

`--api-key-env` and `--env-file` select where the key comes from; the dotenv reader
reads one literal value, never executes the file and never logs the key. Preflight
checks model discovery plus an actual structured call with JSON arguments and a
follow-up request that consumes the tool result. Passing once is necessary but
insufficient evidence of reliable task tool calling.

Model, temperature, output allowance, context declaration, reasoning policy,
iteration budget, deadline and environment declarations live in the JSON config.
Sampling or reasoning request fields belong in `request_options`; reserved request
fields cannot be overridden. The deadline is a safety net sized above the iteration
budget (25 requests at the observed per-request cost), not the primary control.

## Smoke first, then review

```sh
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase smoke --capture-traces --out results/skill-smoke
.venv/bin/python -m pytest bench/harness/tests -m 'not integration' -q
```

Smoke executes exactly three task runs, one per arm, using `smoke_prompt_id`. It
stops after its report, and `summary.json.smoke_validation.technical_passed` is the
execution gate. Checks cover valid tool use, MCP use in B/C, no MCP in A, identical
A/B base prompts, identical B/C schemas, skill placement, provider usage and
accounting, session validity, context budget and repository/input integrity.
`correctness_review_passed` stays false until reviews exist.

Correctly rejected tool arguments and semantic/native tool errors the agent recovers
from are recorded observations, not technical failures, provided at least one call
succeeded and no protocol or transport failure occurred. Truncated output, empty or
unstructured tool responses and missing protocol identifiers are failures; reasoning
text never substitutes for an answer.

Review each `instances/<task>/<arm>/run_1/result.json` final answer against the
snapshot and fill `reviews.json`:

- `1`: complete and correct; every requested component and relationship verified.
- `0.5`: partial, even if the stated references exist.
- `0`: incorrect or unsupported.

Also annotate appropriate MCP selection, unnecessary MCP calls and redundant
repository searches where the review supports these judgments, leaving unavailable
metrics null. Grounding precision alone must not certify success. Regenerate reports
without model calls:

```sh
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase report --out results/skill-smoke
```

## Fifteen-run pilot

```sh
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase pilot --validated-smoke results/skill-smoke \
  --capture-traces --out results/skill-pilot
```

The pilot uses exactly five `pilot_tasks`, once each under A/B/C, with fresh
sessions and a seeded rotating arm order. Admission reads a prior smoke's technical
checks and pinned controls: model, repository, prompts, skill, endpoint, embeddings,
MCP binary, execution sources (including the prompt-building controller) and tool
schemas must still match, and `--capture-traces` is required for the wire audit.

The evaluator-only [task-quality-v1 rubric](TASK_QUALITY_RUBRIC_V1.md) is frozen and
copied/hashed before inference. It separates required components, essential
relationships, task-affecting errors, incidental errors and unsupported symbols.
Task success and strict success are separate fields and separate efficiency subsets;
no historical result is silently rescored.

The pilot stops on a failed final termination, provider/protocol or tool transport
failure, changed source/model/controls/schema, isolation or accounting failure. It
does not stop because a completed answer is partial or wrong. One attempt per
task/arm supports descriptive paired comparisons, not repeatability or statistical
significance.

## Artifacts and interpretation

Use an ignored `results/` directory; never commit raw answers or artifacts. The
command refuses to reuse a non-empty output directory, and there is no implicit
resume that could silently mix experimental inputs.

- `run_manifest.json`: input hashes, config, exact schedule, declared environment.
- `preflight.json`: structured protocol verification and usage, no raw conversation.
- `index_setup.json`: cold setup timing and embedding readiness.
- `results.jsonl` / `results.csv` / `summary.json` / `report.md`: per-run rows,
  descriptive per-arm summaries and comparisons.
- `reviews.json`: manual strict and task-quality annotations.
- `instances/<task>/<arm>/run_N/`: `result.json`, plus `wire/` and observations when
  `--capture-traces` is set.

Wire files contain repository source and model reasoning; they are created with mode
0600 inside 0700 directories under an ignored path, and authentication headers are
never serialized. Keep the whole output tree private.

Usage is cumulative provider-reported input/output across every model request,
including repeated history. Reasoning tokens, if reported, are an output subset and
are never added again to total. Missing fields propagate to null; tool responses are
measured as UTF-8 bytes. Provider cached-token fields are retained per request and
summarised when every request reports them, because consecutive same-task arms can
otherwise differ through prompt caching rather than navigation behavior.

Grounding is a reference-precision proxy, not correctness. Efficiency differences
are not improvements when quality decreases, and a one-attempt-per-task pilot cannot
establish repeatability or significance.

## Validation

```sh
.venv/bin/python -m pytest bench/harness/tests -m 'not integration' -q
```

The offline suite needs no inference endpoint, no MCP binary and no credentials.
Integration-marked legacy tests are opt-in and excluded from CI.
