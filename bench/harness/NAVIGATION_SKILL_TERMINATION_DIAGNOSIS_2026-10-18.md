# Navigation skill termination diagnosis — 2026-10-18

The most likely cross-arm problem is continued exploration under a weak completion contract, combined with a harness that cut off immediately at the request limit. The abrupt cutoff is confirmed in code. The model’s exact reasons for continuing the original broad task cannot be recovered because its raw responses were not retained. There is no evidence here that quantization caused the failure.

All three minimal scenarios terminated correctly both before and after the fix. Strata’s normal structured-tool and final-answer responses were interpreted correctly in these captured cases. A separate real two-request diagnostic verified the reserved-final-turn boundary. Another complete three-arm smoke and the full benchmark have **not** been run.

## 1. Completion handling and Strata responses

`AgenticLoop.run` finishes when the parsed assistant message has nonempty content and no tool calls. Structured tool calls take precedence over accompanying assistant prose; prose alongside pending calls is not treated as the final answer. Before this change, every request up to and including request 25 could invoke tools. The loop executed those final tools and returned max_iterations without another model call to consume their results or synthesize an answer.

The experiment’s strict CompatibleProvider preserves finish_reason and structured tool calls, JSON-decodes valid argument strings, and rejects empty/unstructured responses and finish_reason=length. It does not promote reasoning_content into a final answer. Although the legacy loop has a reasoning fallback for other adapters, this provider never populates that fallback, so it was not active in the smoke.

The captured minimal responses show:
- Direct question: assistant content="5", finish_reason="stop", no tool calls; correctly marked completed.
- Native/MCP first response: content=null, reasoning_content contains the plan to read the file, structured tool_calls with function name, JSON argument string and call ID, finish_reason="tool_calls".
- Second response: nonempty assistant content identifies the package as ripgrep, finish_reason="stop", no tool calls; correctly recognized as completed.
- Outgoing assistant tool calls and tool-result messages preserve the matching tool_call_id. Requests use stream=false, with no forced tool_choice. No alternate response format or additional choice carrying an ignored answer was observed.
- Reasoning is separate from final content. It is captured in private diagnostic artifacts for inspection, but not mistaken for an answer or a separately reported reasoning-token count.

The original 75 raw model responses, their finish reasons, assistant text and reasoning were not saved. It is therefore impossible to prove retrospectively whether any response contained answer-like prose alongside pending tool calls or answer-like text only inside reasoning. The recorded statuses and loop behavior imply that none was accepted as a terminal answer. The new tests show that the parser can recognize legitimate final responses; they do not prove every historical response was flawless.

## 2. Recorded behavior

| Arm | Model requests | Native / MCP attempts | Recorded pattern |
|---|---:|---|---|
| A | 25 | 38 / 0 | 4 listings, 5 file reads, 29 grep searches; long tail of searches |
| B | 25 | 18 / 28 | 20 focused MCP reads, 13 native grep searches, 6 MCP content searches, other discovery; two invalid content-search arguments |
| C | 25 | 6 / 19 | startup/stats, four early listings, investigation/discovery, ten focused reads and five content searches; two late listings |

Confirmed redundancy: A’s tool attempts 7 and 12 were identical read_file calls by name-and-argument hash, each returning 40,033 bytes. The snapshot was unchanged, so the second read repeated the same retrieval. B and C had no exact repeated argument hashes.

Potentially repetitive exploration is visible in A’s 29 grep calls, B’s repeated mixed native/MCP searches and reads, and C’s return to listings late in the run. Several native grep responses were only 17 bytes, consistent with the executor’s “No matches found.” response, but response bodies were not retained, so byte size alone is not proof. These are signs to investigate, not invented query-level diagnoses.

Semantic equivalence, exact unnecessary-call counts and whether enough relevant evidence had already been gathered cannot be established from names, sizes and hashes alone. C’s extra startup check ran after external pre-indexing, but a readiness check is allowed by the skill and cannot automatically be labelled a violation. The traces show no recognized synthesis; they do not prove a particular internal planning or reasoning loop. The minimal tests show that the model can plan one read and then answer.

## 3. Per-request context versus cumulative usage

| Arm | First input | Peak/last input | Share of reported 131,072 context | Peak output per request | Cumulative input over 25 requests |
|---|---:|---:|---:|---:|---:|
| A | 636 | 66,247 | 50.5% | 1,143 | 1,097,025 |
| B | 4,666 | 58,317 | 44.5% | 335 | 874,993 |
| C | 5,817 | 30,756 | 23.5% | 190 | 428,005 |

Input grew monotonically because the loop retained tool results and assistant/tool history. The million-token figure for A is a sum across 25 requests, not the size of one request. Even the largest reported input plus the configured 4,096-token output allowance fits comfortably within the reported context. No recorded output count reached that allowance; the strict provider would have classified finish_reason=length as an error.

Cached-token counts were reported and retained. They are not subtracted from model-input totals or used as a different accounting method. Reasoning-token counts remain unavailable; tool responses are UTF-8 bytes, not estimated tokens.

These data rule out a demonstrated context-capacity overflow. They do not rule out attention dilution or decision-quality degradation with long history. The short minimal cases cannot test that hypothesis; a controlled history ablation would be needed. No history truncation, model setting change or iteration increase is justified by the current evidence.

## 4. Common instructions and smallest bounded fix

The original neutral prompt requested repository evidence, exact files/symbols and uncertainty. It gave no explicit stopping criterion and no description of the finite request budget. The original task suffix likewise requested grounding, not a final-response boundary. The skill in C already encouraged stopping after sufficient evidence; that did not produce recognized completion in this one run.

The fix has two generic parts, identical in A/B/C:
1. Common completion guidance: gather enough evidence for the requested parts, answer without further tools, avoid repeating searches that do not resolve a specific uncertainty, and state missing evidence rather than explore indefinitely. It includes no MCP preference, repository symbol hints or task-specific answer guidance.
2. Opt-in reserve_final_turn: requests 1–24 may call tools; request 25, if reached, receives the same generic synthesis instruction and no tool definitions. This is inside the existing 25-request budget, not a 26th request. Earlier natural completion is unchanged. A tool request during the reserved response becomes an error and is not executed.

The experimental JSON enables this policy; legacy benchmark callers default to the old policy. Future rows record termination_kind so natural completion and budget-prompted synthesis can be compared separately, plus response metadata for finish reasons, field names and content/reasoning sizes. Model, quantization, context, sampling, output allowance, reasoning configuration, timeout and correctness scoring are unchanged.

Producing a final response remains distinct from completing the task correctly. Missing, incorrect or partial answers do not become successful merely because synthesis was requested. Manual repository-grounded review and the full-run gate remain in place.

## 5. Minimal diagnostic results

The existing qwen3.8-flash-next-coder-iq1_m endpoint and inference configuration were used. Every scenario had a fresh conversation, the same local agent loop and no navigation skill. The native case offered only read_file; the MCP case offered only read_code_unit, with the existing prebuilt index and a fresh MCP process. The package-name ground truth was obtained from the actual snapshot’s Cargo.toml: ripgrep. The simple task asks for that one fact; it does not establish completeness on the original broad ignore-navigation task.

| Scenario | Before fix | After fix | Requests before / after | Tool calls before / after | Final finish reason |
|---|---|---|---|---|---|
| no_tools | completed, correct | completed, correct | 1 / 1 | 0 / 0 | stop |
| one_native | completed, correct | completed, correct | 2 / 2 | 1 / 1 | stop |
| one_mcp | completed, correct | completed, correct | 2 / 2 | 1 / 1 | stop |

All six runs completed naturally, so these cases demonstrate parser/transport correctness but do not by themselves exercise the reserved budget boundary or prove the original task is fixed.

A separate one-native-tool diagnostic used a **lower diagnostic-only budget of 2 requests**: read Cargo.toml once, then synthesize on request 2. Raw request 2 omitted tools and included the generic final-response instruction; Strata returned the correct package name with finish_reason=stop and no tool calls. It completed in exactly two requests with one successful native tool call. The benchmark configuration remains max_iterations=25.

Automated validation: 114 harness tests passed. Regression tests cover the boundary at both 2 and 25 requests, refusal to execute tools during final synthesis, unchanged legacy behavior, correct handling of prose with pending tool calls, strict truncation/reasoning handling, shared A/B/C generic guidance, isolation, accounting and existing harness functionality.

## 6. Findings, hypotheses and recommendation

Confirmed: the old harness offered tools through its last request and then cut off; the model is capable of final responses before and after real native/MCP calls; current ordinary Strata responses are parsed correctly; original context did not exceed the reported capacity; A repeated one exact read; B produced two invalid tool calls; all original arms lack a recognized final answer.

Most likely explanation: broad exploration did not voluntarily end under the original completion contract, and the harness provided no bounded synthesis opportunity. This is an operational diagnosis supported by code and sequences, not a proven account of the model’s private reasoning. Semantic search loops, sufficiently complete early evidence, attention dilution and provider-specific historical anomalies remain unconfirmed. There is no evidence assigning blame to quantization.

Keep the iteration limit and model settings unchanged. Retain the common stopping guidance and reserved final response, label budget-prompted completion, retain private diagnostic traces and preserve strict correctness review. The next useful test is the complete three-arm smoke under that shared policy, with richer response metadata and argument logs, after user review. Its outcome may still be negative. Do not launch the 45-run benchmark based on these toy cases.

## Reproduction and artifacts

See [diagnostic instructions](experiments/README.md#termination-diagnostics-and-bounded-synthesis). Run from the repository root with STRATA_BASE_URL supplied at runtime:

```sh
.venv/bin/python -m bench.harness.experiments.diagnose_termination \
  --smoke results/navigation-skill-smoke-20261018-2 \
  --env-file /path/to/strata/.env --out results/termination-diagnostic
.venv/bin/python -m bench.harness.experiments.diagnose_termination \
  --smoke results/navigation-skill-smoke-20261018-2 \
  --env-file /path/to/strata/.env --case one_native --iteration-budget 2 \
  --out results/termination-boundary
.venv/bin/python -m pytest bench/harness/tests -q
```

Local artifact directories (ignored, not for version control):
- results/navigation-termination-diagnostic-before-20261018/: original neutral prompt, original budget policy, all three cases.
- results/navigation-termination-diagnostic-after-20261018/: revised common prompt and policy, all three cases.
- results/navigation-termination-budget-check-20261018/: separate two-request boundary verification.

Each directory records a manifest, per-case wire request bodies/responses without authentication headers, parsed loop results, observations and summary.json. The before manifest retains the exact original system template and config. Its responses are preserved rather than overwritten. Raw conversations/reasoning/source snippets remain local. The original smoke artifacts and scores are unchanged. The target checkout and snapshot remained unchanged. No inference server restart, model download, commit or push was performed.

Work stops here for user review before another complete three-arm smoke or the full benchmark.

## Subsequent approved smoke

After user review, the [three-arm retest](NAVIGATION_SKILL_SMOKE_RETEST_2026-10-18.md)
was executed with the proposed shared policy and detailed local traces. Its
readiness gate still failed because all reserved responses reached the unchanged
output limit. No full benchmark followed.
