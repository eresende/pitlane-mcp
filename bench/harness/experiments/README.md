# Navigation skill experiment

This opt-in experiment reuses `AgenticLoop`, `BaselineExecutor`, the MCP stdio
transport, existing ripgrep prompts and `QualityScorer`. Existing `bench.harness.run`
commands, provider behavior, artifact schemas and historical reports are unchanged.
The shared loop adds optional system-prompt and bounded final-turn overrides;
legacy callers retain their defaults. Native grep intentionally fixes individual
file searches and reports invalid paths instead of silently returning no matches.

## Treatments and isolation

`../configs/navigation-skill.json` defines:

| Arm | Native read/search/list | MCP default-tier tools | Navigation skill |
|---|---|---|---|
| A | yes | unavailable; no server process | absent |
| B | yes | yes | absent |
| C | yes | yes, identical to B | exact SKILL.md in system prompt |

A/B receive byte-identical neutral system prompts. C appends the entire skill,
including front matter, before the first model request. The recorded skill hash
identifies those bytes. All arms receive identical task prompts, including the
existing prompt suffix. No legacy MCP-specific or task-specific benchmark guidance
is injected. Server tool descriptions are part of the MCP treatment in B and C;
they already contain navigation guidance and may reduce the marginal skill effect.

There is no host CLI agent, inherited conversation, dynamic skill discovery,
global agent config loading, shell tool, or unrestricted file reader. Each run
starts a new conversation, tool executor, and (B/C) MCP process. The model only
sees tools restricted to a tracked-HEAD snapshot. Untracked files, `.git`, agent
configuration and instruction files are absent; escaping/broken symlinks are rejected. Native
paths and MCP project paths cannot escape the snapshot. The MCP process has its
own HOME/cache and receives only PATH plus explicit `PITLANE_EMBED_*` settings.
The target checkout is never edited; its HEAD, status and file bytes, and snapshot
bytes, are checked throughout. Tracked dirty files cause refusal.

An advanced-tier **preparation-only** MCP process builds a fresh cache and waits
for embeddings if running. Task sessions expose the default tier. Cold setup
(including embeddings) and per-session cache-load overhead are separate from
navigation time. No model download or inference-server configuration/restart is
performed. Existing remote embedding settings may incur normal embedding API use;
record and hold them fixed. Without embedding settings, record lexical navigation
as such; do not compare it with a semantic-enabled run.

Five exact existing prompts cover symbol lookup, execution tracing, architecture,
dependency tracing and semantic discovery. There is no explicit change-impact
prompt in the existing suite: `usage_cli_to_search` is a dependency-path proxy,
not evidence of refactoring impact accuracy. No new symbols or answers are invented.

## Setup and preflight

Run from the repository root using Python 3.12+ on Unix (hard deadlines use SIGALRM):

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r bench/harness/tests/requirements.txt
export STRATA_BASE_URL='http://<your-strata-host>:8080/v1'
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase preflight --env-file /path/to/strata/.env --out results/skill-preflight
```

Alternatively export `STRATA_API_KEY`; `--api-key-env` selects a different variable.
The dotenv reader reads one literal value, never executes the file and never logs
the key. Do not put credentials in CLI arguments or committed config. Endpoint
selection is through `STRATA_BASE_URL` or `--base-url`; only its hash is persisted.
Preflight checks model discovery, an actual structured call with JSON arguments,
and consuming its result in a follow-up request. Passing once is necessary but
insufficient evidence of reliable task tool calling.

The model, temperature, output limit, 131072 context declaration, reasoning policy,
iterations, timeout and environment declarations live in the JSON config. The
API's reported model context must match for smoke validation to pass. Extra sampling
or reasoning request fields belong in `request_options`; reserved request fields
cannot be overridden.

`verify_reported_context` (default `true`) requires the endpoint to report the
loaded model's context as `meta.n_ctx` on `/models` and to match `context_window`.
Set it to `false` only for an OpenAI-compatible endpoint that cannot report its
context: smoke then lists `context_verified` under `not_evaluated` instead of
silently passing, and the declared context stays an unverified assumption. The
model-stability check compares the selected `/models` entry before and after each
run, so a server that also lists other models or extra response keys is supported;
a changed, missing or newly re-contexted entry still stops the run.

No reasoning override is sent by default: the same existing server default applies
to all arms. Engine version, quantization and KV settings
are user declarations unless independently observed; this harness does not change
them. Loaded-model metadata and available host GPU VRAM are sampled before/after
runs. Every phase also records the serving identity once as `environment.server`:
Strata's `/v1/status`, `/settings` and `/props` template hash when those routes
exist, otherwise a llama.cpp-family native listing (`/api/v0/models`: load state,
quantization, loaded context). When the endpoint exposes an engine build,
`declared_engine_matches_observed` compares it with `declared_environment.engine`;
otherwise it is listed under `not_evaluated`. The pilot's per-run stability check
uses the same control surface and is recorded as `not_evaluated` when no control
route exists. Configuration-change monitoring and server model-load events are unavailable.

The committed profile is a hosted OpenAI-compatible endpoint, because local serving
could not satisfy the suite's 131,072-token context on a 24 GB card (the largest
prompt observed is 116,857 tokens):

```sh
export ZAI_API_KEY=...            # or point --env-file at a private file
.venv/bin/python -m bench.harness.experiments.navigation_skill --phase smoke \
  --base-url https://api.z.ai/api/coding/paas/v4 \
  --api-key-env ZAI_API_KEY --env-file ~/.secrets --out results/zai-smoke
```

The endpoint lists `glm-4.7` but reports no `meta.n_ctx`, so
`verify_reported_context` is false and `context_verified` is not evaluated; the
declared 131,072 budget still gates every request. `min_free_vram_gib` is null
because no local accelerator is involved.

Local profiles were rejected first, and the reasons are worth keeping: a 27B model
at 131,072 context needs about 20% of layers on CPU to fit, which tripled wall
clock; full GPU offload used 23.3 of 23.98 GiB and crashed the host desktop session
during a pilot; and IQ3_XXS, though it fit comfortably, degenerated into 103
parallel `read_file` calls in one turn and then failed with `provider_http_400`.

`min_free_vram_gib` refuses to start a run when the measured free VRAM is below the
threshold; the observed headroom is recorded in `run_manifest.json`. A tighter
profile is not merely risky for the benchmark — a 0.7 GiB margin crashed the host
desktop session during a pilot, so keep real headroom rather than filling the card.

## Smoke first, then review
```sh
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase smoke --env-file /path/to/strata/.env --out results/skill-smoke
.venv/bin/python -m pytest bench/harness/tests -m 'not integration' -q
```

Smoke executes **exactly three task runs**, one per arm, using the configured `smoke_prompt_id` (currently `symbol_cli_config_flow`).
It stops after its report. `summary.json.smoke_validation.technical_passed` is the
execution gate; `correctness_review_passed` stays false until reviews exist, and
`not_evaluated` names checks the endpoint could not support. Check valid tool calls,
MCP use in B/C, no MCP in A, identical A/B base prompts, identical B/C schemas,
correct skill placement, usage availability, context and repository integrity.
Correctly rejected tool arguments and semantic/native tool errors that the agent
recovers from are recorded observations rather than technical failures, provided at
least one call succeeded and no protocol or transport failure occurred; this is the
same policy the pilot gate applies.
A hard per-run deadline includes provider and tool calls. It is sized above the
iteration budget (25 requests at the observed per-request cost), so it acts as a
safety net rather than preempting iterations a run is allowed to use; on the
20%-CPU-offload profile the slowest observed arm costs about 800s, hence 1200s. Model output truncation,
empty or unstructured tool responses and missing protocol identifiers are failures;
reasoning text never substitutes for an answer. There are no automatic HTTP retries.

Review each `instances/<task>/<arm>/run_1/result.json` final answer against the
snapshot. Fill `reviews.json` with a correctness score and concrete evidence:

- `1`: complete and correct; every requested component and relationship verified.
- `0.5`: partial, even if the stated references exist.
- `0`: incorrect or unsupported.

Also annotate appropriate MCP selection (boolean), unnecessary MCP calls (count),
and redundant repository searches (count), where review supports these judgments.
Leave unavailable metrics null. Grounding precision alone must not certify success.
Regenerate reports without model calls:

```sh
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase report --out results/skill-smoke
```

Technical readiness and correctness evaluation are separate. A technically valid
completed answer may be incorrect; it is still a valid benchmark observation.
Neither partial correctness nor an unreviewed answer prevents technical admission.
Historical strict scores remain unchanged. Review is required to interpret quality,
not to manufacture a prerequisite of perfect agent performance.

## Fifteen-run pilot and fixed task-quality rubric

The pilot uses exactly five `pilot_tasks`, once each under A/B/C, with fresh
sessions and seeded rotating arm order. It never reuses historical observations.
Admission reads a prior smoke's technical checks and pinned execution controls.
The prompt-building controller, loop, provider, tools and completion guidance must
be byte-identical; report and rubric changes are explicitly recorded without forcing
another smoke. Model, repository, prompts, skill, endpoint, embeddings, MCP binary, execution sources
and schemas must still match. `--capture-traces` is required for actual wire checks.

The evaluator-only [task-quality-v1 rubric](TASK_QUALITY_RUBRIC_V1.md) is frozen and
copied/hashed before inference. It separates components, essential relationships,
task-affecting errors, incidental errors and unsupported symbols. The strict
0/0.5/1 score and strict success stay unchanged. Task success and strict success
are separate fields and separate efficiency subsets. No historical result is
silently rescored; absent new reviews produce null new metrics.

```sh
.venv/bin/python -m pytest bench/harness/tests -m 'not integration' -q
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase pilot --validated-smoke results/technically-passing-smoke \
  --env-file /path/to/strata/.env --out results/new-pilot --capture-traces
# Populate strict review and each preregistered task_quality check with source evidence.
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase report --out results/new-pilot
```

The pilot stops after any failed final termination, provider/protocol or tool
transport failure, changed source/model/controls/schema, isolation or accounting
failure. Correctly rejected model arguments and returned semantic/native errors
are recorded agent observations if the agent recovers and completes. The pilot
does not stop because a completed answer is partial or wrong. Engine controls are
observed read-only before/after each run, outside navigation timing: Strata
settings/template/process/model/context where those routes exist, otherwise the
llama.cpp-family load state, quantization and loaded context. No server mutation is
performed, and none of this can prove that no operator changed the server. For
Strata, the default xhigh template should also be verified before the pilot with the
existing Docker verifier rather than inferred from configuration labels.

`summary.json` retains legacy keys and adds execution/quality separation,
task-quality metrics and task-success-only summaries. `reviews.json` contains
scored checks plus task/incidental error and invented-symbol evidence lists.
Across-task spread is not repeated-run variance. One attempt per task/arm supports
descriptive paired comparisons, not repeatability or statistical significance.

The first fresh 15-run pilot and its source-grounded quality classifications are
recorded in [the pilot report](../NAVIGATION_SKILL_PILOT_2026-10-18.md). It passed
technical execution checks; imperfect answers were retained as observations.

An explicitly authorized full experiment remains a separate command:

```sh
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase full --validated-smoke results/skill-smoke \
  --env-file /path/to/strata/.env --out results/skill-full
```

This command is **not** run automatically. It requires passing technical checks
and identical config/model, source,
skill, repository, prompts, MCP binary, embedding settings and provider endpoint.
If smoke fails, preserve its artifacts, address the cause, and run another smoke
into a new directory. Do not loosen correctness criteria to enable the full run.
Full executes 5 prompts × 3 arms × 3 repetitions = 45 fresh sessions. Seeded task
shuffle and cyclic arm rotation balance execution positions over repetitions.
Cache state inside Strata is not reset; this reduces order bias, not eliminates it.
Do not run concurrent inference jobs. Record operator-observed loading/config or
memory-pressure events separately; metadata snapshots cannot prove their absence.

## Artifacts and interpretation

Use ignored `results/` or a directory outside the repository; never commit raw
answers or artifacts. The command refuses to reuse a nonempty output directory.
There is no implicit resume that could silently mix experimental inputs.

- `run_manifest.json`: input hashes, config, exact schedule, declared/observed environment.
- `preflight.json`: structured protocol verification and usage, no raw conversation.
- `index_setup.json`: cold preparation, readiness and server version/embedding metadata.
- `instances/.../result.json`: per-run metrics, final answer, tool arguments,
  call hashes and failures. These local arguments may quote repository text;
  never commit these artifacts. Initial smoke artifacts recorded only hashes,
  which limits retrospective tool-selection review.
- `results.jsonl` / `results.csv`: structured rows for further analysis.
- `summary.json`: A/B, B/C, A/C deltas; per-task and aggregate n/mean/median/sample
  standard deviation/min/max; efficiency conditional on reviewed task success.
- `reviews.json`: manual quality annotations and supporting repository evidence.
- `report.md`: environment, method, measured tables, limitations and reproduction.

Token fields are provider-reported cumulative session input/output/total, summing
every request including repeated history. Missing any request's field makes that
cumulative field unavailable, not zero. Reasoning is an output subset if reported,
never added a second time. Tool-response size is UTF-8 bytes; tool-response tokens
are unavailable. Do not compare these values to character-based token estimates.

The historical quality score measures precision of references found by regex and
source lookup; it cannot verify completeness or causal explanation and can favor
terse answers. It remains a comparable grounding proxy, with independently reviewed
correctness primary. Successful-only efficiency uses only completed answers with
manual correctness 1; report its sample size and task composition. Exact repeated
successful calls and exact calls repeated after failure are mechanical proxies,
not semantic redundancy or proof of wasted work. Protocol failures are separate
from failed/invalid executed tool calls. Native fallback counts native calls after
the first MCP call; total native calls also include any initial native navigation.

The initial smoke sample is for instrumentation validation. Negative and inconclusive
outcomes are valid; do not claim significance or effectiveness from three runs.

## Termination diagnostics and bounded synthesis

The failed initial smoke is diagnosed in
[the termination review](../NAVIGATION_SKILL_TERMINATION_DIAGNOSIS_2026-10-18.md).
The experimental profile now uses identical tool-agnostic completion guidance in
A/B/C and `reserve_final_turn: true`. Within the unchanged 25-model-request budget,
requests 1–24 may use tools; request 25, if needed, asks for an evidence-backed
answer with no tools offered. Natural earlier completion is preserved. An attempted
tool call during the reserved response is an error, not another tool invocation.
`termination_kind` distinguishes `natural`, `budget_synthesis` and `unfinished`;
a budget-prompted response is **not** automatically a correct or complete task.
Manual correctness review remains mandatory. Legacy benchmark configurations opt
out by default and retain their previous iteration behavior.

When diagnosing a termination regression, use this minimal diagnostic command before another smoke:

```sh
.venv/bin/python -m bench.harness.experiments.diagnose_termination \
  --smoke results/skill-smoke --env-file /path/to/strata/.env \
  --out results/termination-diagnostic
```

It reuses the existing indexed snapshot, runs a direct arithmetic question, one
native read and one MCP read, and verifies answers against arithmetic or the actual
Cargo.toml package name. It does not load the navigation skill or launch A/B/C
benchmark arms. Each scenario starts a fresh conversation. Model, sampling,
reasoning policy and inference configuration are unchanged.

To exercise the reserved response with the same one-native-tool question, use a
**lower diagnostic-only** budget (the benchmark limit remains 25):

```sh
.venv/bin/python -m bench.harness.experiments.diagnose_termination \
  --smoke results/skill-smoke --env-file /path/to/strata/.env \
  --case one_native --iteration-budget 2 --out results/termination-boundary
```

`manifest.json` pins diagnostic configuration and the exact common prompt. Per-case
`wire/` files contain JSON request bodies and responses, including assistant content,
reasoning fields, finish reasons and tool IDs; authentication headers are never
saved. `parsed_result.json` allows comparison against the actual loop decision.
Keep these ignored/private artifacts out of version control. Subsequent benchmark
rows also record response-field metadata (keys, finish reasons, tool-call count,
content/reasoning byte sizes) without retaining raw reasoning/source bodies.
A changed profile/source hash requires a new technically passing, reviewed smoke before full
execution. The minimal cases alone do not establish broad navigation correctness.

For an explicitly authorized smoke requiring detailed local investigation, add
`--capture-traces` to the navigation_skill command. It uses the same traced provider
as the minimal diagnostics and saves `preflight-wire/` plus per-instance `wire/`,
`observations.json` and `parsed_result.json` under the ignored output directory.
These files include model reasoning and repository source in tool messages; never
commit them. Authentication headers are not captured. The flag is pinned in the
manifest/identity and applies equally to every arm. Wire logging I/O is included in
measured navigation time; parsed-conversation serialization is outside that timer.

## Recorded-evidence synthesis diagnostic

`diagnose_synthesis` makes exactly one generation request from an existing wire
checkpoint. It does not run smoke arms or execute any additional repository tools.
Verify the checkpoint's evidence first; this command does not establish sufficiency
or automatically grade causal completeness.

```sh
.venv/bin/python -m bench.harness.experiments.diagnose_synthesis \
  --source-run results/navigation-skill-smoke-20261018-3 \
  --arm C --checkpoint 16 --env-file /path/to/strata/.env \
  --out results/synthesis-diagnostic
```

Set `STRATA_BASE_URL` as above. Use a new ignored output directory separate from
the preserved source run. `--checkpoint 16` means **after** request 16's tool
results, which are found in recorded `request_17.json`. All those messages remain
intact, including C's skill and any earlier unhelpful tool responses. The command
removes tool definitions/tool choice and appends a tool-agnostic instruction for
an evidence-grounded answer of at most 300 words. Model, sampling, reasoning fields,
streaming policy and output allowance are copied unchanged from the wire payload;
model discovery must confirm the original context-window declaration.

`manifest.json` pins the source checkpoint/settings/instruction; `source_integrity.json`
checks the complete original artifact tree before/after. New `wire/`, observations
and summary files contain private local conversation/source/reasoning data, with
no authorization headers. Keep them out of version control. Review the visible
answer against the recorded evidence and snapshot, recording a separate review;
`stop` alone is not correctness. Summary tokens cover only this new request,
not historical requests whose messages it replays. A terminal tools-disabled reply
does not demonstrate natural stopping during navigation or make the smoke gate pass.

The targeted investigation and proposed next steps are documented in
[the diagnostic report](../NAVIGATION_SKILL_TARGETED_DIAGNOSTICS_2026-10-18.md).
The benchmark's shared completion prompt and the navigation skill are unchanged
by this diagnostic. Native grep now supports both files and directories through
the same executor in A/B/C; invalid/missing explicit paths return errors instead
of successful empty searches. Valid directory recursion, the 512 KiB per-file
limit and 200-match cap are retained. An explicitly selected oversized/unreadable
file returns an error; directory searches retain their existing skip behavior.

## Matched completion-guidance and reasoning comparison

After review, the following opt-in command runs four **synthesis diagnostics**
from the same C checkpoint, not the A/B/C navigation benchmark arms:

```sh
.venv/bin/python -m bench.harness.experiments.compare_synthesis \
  --source-run results/navigation-skill-smoke-20261018-3 \
  --strata-source /path/to/strata --container strata \
  --env-file /path/to/strata/.env --out results/matched-synthesis
```

`STRATA_BASE_URL` supplies the existing OpenAI-compatible endpoint. Every request
has identical recorded evidence and tool access disabled, temperature 0 and
`max_tokens=4096`. The factors are the unchanged generic reserved-turn instruction
versus concise task-neutral causal coverage, and omitted/server-default reasoning
versus explicit `reasoning_effort=medium`. The seeded execution order is recorded.
No shared benchmark prompt, skill or inference-server configuration is changed.

The verifier is deliberately specific to the inspected Docker deployment. It
queries read-only `/settings`, `/props`, `/v1/models` and `/v1/status`; checks that
the API is loaded/idle with the approved model and context; compares frontend/server
source hashes against the supplied read-only source tree; and runs a separate
CPU-only Python process inside the existing container. That process reads only
needed config fields, imports the deployed frontend/tokenizer, and renders/encodes
each request without constructing an engine, loading weights or starting a server.
It checks medium's template kwargs and absence of the xhigh instruction against
the default's xhigh instruction, then compares each actual API input-token count
with the independently encoded prompt length. It never treats HTTP acceptance or
shorter output alone as proof of reasoning control.

Medium cases are skipped if deployment/template verification fails; subsequent
medium cases are skipped if a live prompt-count check fails. The verifier currently
supports this deployment's start-position effort, empty shared defaults and absent
config effort override. A different deployment requires further inspection; refusal
is not evidence that its engine cannot support medium. The server has no direct
per-response effective-effort echo. Verification establishes the applied template
control, not a guaranteed amount or quality of reasoning; exact reasoning tokens
remain unavailable when usage omits them.

All wire data, rendered-prompt/token-ID hashes, verification metadata and raw answers
stay under the new ignored output directory. `results.json` contains unreviewed
metrics; populate a separate manual review and preserve raw per-request summaries.
Grounding score is a reference-precision proxy, not causal correctness. A terminal
reply is prompted synthesis; it cannot pass the navigation readiness gate.

The measured comparison, manual source review and limitations are recorded in
[the matched synthesis report](../NAVIGATION_SKILL_MATCHED_SYNTHESIS_2026-10-18.md).

## Provisional completion policy and evidence-presentation diagnostic

Following review, the navigation experiment uses default **xhigh** reasoning
(no per-request effort override) and the approved concise causal guidance. The
same task-neutral answer guidance is included in the neutral system prompt for
A/B/C, and the same concise synthesis instruction is used if their reserved final
request is reached. Only C still appends the unchanged skill. The legacy loop's
default reserved-turn prompt is preserved; the experiment opts into its custom
prompt via `final_turn_prompt`. Reasoning defaults must be reverified before a
future smoke; the configuration description is not a live effective-effort echo.
No new smoke is implied by this provisional policy.

The evidence diagnostic makes four tools-disabled synthesis requests, two per
presentation, in balanced order accumulated → compact → compact → accumulated:

```sh
.venv/bin/python -m bench.harness.experiments.compare_evidence \
  --source-run results/navigation-skill-smoke-20261018-3 \
  --reasoning-proof results/navigation-skill-matched-synthesis-20261018-1/reasoning_verification.json \
  --container strata --env-file /path/to/strata/.env \
  --out results/new-evidence-diagnostic
```

Set `STRATA_BASE_URL` at runtime. It reuses the verified historical C checkpoint
and its original system/task messages unchanged, so this diagnostic does not test
the new system-prompt policy or navigation behavior. Both presentations append
the same approved concise instruction and keep temperature 0, output allowance
4096 and the existing model/context. Additional tools remain unavailable.

Compact evidence is extracted mechanically from **every** recorded source-bearing
response: source fields, investigation Rust blocks, positive native/MCP text hits
and source signatures. First-seen order is preserved; identical snippets are
deduplicated. Generated truncation markers and navigation/tool metadata are removed.
File/line provenance is retained. No source lines are fetched to enrich the model
input: snapshot reads only verify recorded snippets against the frozen repository.
Original source comments remain verbatim; no reviewer explanations, corrected
facts or topic-specific questions are inserted. The hidden/precedence/pruning/state
rubric is for evaluation only. Omitted topics remain unassessed, not assumed correct.

The command revalidates Docker code/template hashes against the reviewed proof,
renders/tokenizes both inputs CPU-only, and checks live input counts. It refuses
changed defaults/model state and stops on a renderer mismatch. It does not change
or restart Strata. Original checkpoint artifacts are hashed before/after; traces,
compact source collection and provenance audit remain in a new ignored directory.
Run automated tests **before**, rather than during, inference to avoid adding host
CPU load to timed requests.

This changes presentation, history roles, repetition and metadata together; it
does not separately identify the effect of length or a particular wrapper. Cache
state is not reset. One small diagnostic cannot prove a model-intrinsic cause.
Future cross-model testing requires its own approved deployment and verification;
this command never downloads or switches models.

See [the evidence-presentation report](../NAVIGATION_SKILL_EVIDENCE_PRESENTATION_2026-10-18.md)
for measured results, four-concept review and the recommended cross-model follow-up.

## Already-loaded LM Studio cross-model diagnostic

After operator approval and manual model preparation, this command verifies the
exact loaded instance/quantization/context before making any generation request.
It has no load, unload, download or inference-setting mutation code paths.

```sh
# Set LM_STUDIO_BASE_URL to the existing server origin or /v1 base at runtime.
.venv/bin/python -m bench.harness.experiments.compare_local_model \
  --model 'your-exact-loaded-instance-id' \
  --model-file /path/to/the-existing-Q4_K_M.gguf \
  --source-run results/navigation-skill-evidence-presentation-20261018-1 \
  --out results/new-local-model-diagnostic
```

Native `/api/v1/models` and `/api/v0/models` must agree on the loaded Q4_K_M
instance with at least the controlled 131072-token context. A CPU-only GGUF header
read records architecture, training context and the existing template, skipping
tensor payloads. The normal strict provider performs one synthetic structured
tool round trip and checks provider token reporting. Only after it passes are
four tools-disabled synthesis requests sent, in the same order as the reference.

Each request is copied from the preserved accumulated/compact wire payload; **only
the model identifier changes**. Task, source, completion guidance, temperature 0,
4096 output allowance and non-streaming policy remain byte-for-byte/field-for-field
unchanged. Native model reasoning defaults are retained rather than assuming
Strata's effort control exists. Record API-advertised reasoning options, the GGUF
template and observed reasoning output separately: none is a direct echo of all
effective per-request template arguments.

Raw preflight and diagnostic wire responses remain in the ignored artifact directory.
Per-response usage is authoritative, including a reasoning-token output subset when
the provider reports it; never add that subset again to total tokens. Tokenizers,
server templates may differ, and runtime/model/quantization differ, so raw token
ratios across providers are not equivalent costs. Stop if the exact loaded model
or configuration changes. Independently review answers against the same source
and keep raw summaries separate from manual annotations. This is not a navigation
benchmark or permission to run a subsequent smoke.

Measured results and the existing-task proposal are in
[the LM Studio cross-model report](../NAVIGATION_SKILL_CROSS_MODEL_2026-10-18.md).

## Infrastructure review and historical reproduction

The [infrastructure review](../NAVIGATION_BENCHMARK_INFRASTRUCTURE_REVIEW_2026-10-18.md)
documents corrections made after the pilot. Future smoke/pilot admission checks
terminal `stop`, internally consistent per-request/cumulative usage, context and
request budgets, source/config/skill/prompt integrity and a separately pinned MCP
binary. Execution-critical changes require a **fresh** technical smoke; the old
CLI smoke is retained as historical evidence and is not admission evidence for the
reviewed code. Correctness is still measured separately and need not be perfect.
Do not regenerate historical reports with changed validators or overwrite their
artifacts. Their recorded source hashes describe the code actually used then.

Wire traces are created with mode 0600 under 0700 wire directories. New diagnostic
output roots and navigation output roots use 0700 as well. Authentication headers
are never serialized; dotenv variants are omitted from new tracked-HEAD snapshots.
This is protection for a trusted local public-repository experiment, not an OS
sandbox for hostile repositories or MCP executables. Keep the whole output tree
private: answers, source payloads, arguments and reasoning are sensitive even when
no API key occurs. In-repository outputs must be Git-ignored; historical output
permissions/files are not rewritten by the review.

Offline regression tests and the new benchmark-harness CI job require no inference
server, downloaded model, MCP binary or credentials. Integration-marked legacy
tests are opt-in and excluded from CI. Reproduce the offline check with:

```sh
.venv/bin/python -m pip install -r bench/harness/tests/requirements.txt
.venv/bin/python -m pytest bench/harness/tests -m 'not integration' -q
```

The Strata Docker renderer diagnostics intentionally target the inspected
0.1.44.1 deployment; they are not a general compatibility guarantee for other
engines. They depend on local preserved checkpoints, read-only Strata source,
container access and its tokenizer. Raw checkpoints remain private rather than
being bundled for public reproduction. New experiments can be reproduced from
a fresh clean ripgrep checkout, pinned tool binary/embedding settings and new
smoke artifacts. Manual source-grounded annotations are not deterministic tests;
a new review must retain its rubric, evidence and reviewer identity separately.
All commands that generate model responses still require explicit experimental
authorization. No historical raw conversation is needed to run the offline tests.
