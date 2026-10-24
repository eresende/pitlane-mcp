# Navigation skill smoke retest — 2026-10-18

**Readiness gate: FAIL.** The approved three-arm smoke completed execution, but no arm delivered an accepted final answer. All three reached the reserved final request, and all three responses were truncated at the unchanged 4,096-output-token allowance. There were no natural completions and no accepted budget-prompted completions. The 45-run benchmark was not started.

## Method and environment

- Existing model: `qwen3.8-flash-next-coder-iq1_m`; requested quantization `iq1_m`. API discovery reported it loaded with `131072` context.
- Strata v0.1.44.1, 8-bit KV cache, Radeon RX 7900 XTX 24 GB, Ryzen 9 9950X and 32 GB RAM remain the user-declared environment. Engine/KV settings were not independently read or modified. No inference-server restart or model download was performed.
- Target: ripgrep `4649aa9700619f94cf9c66876e9549d83420e16c`; the tracked-file snapshot, target HEAD/status/content, prompt-set hash and skill hash match the previous measured smoke.
- Same existing task: symbol_ignore_logic, one repetition in each of A native, B native+MCP and C native+MCP+skill. Fresh conversations and B/C MCP sessions; seeded order C → B → A, matching the prior smoke.
- Temperature 0, server-default reasoning request policy, 4,096 output-token allowance, 25 model requests maximum and 300-second timeout per task are unchanged. The common completion guidance and request-25 synthesis policy are identical in A/B/C. A/B base prompts match byte for byte; only C appends the exact skill. B/C tool schemas match. A exposes only the three native tools.
- Cold MCP setup, including embeddings, took 13.564s and is excluded from navigation time. PitlaneMCP remains 0.17.1 with the same configured remote Qwen/Qwen3-Embedding-8B embeddings; task navigation started only after readiness.
- Detailed wire/conversation traces were enabled equally for all arms, locally under the ignored results directory. Authentication headers are never captured. Unlike the earlier smoke, navigation time includes wire-log disk I/O; parsed-result serialization is outside the timer. Temporal/cache/path differences and logging mean this is a descriptive comparison, not a pure single-factor timing experiment.
- Run identity: `5999c54bc8e41891d0fa3a0fcbefd2281386eb9eb61707f93aa40d1a903824c5`. The manifest pins config, source hashes, endpoint fingerprint, embedding settings and trace policy. Source changes remain uncommitted.

## Natural versus budget-prompted termination

All 75 task wire requests and responses were inspected. For each arm, requests 1–24 returned structured tool calls. Request 25 omitted tools and contained the same generic final-response instruction. It returned finish_reason=length with completion_tokens=4096, rather than a successful terminal response. The policy did not issue a 26th request, silently retry, relax scoring or increase an inference limit.

| Arm | Model requests | Natural completion | Reserved synthesis attempted | Accepted completion | Final result |
|---|---:|---|---|---|---|
| A | 25 | no | yes | no | output_truncated |
| B | 25 | no | yes | no | output_truncated |
| C | 25 | no | yes | no | output_truncated |

| Arm | Final input tokens | Final output tokens | Assistant answer text (UTF-8 bytes) | Reasoning text (UTF-8 bytes) | Diagnostic observation |
|---|---:|---:|---:|---:|---|
| A | 65,721 | 4,096 | 782 | 12,805 | Started a correct central-file map, then cut off mid-list before explaining the requested connection. |
| B | 45,529 | 4,096 | 0 | 16,184 | No assistant answer content was emitted; reasoning text was present when the output limit was reached. |
| C | 45,694 | 4,096 | 6,491 | 7,882 | Produced a lengthy partial draft but cut off during the central traversal-to-matching explanation. |

Raw reasoning is retained only in local traces and is not reproduced here. Separate reasoning-token counts were not reported. The byte sizes above are not token estimates. Reasoning and visible answer generation share the request’s reported completion allowance; it would be incorrect to add reasoning bytes or an inferred reasoning-token count to total tokens.

## Correctness and readiness

The existing strict provider rejected finish_reason=length. All accepted final_answer fields remain empty; task correctness and historical grounding scores are 0. A/C drafts contain useful repository references, but are incomplete and are not substituted for successful final answers. B produced no answer content. Manual review records these delivery failures, rather than asserting that every retrieved fact was false. Successful-only efficiency has n=0 in every arm.

11/12 technical checks passed. Only all_completed failed. Schema/transport checks, actual C-only skill presence, A’s absence of MCP access, common task/base prompts, identical B/C schemas, token availability, context verification and original/snapshot integrity passed. The technical gate and reviewed correctness gate are both false. Passing isolation and protocol checks alone does not authorize a full benchmark.

## Comparison against the previous measured smoke

Previous: results/navigation-skill-smoke-20261018-2/. Current: results/navigation-skill-smoke-20261018-3/. Each arm has n=1, so averages and medians are identical and sample standard deviations/repeat variability are unavailable. All prior runs ended max_iterations; all current runs end error/output_truncated. Correctness/grounding stay 0 throughout.

| Arm | Previous → current total tokens | Token change | Previous → current time (s) | Previous → current tool attempts | Previous → current schema-invalid calls |
|---|---:|---:|---:|---:|---:|
| A | 1,103,450 → 1,168,834 | +5.93% | 145.283 → 237.411 | 38 → 38 | 0 → 0 |
| B | 880,502 → 631,950 | -28.23% | 123.553 → 175.249 | 46 → 24 | 2 → 0 |
| C | 431,010 → 653,409 | +51.60% | 74.062 → 186.584 | 25 → 42 | 0 → 0 |

| Arm | Current cumulative input | Current cumulative output | Current cumulative total | Native / MCP attempts | Tool-response bytes |
|---|---:|---:|---:|---|---:|
| A | 1,158,261 | 10,573 | 1,168,834 | 38 / 0 | 235,734 |
| B | 624,443 | 7,507 | 631,950 | 8 / 16 | 162,662 |
| C | 644,112 | 9,297 | 653,409 | 10 / 32 | 147,199 |

All current schema-invalid calls, failed tool transports, provider protocol errors and exact retries after identical failures were 0. Output truncation is an agent/provider-generation failure, distinct from a failed tool call. The schema/transport success rate does not capture all ineffective tool uses, as discussed below.

| Current comparison, right minus left | Quality mean/median delta | Cumulative token delta | Navigation-time delta (s) |
|---|---:|---:|---:|
| A vs B | 0 | -536,884 | -62.161 |
| B vs C | 0 | +21,459 | +11.335 |
| A vs C | 0 | -515,425 | -50.827 |

C used 21,459 more tokens than B in this retest (about 3.40%) and made 42 attempts versus B’s 24. This sample does not demonstrate a skill benefit. Conversely, B/C using fewer tokens than A is not validated successful-task efficiency: all three failed. The old-to-new token/time changes include different search trajectories, synthesis work and trace overhead; neither improvement nor regression should be generalized from this single task/repetition.

## Tool-selection behavior and local investigation

| Arm | Recorded behavior | Confirmed ineffective native file-targeted searches |
|---|---|---:|
| A | 27 grep searches, 7 file reads, 4 listings; a long traversal-oriented search tail | 10 |
| B | 7 focused MCP reads, 6 content searches, 1 investigation, 2 native full-file reads, 5 listings and 1 native grep | 1 |
| C | 17 focused MCP reads, 11 content searches, 1 investigation, 1 discovery call, 3 listings and 7 native grep calls | 6 |

The native grep tool documents a directory path, but the agent repeatedly supplied source-file paths. The implementation walks directories; for a file it silently returns No matches found. A local deterministic check, using the same recorded regex against dir.rs, returned no matches for the file path but found actual dir.rs matches under the containing directory. This is a demonstrated tool-contract/validation gap and an ineffective invocation pattern in every arm, not proof that those source symbols are absent. These calls passed JSON Schema and were therefore not counted as invalid arguments by the existing metric. No tool behavior was changed during the smoke.

A also repeated the same should_skip_entry search with a narrower directory scope; both successful outputs were identical. That is one confirmed semantically redundant search even though argument hashes differ. No exact successful-call hashes repeated in any current arm. Other search overlap is not automatically labelled waste: a narrower query or focused read may add relevant evidence. Overall unnecessary-MCP and semantic-redundancy totals are left unassigned rather than guessed; confirmed lower bounds and call details are retained in local review/analysis artifacts.

B/C generally found relevant owners and used investigation, content search and focused reads. Their overall navigation was mixed: B fell back to two large native reads; C’s early discovery query was a keyword bag, then it fell back to multiple ineffective native searches despite available MCP tools. C had read Ignore matching, Walk traversal and Gitignore matching by request 16, yet continued through request 24 into parallel-walker and CLI details. This supports an over-exploration/late-synthesis concern; it does not establish the model’s private cause or a universal minimum sufficient call count. Binary appropriate-selection judgments are not forced onto this mixed trajectory; the qualitative evidence is preserved.

## Context and other environmental observations

| Arm | Peak input tokens in one request | Share of 131,072 context | Final cached tokens |
|---|---:|---:|---:|
| A | 65,721 | 50.1% | 0 |
| B | 48,936 | 37.3% | 0 |
| C | 48,773 | 37.2% | 0 |

Cumulative input sums repeated history across 25 calls; it is not the size of one context. Every peak input plus the configured 4,096-output-token allowance fits within the reported context. The recorded failures are generation-limit exhaustion, not demonstrated context overflow. Attention dilution remains a hypothesis without a controlled ablation.

All reserved final calls reported cached_tokens=0 after tool definitions were omitted; earlier calls frequently reported cached prefixes. A changed formatted prompt prefix is a plausible explanation, but cache implementation details were not independently verified. These cache counts are preserved and never subtracted from input-token totals.

Model-list metadata was identical before/after each run. Observed host VRAM usage ranged from 23,673,860,096 to 23,778,295,808 bytes. These sparse host samples cannot exclude transient pressure or independently verify server KV/engine settings. No model-loading or configuration-change event was observed by the harness; direct server event monitoring is unavailable. No cause is assigned to quantization.

## Recommendations and stopping point

The shared final turn worked as a control-flow boundary, but it did not make the broad task reliably finish within the unchanged output allowance. The direct confirmed failure is finish_reason=length. A/C were verbose and incomplete; B exhausted the generation allowance before emitting answer content. The detailed traces now expose this distinction, which was unavailable in the original smoke.

Before another benchmark attempt, review two separate possible follow-ups: (1) common concise final-answer guidance that prioritizes the requested connection and key evidence instead of an exhaustive implementation tour; (2) consistent native search handling of file paths, either explicit rejection or actual single-file support, with tests. Apply any chosen navigation changes identically across arms. Neither fix was applied opportunistically during or after this smoke. A concise-output prompt is a hypothesis to test, not a proven remedy.

Do not automatically raise max_iterations, increase max_tokens, change reasoning configuration, swap models, salvage truncated drafts as success, or launch the full suite. Any future inference-setting change needs a separately reviewed profile and fresh matching smoke. The current readiness gate remains failed, and work stops here for user review.

## Reproduction and local artifacts

```sh
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase smoke --capture-traces --env-file /path/to/strata/.env \
  --out results/skill-smoke-retest
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase report --out results/skill-smoke-retest
.venv/bin/python -m pytest bench/harness/tests -q
```

Set STRATA_BASE_URL at runtime; no local IP address or credential is committed. Use a new ignored artifact directory for each execution. See [execution instructions](experiments/README.md).

Current local artifacts: results/navigation-skill-smoke-20261018-3/. In addition to the manifest, readiness metadata, reviews, CSV/JSONL and standard report, this directory contains comparison_previous_smoke.json, failure_analysis.json, native_search_diagnosis.json, preflight-wire/, and instances/.../wire/, observations.json and parsed_result.json. Raw source, conversations and reasoning remain in ignored local artifacts only. This Markdown report contains aggregate results and analysis, not raw conversations or reasoning.

Automated validation: 115 harness tests passed before execution, including trace capture, credential/header exclusion, preserved wire payloads, shared completion policy and boundary tests. All 75 task request/response pairs, metric sums, shared final prompts, C-only skill loading and repository integrity were audited afterward. No production Rust changes, commits, pushes or full benchmark runs were made.
