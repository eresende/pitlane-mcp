# Navigation skill targeted diagnostics — 2026-10-18

Native grep is fixed and regression-tested. A single replay of C's recorded evidence terminated with `stop` using 617 of the unchanged 4,096 output tokens. Its references and forward call chain are correct, but the answer omits central traversal effects, so manual correctness is **partial (0.5), not successful task completion**. The existing three-arm readiness gate remains failed. No new smoke or full benchmark was run.

## Native grep: cause, fix and verification

`BaselineExecutor._grep_search` called `os.walk()` for every input. Walking a regular source file yields no entries, so valid file searches silently returned `No matches found.` Missing paths behaved the same way. Paths outside the repository silently fell back to searching the root.

The shared executor now validates the explicit path and searches a regular file directly or recursively walks a directory. Results retain repository-relative paths, one-based line numbers, Python regex matching, sorted filenames within each directory, UTF-8 replacement decoding, the 512 KiB per-file limit and 200-match cap. Valid empty searches still return `No matches found.` Missing paths, invalid intermediate components, unsupported path types and paths outside the repository return `Error:` responses. An explicit oversized or unreadable file also returns an error; recursive directory searches retain their existing file-skip behavior. The tool's path description now explicitly supports files and directories.

This implementation and description apply to **A, B and C** through their existing shared native executor. Experimental error accounting classifies these failures as `native_tool_error` (or the existing `path_outside_snapshot` restriction). No PitlaneMCP production search implementation changed. Older artifacts are preserved; they are not retrospectively reclassified or rescored.

Regression coverage includes file and directory matches, omitted/root/subdirectory paths, absolute file paths, true empty results, missing/invalid/escaping paths, unsupported FIFO paths, file-read failure, invalid regex, size/match limits, response byte accounting and shared A/B/C dispatch/error classification. **139 harness tests passed**, including 19 native-search cases and 5 replay-control cases.

A deterministic replay of every original file-targeted native grep against the same frozen snapshot verified results against regex matching of the actual source:

| Arm | Original file searches replayed | Searches with real source matches | Genuine empty searches | Correct after fix |
|---|---:|---:|---:|---:|
| A | 10 | 9 | 1 | 10/10 |
| B | 1 | 1 | 0 | 1/1 |
| C | 6 | 4 | 2 | 6/6 |

Thus 14 of these 17 calls demonstrably produced false empty results previously. The other three were genuinely empty searches; they should not be counted as false negatives. This check uses no model calls.

## Checkpoint evidence audit

Source artifacts: `results/navigation-skill-smoke-20261018-3/`, task `symbol_ignore_logic`, ripgrep commit `4649aa9700619f94cf9c66876e9549d83420e16c`. The original task asks for ignore/gitignore implementation owners and the connection between traversal and ignore matching.

The checkpoint is **after request 16's tool results**. Those results appear in the outgoing `C/run_1/wire/request_17.json`; request 16's input alone does not include its newly retrieved Gitignore body. No request-17 generation or later evidence was included. All recorded messages through this boundary were preserved, including the unsuccessful native searches and the unsuccessful guessed-symbol search returned during request 16.

| Available by checkpoint | Actual evidence reviewed | Supports |
|---|---|---|
| Request 4 investigation | `HiArgs::walk_builder`, `core/flags/hiargs.rs:868–911` | CLI configuration of ignore/traversal options and `.rgignore` |
| Request 6 symbol read | First 120 source lines of `impl Ignore` | `add_parents`, `add_child`, beginning of `add_child_path`; **not** the whole declared 141–544 range |
| Request 12 read | `ignore/src/dir.rs:340–544`, 205 source lines | `matched_dir_entry`, `matched`, `matched_ignore`, override/type ordering and parent matchers |
| Request 13 read | `ignore/src/walk.rs:920–1080`, 161 source lines | `skip_entry`, iterator `next`, `add_child`/`parent` state transitions and `skip_current_dir` pruning |
| Request 14 search | `should_skip_entry` plus returned context | Calls `matched_dir_entry`; ignore match returns true. Requested after-context was capped at five lines |
| Request 16 read | `ignore/src/gitignore.rs:120–330`, 211 source lines | `Gitignore::matched`, `matched_stripped`, relative-path stripping and reversed glob selection |

The four selected standalone source payloads were checked as verbatim substrings of their snapshot files. Actual returned source was inspected rather than trusting symbol range metadata or outlines. The checkpoint supports a short, evidence-grounded answer covering owners, matching, scoped traversal state and pruning. Full matcher construction/parsing, parallel traversal and GlobSet internals are not fully established; an exhaustive implementation tour would require more evidence, but the original question does not require one.

## Isolated synthesis experiment

Exactly **one new generation request** was executed. A first sandboxed attempt failed during model discovery before any generation; the authorized endpoint request succeeded outside that network restriction. The failed setup directory is retained separately.

The replay copied all recorded messages and inference options, omitted tools/tool choice, and appended one tool-agnostic final-answer instruction: answer now from existing evidence in at most 300 words, prioritize essential files/symbols, the traversal-to-ignore connection and short execution flow, and state uncertainty. It supplied no new source or ground-truth answer. The native grep fix therefore cannot explain this replay's result: the original misleading empty results remained in its context.

Model: `qwen3.8-flash-next-coder-iq1_m`; temperature 0; `max_tokens=4096`; non-streaming; unchanged server-default reasoning policy and no added inference fields. API discovery before/after reported the same loaded model and context 131,072. Strata v0.1.44.1, IQ1_M, 8-bit KV cache, RX 7900 XTX 24 GB, Ryzen 9 9950X and 32 GB RAM remain the declared environment; engine/KV configuration was not independently inspected or changed. No restart or model download occurred.

| Metric | C's previous reserved synthesis, request 25 | New post-request-16 replay |
|---|---:|---:|
| Finish reason | length | **stop** |
| Accepted terminal protocol response | no | **yes** |
| Input tokens in this request | 45,694 | 27,224 |
| Output tokens, including any reported reasoning contribution | 4,096 | 617 |
| Total tokens in this request | 49,790 | 27,841 |
| Separate reasoning tokens | unavailable | unavailable |
| Answer content, UTF-8 bytes | 6,491, truncated draft | 1,745 |
| Reasoning content, UTF-8 bytes | 7,882 | 636 |
| Request execution time, seconds | 78.634 | 26.077 |
| New tools executed | 0 | 0 |
| Fully correct and complete task | no | **no: partial** |

Timing covers the generation request and wire logging, excluding model discovery and evidence audit. Both calls reported zero cached input tokens. The two rows have different histories and instructions: this is a descriptive comparison, **not** a controlled attribution of token/time changes solely to the concise prompt.

The model ended the single diagnostic response without truncation or another intervention. Because tools were explicitly disabled and synthesis explicitly requested, this is **prompted synthesis termination**, not proof of natural stopping during an unconstrained navigation run. Reasoning did not exhaust the allowance: only 617 completion tokens were reported in total and visible answer content was delivered. Exact reasoning-token counts/fractions and tool-response token counts remain unavailable; reasoning bytes are not token estimates and are not added to totals.

The new request consumed 27,841 tokens. The historical C requests 1–16 had already consumed 284,554 tokens: 281,390 input plus 3,164 output. They were not re-executed or charged as new diagnostic requests. A retrospective hypothetical prefix-plus-replay ledger would be 312,395 tokens; it is **not** this experiment's measured cost or a completed navigation run. C's original 25-request session total remains 653,409 tokens. These cumulative sums must not be confused with the replay's actual 27,224-token input context; input plus the 4,096 allowance fits the unchanged context window.

### Answer review

The delivered answer identifies four real owning files and the forward chain from `HiArgs::walk_builder` through `Walk::skip_entry`/`should_skip_entry`, `Ignore::matched_dir_entry`/`matched_ignore`, and `Gitignore::matched`/`matched_stripped`. It describes override/type handling, parent matchers and last-glob selection correctly at its stated level, and explicitly marks parallel traversal and GlobSet internals as uninspected.

However, it ends at selecting the matching glob. It does not explain how that result skips an entry or prunes an ignored directory, or how entering/exiting directories updates/restores the scoped Ignore matcher. Merely listing `add_child` and `add_parents` does not explain that relationship. Both mechanisms were present in the checkpoint. Under the existing requirement for a complete causal explanation, the conservative manual score is **0.5**; incomplete output is not counted as success.

The unchanged automated grounding scorer gives **1.0**: four grounded files, 24 grounded extracted symbols, zero ungrounded references. This demonstrates why reference precision must remain a secondary metric: it does not establish execution-flow completeness. The review is repository-grounded agent review, without an independent second reviewer.

Sparse host VRAM samples were 23,740,125,184 bytes before and 23,744,339,968 after, out of 25,753,026,560 reported bytes. No loaded-model metadata change was observed. These endpoint samples cannot exclude transient pressure or unobserved server changes; no causal hardware or quantization claim is supported.

## Stopping-guidance review

Neither benchmark completion guidance nor `skills/pitlane-navigation/SKILL.md` was changed. A/B's base instructions remain identical; C alone adds the original skill. The diagnostic's 300-word instruction has **not** been promoted into the benchmark profile.

Confirmed shared guidance already says to collect enough evidence, answer without tools, avoid searches that do not resolve a remaining uncertainty, and report missing evidence instead of exploring indefinitely. The reserved turn also forbids tools. Neither gives a concrete coverage check or output-length priority. These omissions are plausible contributors to the earlier broad drafts; they do not prove a private reasoning cause.

Confirmed skill guidance already says to stop once enough named files/symbols support an answer, expand only as necessary and prefer a few relevant symbols over exhaustive dumps. It does **not** require endless searches or tool switching. Its suggested entry → orchestration → scanning → matching → output path is qualified by “as relevant,” but may still invite an overly broad tour for a narrow subsystem question. Naming symbols alone can also appear sufficient without explaining decisions and state changes; the replay illustrates that gap.

The recorded C trajectory switches from focused MCP reads into six native file searches, then back to MCP. Four of those native calls had real matches hidden by the bug. After the request-16 checkpoint it continues into parser, parallel walker and CLI details through request 24. This establishes additional exploration after enough core evidence was available; it does not establish that every later call was useless. Narrower reads sometimes add evidence. The skill permits native exact-string lookups and fallback for failing/unhelpful MCP; it does not explicitly encourage repeated alternation.

MCP response guidance is another shared B/C influence: the request-14 content search recommends further navigation and references advanced primitive names despite default-tier exposure. Such tool-output steering can compete with a stopping rule. No production tool or steering behavior was changed here.

### Proposals for review, not applied

**General completion guidance, identical in A/B/C:** require a compact coverage check before each further search: identify owners, connect the essential operations, explain the observable effect of returned decisions/state changes, and cover every requested part or explicitly mark missing evidence. Once covered, synthesize. Before an additional call, identify the specific unresolved question it will answer. Prefer a short causal explanation over a symbol inventory. Any answer-length guidance must preserve completeness across all five workloads, rather than treating 300 words as proven suitable for every task.

**Skill-only candidates for a separate later experiment:** make the minimal task-specific path the default rather than a full entry-to-output tour; explicitly reassess coverage after a focused read; distinguish discovery from causal explanation; and limit repeated native/MCP switching when it adds no new evidence. Test such a revision separately against the unchanged skill with the same common guidance. Do not change the skill and common prompt together and attribute the result solely to the skill.

## Findings, limits and next experiment

**Confirmed:** native file searches had false negatives; the shared fix returns correct results for all 17 recorded file calls. The preserved C checkpoint contains the core traversal/matching evidence. This model can produce a concise terminal answer from that context within the original output allowance. The resulting answer is factually grounded but incomplete, and the historical gate still fails.

**Hypotheses:** concise prioritization may reduce synthesis verbosity; a concrete causal-coverage rule may improve completeness and autonomous stopping; earlier synthesis may reduce distraction. The replay changes both checkpoint/history and instruction relative to the original final call, so it does not isolate these effects. There is no demonstrated context overflow, quantization cause, or reason to increase iterations/output allowance.

Recommended next experiment, after review: use the **same preserved post-request-16 context** for a small tools-disabled comparison of the current generic final instruction versus a concise instruction with explicit causal coverage. Keep all inference settings fixed, review returned decisions/state effects against existing evidence, and preserve separate traces. This would test prompt effects at a matched context before committing to another smoke. A repeated sample would be needed before claiming reliability. Do not reuse the new replay as a benchmark success or readiness authorization.

Only after reviewing those diagnostics should a new three-arm smoke test the repaired native tool and any approved **common** guidance from fresh sessions. Leave the skill unchanged for that test, measure natural versus reserved completion, and require fully correct answers in every arm. The present evidence cannot establish skill benefit, cross-arm success, or successful-task efficiency. No full benchmark is authorized by this report.

## Reproduction and retained artifacts

```sh
.venv/bin/python -m pytest bench/harness/tests -q
# Set STRATA_BASE_URL at runtime; no endpoint or credential is embedded in source.
.venv/bin/python -m bench.harness.experiments.diagnose_synthesis \
  --source-run results/navigation-skill-smoke-20261018-3 \
  --arm C --checkpoint 16 --env-file /path/to/strata/.env \
  --out results/new-synthesis-diagnostic
```

See [diagnostic execution instructions](experiments/README.md#recorded-evidence-synthesis-diagnostic). Successful local artifacts are in `results/navigation-skill-synthesis-20261018-2/`: manifest, unchanged-checkpoint wire messages/new response, observations, summary, manual review, evidence audit, native regression replay, environment samples and source integrity check. The separate `...-1/` directory contains only the failed sandbox setup's empty wire directory; no generation was executed there.

The entire original smoke artifact tree hashes identically before/after (`5e3674bb4f09d6d21a25bb7c7b18afafcf133e4080e43466581acf4ec71db733`), including the repository snapshot and original traces. New artifacts are ignored by Git. Authentication headers are never captured. Raw conversations, source payloads and reasoning remain local; this report contains aggregate metrics and review findings only. The benchmark profile/common instructions, SKILL.md and production Rust code remain unchanged. No commit, push, PR, three-arm smoke or full benchmark was performed. Work stops for user review.
