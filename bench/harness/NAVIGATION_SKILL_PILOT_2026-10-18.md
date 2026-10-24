# Navigation skill pilot — 2026-10-18

**The 15-run pilot completed and technical readiness passed. Correctness was measured after execution, not used as an admission gate. This sample does not show an overall benefit from the navigation skill.**

C selected more MCP calls than B on **all five tasks**, but more total calls on only **three**. Its task-oriented quality was higher on **one**, tied on **two**, and lower on **two**. Across the five tasks, C consumed **22.7% more cumulative tokens** and **8.7% more time** than B, with lower average strict and task-oriented quality. These are descriptive results from one fresh attempt per task/arm, not estimates of a repeatable or statistically significant effect.

All 15 answers terminated with `finish_reason=stop`: **13 natural**, **2 budget-prompted**. Six rejected/failed navigation calls were recovered agent errors; there were **no provider/protocol or tool-transport failures**, source mutations, isolation failures, missing usage fields or context overflows. No further navigation experiment, full 45-run benchmark, skill/production Rust edit, commit, push or PR followed.

## Methodology revision implemented before inference

The runner now separates:

1. **Technical readiness:** connectivity, exercised structured tool protocol, isolation, unchanged model/controls/schemas, valid accounting, budget/context compliance and valid final termination.
2. **Correctness evaluation:** source-grounded strict and task-oriented reviews after execution. Imperfect completed answers remain valid observations.

`--phase pilot` requires a technically passing smoke and matching execution controls. Admission uses the previous smoke only to check controls and schema identities; it does not import its answers, scores or performance observations. Controller/report/rubric changes are explicitly pinned rather than forcing another identical smoke. Execution-critical agent loop, native executor, provider, MCP transport, tool wrapper and completion-guidance sources must match the smoke. Every new attempt is audited against actual wire messages and schemas. Execution stops after a technical failure; no retries/resume silently fill missing cases.

Correctly returned argument/path/native/semantic errors are measured agent observations when the session recovers and completes. A timeout, truncated/absent final answer, provider/protocol or tool-transport failure, changed controls, source/schema/isolation failure or invalid accounting stops the pilot. This policy was documented before inference. An invalid request rejected by the snapshot restriction is not a successful escape.

The unchanged strict score remains **0 / 0.5 / 1**. Strict success still requires a completed, fully correct answer with score 1. No historical result or report was rescored.

The additional [task-quality-v1 rubric](experiments/TASK_QUALITY_RUBRIC_V1.md) was written, tested, copied and hashed before inference. It measures required components and essential relationships separately, with **0 / 0.5 / 1 per check**. Task quality is the equally weighted mean of the component mean and relationship mean. An error changing a required owner/operation/decision/causal edge, or an unsupported required symbol, caps it at 0.5. Incidental errors in optional details remain visible and affect strict correctness but do not reduce task quality. Omission of optional detail is not an error.

Exact check IDs are fixed in `experiments/task_quality.py`. The five categories are required components, essential relationships, task-affecting errors, incidental errors, and unsupported/invented symbols. A task-oriented success requires completed execution and task quality 1; it is separate from strict success. Missing review remains null. A correct inference can receive credit when independently verified against source; an explicit uncertainty does not make missing required coverage complete. Equivalent valid paths/abstractions are accepted without requiring every flag or helper.

Rubric document SHA-256: `03df7b004d1b1786a7f7db7058c2d8a9714f58cd7097965ea9dc348a18303f02`. The document, Python checks, prompts and skill remained unchanged throughout execution.

## Workload and controls

All existing prompt text and grounding suffixes were used unchanged. No review rubric, corrected fact, canonical symbol or source anchor was added to model inputs.

| Short label | Existing prompt ID | Purpose |
|---|---|---|
| Ignore | `symbol_ignore_logic` | Ignore implementation and its connection to traversal |
| CLI | `symbol_cli_config_flow` | Parsing → runtime configuration → search |
| Call chain | `graph_nav_call_chain` | Entry → regex match → stdout |
| Architecture | `arch_package_map` | Main modules/crates and responsibilities |
| Binary | `semantic_search_probe` | Binary detection and handling policy |

Exactly one fresh session ran per task/configuration, **15 new observations**. Seed **20261018** yielded: architecture **B/A/C**, binary **A/C/B**, ignore **C/B/A**, call chain **B/A/C**, CLI **A/C/B**. There is no historical result reuse or repeated-run variance estimate.

| Control | Fixed value |
|---|---|
| Model / quantization | `qwen3.8-flash-next-coder-iq1_m` / IQ1_M |
| Engine | Existing Docker Strata **0.1.44.1** |
| Reasoning | Verified default **xhigh**; no request override |
| Context | **131072** |
| Sampling / output | Temperature **0**, **4096 output tokens per request** |
| Request budget / timeout | **25**, including reserved synthesis; **300 s** per attempt |
| Repository commit | ripgrep `4649aa9700619f94cf9c66876e9549d83420e16c` |
| Completion guidance | Unchanged shared neutral + concise causal guidance in A/B/C |
| Skill | Exact unchanged skill only in C; hash `09556ef23680fdde79a6436bd6cf9e4b3e606e08f7099985187ed5de9e5506e9` |
| Tools | Same fixed native read/search/list implementation in A/B/C; identical MCP schemas in B/C |
| Hardware declaration | RX 7900 XTX 24 GB, Ryzen 9 9950X, 32 GB RAM, 8-bit KV cache |

A exposes **3 native tools**, with no MCP server or skill. B/C expose **17 total tools** in this server build; only C appends skill instructions. Native grep includes the previously corrected file/directory implementation. No host/global agent instructions are loaded; tracked-HEAD snapshots omit agent configuration and escaping/broken symlinks. Per-arm actual wire audits passed, including A/B system equality, identical task/suffixes, C-only skill and unchanged schemas. Repository, snapshot and harness Python source hashes match their pre-run manifest.

Cold MCP setup including indexing and embedding readiness took **13.544 s**, excluded from navigation. The snapshot contains **101 indexed files / 3207 symbols**, with embeddings ready before timing. Task sessions use the default tier and watchers are disabled; the preparation-only advanced tier is not exposed to agents. Existing embedding settings were retained.

Before inference, read-only Docker code/config/template/tokenizer verification established default xhigh. After all runs, CPU-only rendering of the **first and final request of every session (30 payloads)** exactly matched live input-token counts and the deployed xhigh rendering path. Every actual request contains unchanged sampling controls and no reasoning override. There is no effective-effort response echo or numeric reasoning-token telemetry; verification does not claim a particular amount of reasoning.

Settings, template, requested model/context, concurrency and Strata process identity matched before/after every run. The API request-counter increase was exactly **286 = 2 preflight + 284 pilot requests**, with no additional inference generations observed. No server mutation/download/load/restart commands were issued. Host VRAM samples ranged **23,685,619,712–23,808,053,248 bytes**. Sparse samples show no observed model/configuration change or memory failure, not a proof of transient pressure or invisible same-model reload absence. KV precision and hardware details remain user declarations where not exposed independently.

## Aggregate quality and execution

Each arm has **five different tasks, one attempt each**. Mean/median are across tasks, not repeated measurements of one task.

| Arm | Strict mean / median | Task quality mean / median | Strict successes | Task-oriented successes | Tokens mean / median | Time mean / median (s) |
|---|---|---|---:|---:|---|---|
| A: native | 0.600 / 0.500 | 1.0000 / 1.0000 | 1/5 | 5/5 | 993,294.8 / 998,452 | 147.778 / 147.417 |
| B: MCP | 0.600 / 0.500 | 0.9325 / 1.0000 | 1/5 | 4/5 | 321,831.0 / 310,105 | 90.731 / 95.060 |
| C: MCP + skill | 0.500 / 0.500 | 0.8000 / 1.0000 | 0/5 | 3/5 | 395,002.4 / 402,816 | 98.623 / 96.891 |

| Arm | Cumulative input | Cumulative output | Total | Native / MCP calls | Failed / calls | Exact redundant calls | Natural / budget |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 4,940,462 | 26,012 | 4,966,474 | 184 / 0 | 1 / 184 (0.54%) | 7 | 4 / 1 |
| B | 1,586,834 | 22,321 | 1,609,155 | 82 / 54 | 3 / 136 (2.21%) | 1 | 4 / 1 |
| C | 1,948,805 | 26,207 | 1,975,012 | 29 / 125 | 2 / 154 (1.30%) | 0 | 5 / 0 |

Across the pilot: **8,476,101 input + 74,540 output = 8,550,641 tokens**, **474 tool calls**, and **1685.655 s** summed navigation time. Preflight used **701 input + 141 output = 842 tokens**, excluded above; combined new inference consumption is **8,551,483 tokens**. Index/embedding usage is separate and unavailable from the agent usage counter.

All last finish reasons are `stop`; none are `length`. Reserved synthesis occurred only for architecture A and call-chain B. Natural completion means a final answer while tools remained available, without the reserved prompt. It is not a correctness certification.

Reasoning-token counts and tool-response token counts are **unavailable**; neither is estimated. Output usage includes any reasoning emitted by the provider. Tool responses total **1,578,183 / 546,471 / 418,089 UTF-8 bytes** in A/B/C and are not added again to provider token totals. Cached input fields are retained per request; the same provider accounting method applies to all arms.

Peak input in a single request was **102,005 / 71,523 / 38,806 tokens** in A/B/C. Including the full 4096 output allowance gives **106,101 / 75,619 / 42,902**, all below 131072. Session totals repeatedly count accumulated history; millions of cumulative tokens are not millions of simultaneously resident context tokens. Smaller tool-response byte totals alone do not imply smaller cumulative input: call timing, repeated history, schemas and C's skill all contribute.

## Per-task results

Each cell in the quality table is **strict / task-oriented**. Strict partial results remain partial; the new assessment does not replace them.

| Task | A | B | C |
|---|---:|---:|---:|
| Ignore | 1 / 1 | 0.5 / 1 | 0.5 / 0.5 |
| CLI | 0.5 / 1 | 0.5 / 1 | 0.5 / 1 |
| Call chain | 0.5 / 1 | 0.5 / 0.6625 | 0.5 / 1 |
| Architecture | 0.5 / 1 | 0.5 / 1 | 0.5 / 1 |
| Binary | 0.5 / 1 | 1 / 1 | 0.5 / 0.5 |

| Task | Arm | Input | Output | Total tokens | Native / MCP | Errors | Seconds | Tool bytes | Completion |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| Architecture | A | 906,585 | 5,197 | 911,782 | 50 / 0 | 0 | 164.290 | 235,647 | Budget |
| Architecture | B | 457,040 | 3,564 | 460,604 | 31 / 2 | 0 | 103.114 | 255,449 | Natural |
| Architecture | C | 419,205 | 5,165 | 424,370 | 18 / 18 | 0 | 96.891 | 82,333 | Natural |
| Binary | A | 1,225,126 | 4,273 | 1,229,399 | 33 / 0 | 1 | 136.879 | 358,484 | Natural |
| Binary | B | 386,882 | 5,106 | 391,988 | 14 / 12 | 1 | 97.615 | 109,960 | Natural |
| Binary | C | 249,398 | 4,343 | 253,741 | 6 / 17 | 1 | 78.820 | 70,592 | Natural |
| Ignore | A | 993,900 | 4,552 | 998,452 | 29 / 0 | 0 | 134.640 | 301,569 | Natural |
| Ignore | B | 304,638 | 5,467 | 310,105 | 19 / 15 | 2 | 95.060 | 74,831 | Natural |
| Ignore | C | 397,362 | 5,454 | 402,816 | 1 / 30 | 1 | 103.680 | 105,174 | Natural |
| Call chain | A | 764,019 | 6,976 | 770,995 | 29 / 0 | 0 | 155.661 | 318,100 | Natural |
| Call chain | B | 277,320 | 4,458 | 281,778 | 11 / 13 | 0 | 90.238 | 43,577 | Budget |
| Call chain | C | 488,771 | 6,904 | 495,675 | 4 / 35 | 0 | 128.273 | 78,247 | Natural |
| CLI | A | 1,050,832 | 5,014 | 1,055,846 | 43 / 0 | 0 | 147.417 | 364,383 | Natural |
| CLI | B | 160,954 | 3,726 | 164,680 | 7 / 12 | 0 | 67.627 | 62,654 | Natural |
| CLI | C | 394,069 | 4,341 | 398,410 | 0 / 25 | 0 | 85.448 | 81,743 | Natural |

Output columns are cumulative across a session, so values above 4096 do not indicate a per-request allowance violation. All actual request limits and token sums passed validation.

## Source-grounded quality findings

Review was performed independently of the runtime gate against the frozen source. No facts were fed back to agents. It was a single assistant review with arm labels visible, not blind grading or an independent second reviewer. Full check-level evidence and error objects are local in `reviews.json` and `correctness_dimensions.jsonl`. The following describes the distinctions behind the scores, including judgment-sensitive classifications.

- **Architecture:** all three identify the requested main components and relationships. A incorrectly says `HiArgs::from_low_args` itself builds matcher/searcher/printer; those accessors are invoked later by `main.rs:113–140`. B calls the decompression flag `--zip`; `flags/defs.rs:5961–5976` defines `-z/--search-zip`. C groups parallel traversal with `WalkDir`; `ignore/src/walk.rs:1372–1373,1541` uses `fs::read_dir` for parallel work. These extra implementation details do not change the required high-level package ownership map, so all task scores are 1 while strict scores are 0.5.
- **Binary:** B correctly explains the NUL heuristic, explicit/implicit policies, scanning and output effects. A misrepresents the public `BinaryDetection` wrapper's API as enum variants; the real enum is in `line_buffer.rs`, while `searcher/mod.rs:55–104` exposes lowercase methods. Its actual scanning/policy explanation is complete, making that representation error incidental. C assigns required detection/output callbacks to nonexistent qualified owners: `LineBufferBuilder::fill` and `StandardImpl::binary_data`. The actual owners are `LineBuffer::fill` (`line_buffer.rs:325,406`) and `StandardSink::binary_data` (`printer/standard.rs:807,885`). These change the requested implementation targets; C's task score is capped at 0.5 despite largely correct policy relationships.
- **Ignore:** A correctly connects rule parsing, hierarchy, matcher scope and pruning. B also covers that flow, but adds an unsupported optional history claim that absent `IgnoreCache`/`matches_path` APIs were renamed. Current-source absence does not establish that history; it is unsupported provenance, not a proven historical falsehood. C correctly describes pruning and state transitions but puts explicit/global ignores after types. `dir.rs:365–409,413–535` evaluates those inside `matched_ignore`, before types. This is a wrong matching decision rule affecting the requested behavior, so C's task score is capped at 0.5. No exhaustive hidden/type precedence inventory was demanded of answers that did not assert one.
- **Call chain:** A supplies concrete matching, sink, printer and stdout connections. Its optional external-package attribution says `regex` where `crates/regex/Cargo.toml:16–21` identifies `regex-automata`. B leaves actual regex execution and printer/stdout-write internals abstract and explicitly unverified; required components average **0.7**, relationships **0.625**, producing **0.6625**. C connects an actual matching call, `Core::sink_matched`, writer `write_all` and parallel `BufferWriter::print`, covering a valid essential path. Its uncertainty paragraph supplies unsupported optional `StandardPrinterSink::write/sink_matched` names; actual owners include `StandardSink::matched`, `StandardImpl::write` and `Core::sink_matched`. That uncertainty is not counted as inspected source. The concrete path remains complete at the requested key-call level, so task quality is 1 and strict quality 0.5. Alternative valid paths are accepted without demanding every private callback.
- **CLI:** all explain the essential update → `LowArgs` → `HiArgs` → main/search builder handoff. A wrongly attributes optional rc argv merge/reparse to `config.rs::args`; `config.rs:16–47` returns argv and `parse.rs:96–111` performs the conditional merge. B/C call `FlagMap` a trie; `parse.rs:368–396` uses a `HashMap`. B additionally claims local matcher/searcher internals are external and unavailable, contradicted by root `Cargo.toml:40–63` and the `grep` facade's local re-exports. The matching library bodies are beyond this task's CLI handoff, which is correctly explained; these errors lower strict precision without removing the covered task edges.

Task scope matters: constructor-versus-accessor detail can be incidental to a package map, while a wrong target owner for a requested detection implementation or an ignore decision that changes visitation is task-affecting. Classifications are explicit and reviewable rather than silently folded into a new score. No task-specific correctness hint was added after seeing an answer.

Observed task-error / incidental-error / unsupported-symbol counts are **0/4/3 in A**, **0/5/1 in B**, **3/3/4 in C**. Some categories overlap: an invented target may also explain a task error; do not add these counts as independent failures. They enumerate reviewed claims, not a proof that every optional factual detail is error-free. Unsupported optional names remain visible even when task quality is 1.

The unchanged lexical grounding proxy averages approximately **1.000 / 0.985 / 0.953** in A/B/C. It can miss wrong method ownership and incomplete causal edges: C's binary answer receives lexical grounding 1 despite its false qualified callback owners, and B's incomplete call-chain answer also receives 1. It is not substituted for either manual quality measure.

## B versus C: additional MCP use and its value

Positive deltas mean C is larger than B.

| Task | Task quality B → C | MCP calls B → C | Total-call delta | Token delta | Time delta (s) |
|---|---|---|---:|---:|---:|
| Ignore | 1 → 0.5 | 15 → 30 | -3 | +92,711 | +8.620 |
| CLI | 1 → 1 | 12 → 25 | +6 | +233,730 | +17.821 |
| Call chain | 0.6625 → 1 | 13 → 35 | +15 | +213,897 | +38.035 |
| Architecture | 1 → 1 | 2 → 18 | +3 | -36,234 | -6.223 |
| Binary | 1 → 0.5 | 12 → 17 | -3 | -138,247 | -18.795 |

C replaces many native calls: **29 versus B's 82**, while MCP calls rise **54 → 125** and total calls **136 → 154**. It is inaccurate to label all 71 additional MCP calls as 71 additional total calls. C uses **91 model requests**, B **86**; source/history placement and skill/schema input contribute to cumulative token use.

C's family mix includes **11 locate**, **33 content search**, **59 code-unit reads** and **9 investigate** calls, compared with B's **1 / 0 / 39 / 6** respectively. C also requests readiness/stats on every task; the repository was already prepared. These are relevant tool families, but relevance is not proof of necessity. B sometimes relies mostly on native navigation despite MCP availability. No unique optimal tool plan was established, so scalar appropriateness, semantic-redundancy and unnecessary-MCP-call counts remain **unavailable**, with qualitative traces retained.

There is a consistent association with greater MCP selection in this pilot. There is **not** a universal absence of task-quality gain: C improves call-chain coverage and avoids B's reserved synthesis there. Nevertheless, C's average strict quality falls **0.6 → 0.5** and task quality **0.9325 → 0.8**, while total tokens rise **1,609,155 → 1,975,012** and time **453.654 → 493.113 s**. The additional exploration did not produce an overall quality advantage. Attribution of these directions as a stable causal skill effect requires repeated observations; none were run.

## Other comparisons and quality-conditioned efficiency

| Comparison | Strict mean delta | Task-quality mean delta | Mean-token delta | Mean-time delta (s) | Mean-call delta |
|---|---:|---:|---:|---:|---:|
| A → B | 0 | -0.0675 | -671,463.8 | -57.047 | -9.6 |
| B → C | -0.1 | -0.1325 | +73,171.4 | +7.892 | +3.6 |
| A → C | -0.1 | -0.2000 | -598,292.4 | -49.155 | -6.0 |

MCP-enabled arms use fewer resources than the three-tool native baseline here, but also have lower mean task-oriented quality. Lower consumption is not declared an improvement when correctness decreases.

Strict-success efficiency has **n=1 for A** (ignore: 998,452 tokens, 134.640 s), **n=1 for B** (binary: 391,988 tokens, 97.615 s), **n=0 for C**. These different tasks are not a fair cross-arm speed comparison. There are **zero shared strict-success B/C tasks**.

Task-success-only token/time means are A **993,294.8 / 147.778 s (n=5)**, B **331,844.25 / 90.854 s (n=4)** and C **439,485 / 103.537 s (n=3)**. Task composition differs, so these are conditional summaries, not matched efficiency rankings. On the **same two tasks where B and C both have task quality 1** (architecture and CLI), B averages **312,642 tokens / 85.371 s**, C **411,390 / 91.170 s**. Strict partial labels still apply to all four answers. Both views and sample/task identities are preserved in machine-readable summaries.

## Errors, redundancy and limits

All six tool errors recovered without transport/protocol failure: nonexistent native paths in binary A/B; two B ignore queries using `project="."`; C binary using a filename in a project-path field; C ignore using `pattern` instead of required `query`. Restrictions rejected these requests without repair or escape. C has one invalid-schema call; the other errors are path/native failures. Exact retry counters are zero; changed arguments, batched bad calls and switching tool family are not counted as exact retries.

Exact redundant successful calls are **7 / 1 / 0** in A/B/C, returning **200,530 / 229 / 0 repeated bytes**. A rereads truncated walk/printer/flag files and relists CLI directories. B repeats one ignore directory listing. Repeating the same native 40,000-character prefix cannot expand missing source evidence. C's zero exact duplicates does not prove absence of semantically overlapping searches/reads. Its broader lookup/read inventory and late exploration sometimes coexist with wrong owners or unverified methods. No unsupported semantic-redundancy count is invented.

Important limitations:

- One attempt per task/arm: across-task standard deviations in JSON describe heterogeneous workloads, not repeated-run variance or statistical significance.
- Single label-aware assistant reviewer; error severity and incidental/task scope decisions remain judgment-sensitive. Detailed evidence permits later independent review without rerunning or silently changing these annotations.
- Task-quality weights/caps are transparent but not externally calibrated. The prototype score supplements strict correctness; it does not prove that every optional claim is correct.
- This native baseline is the existing three-tool harness, with truncated full-file reads and no native line-slice tool, not every native coding agent. Both MCP arms retain identical native tools and server descriptions, which already contain guidance.
- No provider cache flush or GPU warm-up reset was performed. Rotated order reduces systematic bias but five task groups cannot perfectly balance all cache conditions. Temperature zero does not establish repeatability from one observation.
- Light progress/source-review reads overlapped some inference; no tests/builds, additional inference diagnostics or Docker tokenization ran during timed navigation. Host activity was not separately profiled, so small timing differences should not be overinterpreted.
- Source and reasoning traces remain local. There is no reasoning-token breakdown, tool-response token count, exhaustive semantic tool-optimality oracle or reliable transient GPU-pressure measurement.
- These tasks cover navigation and semantic discovery, not a dedicated hypothetical change-impact workload. Synthesis diagnostic results and earlier smoke observations are not pooled into this pilot.

The measured findings support retaining technical-only admission and reporting both quality views. They do not support claiming the current skill consistently improves navigation. Review the annotations and these pilot observations before choosing a follow-up experiment; none is authorized or run automatically here.

## Artifacts, tests and reproduction

Local ignored artifacts: `results/navigation-skill-pilot-20261018-1/`; read-only engine proof: `results/navigation-skill-pilot-20261018-1-verification/`. They include the manifest, copied rubric, readiness/schema fingerprints, preflight and cold-index metadata, per-instance raw results/wire/observations, environment samples, reviewed JSONL/CSV, `reviews.json`, `correctness_dimensions.jsonl`, `paired_B_C.csv`, `pilot_analysis.json`, `summary.json`, `integrity_validation.json` and verification/test summaries. Original per-instance raw results and traces are preserved; reviewed derivatives are separate. Raw conversations, reasoning, tool source payloads and credentials are not committed.

**161 tests passed before inference (10.96 s) and again after (11.04 s)**. Tests cover technical admission independent of partial/unreviewed strict quality, preservation of strict success, task/incidental/unsupported distinctions, missing reviews, fixed check IDs, rotation, accounting failures, contaminated wire prompts/schemas/settings, technical fail-fast checks and the shared native file/directory grep regressions. `git diff --check` and repository/skill/prompt/rubric/Python-source integrity audits passed. No production Rust or skill edit occurred.

```sh
# Use the existing Strata /v1 endpoint at runtime; never put its address/key in config.
export STRATA_BASE_URL='http://<existing-strata-host>:8080/v1'
.venv/bin/python -m pytest bench/harness/tests -q
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase pilot --config bench/harness/configs/navigation-skill.json \
  --validated-smoke results/navigation-skill-cli-smoke-20261018-1 \
  --env-file "$HOME/projects/strata/.env" \
  --out results/new-navigation-pilot --capture-traces
# Review the new frozen snapshot using the predeclared rubric, populate reviews.json.
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase report --out results/new-navigation-pilot
```

See [experiment instructions](experiments/README.md) for admission and artifact details. The reproduction command documents the procedure; this run stopped after 15 attempts and offline reporting. No full benchmark, commit or push was performed.
