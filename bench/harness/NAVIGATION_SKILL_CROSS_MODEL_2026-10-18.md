# LM Studio cross-model synthesis diagnostic — 2026-10-18

**All four Qwen3.8-27B UD-Q4_K_M requests terminated, but the same four-concept diagnostic remains partial.** The answers correctly connect directory pruning and matcher-state changes. They do not fully establish hidden-file conditions or ignore-file/type-filter precedence. The earlier hidden-method attribution error was not repeated, but omission is not proof of corrected understanding. No navigation readiness pass or skill benefit is established.

For a proposed second smoke, use the existing **`symbol_cli_config_flow`** task. It has a more linear parsing-to-configuration handoff and avoids the ignore subsystem's interacting scope/precedence conditions. No smoke was run, and no task/completion instruction was tuned.

## Model and endpoint verification

The operator downloaded/prepared and manually loaded the exact requested instance:

`unsloth/qwen3.8-27b-gguf/qwen3.8-27b-ud-q4_k_m.gguf`

Initial discovery had found a different Q3_K_S instance loaded and another publisher's Q4_K_M on disk. Neither was used for generation. The operator's final supplied instance is the sole model used below. The assistant issued no download, load, unload, server-start or inference-setting mutation commands. Initial read-oriented CLI discovery unexpectedly printed that it was waking LM Studio's service; CLI discovery was discontinued. All subsequent checks used the operator-provided API and local file reads.

| Check | Measured finding |
|---|---|
| API reachability | Existing localhost LM Studio endpoint responds to `/v1/models`, `/api/v0/models` and `/api/v1/models` |
| Exact loaded identity | Supplied instance ID matches native loaded-instance metadata and every generation response's model field |
| Quantization | Both native API versions report **Q4_K_M**; this is the operator-selected Unsloth **UD** variant |
| Local file | `Qwen3.8-27B-UD-Q4_K_M.gguf`, **16,464,440,224 bytes**, exists locally |
| GGUF identity | `general.name=Qwen3.8-27B`, architecture `qwen35`, file type 15 |
| Context support | API and GGUF maximum **262144**; actual loaded context **131072**, matching the controlled context size |
| Tool interface | One actual structured function-call/result/final-answer round trip passed |
| Usage reporting | Input, output, total and separate reasoning-token counts are present |
| Stability | Exact selected model, loaded context and native load configuration matched before/after all runs |

Loaded configuration reports flash attention, GPU KV offload, MTP speculative drafting, one parallel slot, evaluation batch 2048 and physical batch 512. KV **bit width** and exact LM Studio application/runtime build were not exposed by the queried metadata and are unavailable; Strata's declared 8-bit KV setting must not be assumed for LM Studio. Observed host VRAM samples are retained locally. Hardware is the same user-declared RX 7900 XTX / Ryzen 9 9950X / 32 GB system; no claim is made that the two runtimes have identical resource pressure or kernels.

Sparse host VRAM samples ranged from **25,231,097,856 to 25,433,387,008 bytes**, higher than the prior Strata evidence diagnostic's 23,741,288,448–23,809,527,808 bytes. These samples do not establish transient peaks, thrashing or a causal performance effect; no memory failure was reported by the diagnostic.

The native [model-list API](https://lmstudio.ai/docs/developer/rest/list) supplies loaded-instance/context/capability metadata. Metadata alone was not accepted as proof of tool calling: the strict existing provider exercised the documented [structured tool-call interface](https://lmstudio.ai/docs/developer/openai-compat/tools).

### Reasoning configuration

The native API advertises **on/off**, default **on**, for this Unsloth variant. The existing GGUF chat template enables thinking when unspecified and declares a default `reasoning_effort` of **xhigh**, with medium/low branches. This was a CPU-only metadata/header read; tensor payloads were not loaded by the diagnostic script.

No reasoning override was sent. Every response contains separate, nonempty `reasoning_content`, confirming that thinking was active. LM Studio does **not** expose the effective per-request template arguments in the captured API response, so the precise applied effort level cannot be independently certified as xhigh. The combination of native-on metadata and an xhigh-default template is documented, not promoted into a stronger equivalence claim with Strata. No reasoning settings or developer options were changed.

The model-appropriate policy is therefore **retain the existing native reasoning default**, record the limitation, and use the provider's actual reasoning-token reporting. The model/server/quantization and possibly effective reasoning policy differ from the Strata profile; this is a model-profile diagnostic, not an isolated quantization or reasoning-effort experiment.

## Preflight

Before repository synthesis, the model received the existing synthetic `benchmark_echo` probe. It returned one structured call with the correct JSON nonce, consumed the supplied tool result and returned the nonce as a final answer. No argument repair or retry occurred. The finish reasons were `tool_calls` then `stop`.

| Preflight request | Input | Output | Reasoning subset | Total |
|---|---:|---:|---:|---:|
| Tool request | 329 | 81 | 44 | 410 |
| Result → answer | 390 | 28 | 16 | 418 |

This is one synthetic round trip, not a reliability guarantee or a test of PitlaneMCP integration. Its **828 total tokens** are excluded from the synthesis table below. Two preflight generation requests plus four synthesis requests were executed.

## Fixed evidence and instructions

Each synthesis request was copied from the already reviewed `results/navigation-skill-evidence-presentation-20261018-1/<presentation>_<repeat>/wire/request_01.json`. **Only the model identifier changed.** Original task, system prompt/skill, accumulated or compact evidence, completion guidance, temperature **0**, non-streaming mode and **4096 output-token allowance** were preserved exactly. No new facts, questions, explanations or corrected source were added.

The evidence remains ripgrep `4649aa9700619f94cf9c66876e9549d83420e16c`, C's checkpoint immediately after request 16. The compact collection is the same reviewed extraction of 86 recorded source/signature snippets; it was not reselected for the new model. Both inputs retain the historical C system prompt and unchanged skill; this does not test A/B/C navigation treatment differences.

Fresh requests ran sequentially in the same balanced order: **accumulated 1 → compact 1 → compact 2 → accumulated 2**. No answer from an earlier request was appended. All requests have tools absent; no native or MCP calls were executed during synthesis. The normal inference timeout remained 300 seconds. Verification, local metadata reads and grading are outside measured request time.

The full input plus output allowance is 27,249 + 4,096 tokens; compact is 13,770 + 4,096. Both fit the actual loaded context with ample room. Preserved wire-message equality and full-context capacity were verified; no input truncation or context overflow was observed.

## Results

All token columns below are **LM Studio's model-native reported units**, not equivalent costs across models. Reasoning is a subset of output and is not added a second time.

| Presentation / repeat | Input | Output | Reasoning subset | Total | Finish | Time (s) | Grounding | Manual diagnostic score |
|---|---:|---:|---:|---:|---|---:|---:|---:|
| Accumulated 1 | 27,249 | 2,604 | 2,020 | 29,853 | stop | 73.227 | 1.0 | 0.5 |
| Compact 1 | 13,770 | 1,558 | 948 | 15,328 | stop | 38.035 | 1.0 | 0.5 |
| Compact 2 | 13,770 | 1,558 | 948 | 15,328 | stop | 24.801 | 1.0 | 0.5 |
| Accumulated 2 | 27,249 | 2,604 | 2,020 | 29,853 | stop | 43.127 | 1.0 | 0.5 |

Synthesis consumption: **82,038 input + 8,324 output = 90,362 total tokens**, including **5,936 reported reasoning tokens** within output. With preflight, total new consumption is **91,190 tokens**. No historical requests were rerun or charged as new requests. Cache-token counts are unavailable from these responses; draft-token statistics are recorded separately and are not added to usage totals.

| Presentation, n=2 | Mean/median native total tokens | Mean/median time (s) | Time sample SD (s) | Four concepts fully demonstrated |
|---|---:|---:|---:|---:|
| Accumulated | 29,853 | 58.177 | 21.284 | 0/2 |
| Compact | 15,328 | 31.418 | 9.357 | 0/2 |

Visible answers are identical within each presentation, with distinct request IDs/timestamps. Accumulated answers have 239 whitespace-delimited words; compact answers have 234. Two identical greedy repetitions do not establish general reliability. Latency varies substantially, and cache/warm-up state was not reset; no reliable speed ranking or statistical significance is claimed. No test suite ran during timed inference.

These are **prompted synthesis** completions after an explicit final instruction with tools disabled, not natural navigation termination. Successful-only efficiency under the four-concept diagnostic has n=0.

## Correctness review against the same repository

The reviewer-only rubric is unchanged from the evidence-presentation diagnostic. It evaluates hidden-file handling, rule precedence, directory pruning and matcher-state transitions. It was not inserted into the broad original task. Missing detail is recorded as incomplete/unaddressed, not automatically as a false belief. These are diagnostic scores, not retroactive rescores of historical navigation runs.

| Presentation, both repeats identical | Hidden-file handling | Rule precedence | Pruning | Matcher state |
|---|---|---|---|---|
| Accumulated | incomplete: a following check is mentioned, but not the no-prior-match condition | incomplete: correct category ordering, but ignore-exclusion/type-filter short-circuit omitted | correct | correct |
| Compact | unaddressed in matching explanation | incomplete: overrides and last-glob semantics, but ignore/type relationship omitted | correct | correct |

`dir.rs:350–359` shows that `matched_dir_entry` applies hidden exclusion only after `matched` returns None. `matched` at 365–409 returns override matches immediately and returns ignore-file exclusions before consulting types. These full bodies are present in both evidence inputs. The new answers do not fully state those relationships.

Both presentations correctly connect `Walk::next`, `skip_entry`, `should_skip_entry` and `matched_dir_entry`, directory pruning, child matcher construction and restoring `parent()` on Exit. The compact answer also describes the balancing child push for skipped directories. These claims agree with `walk.rs:932–1040` and the returned helper context. Gitignore root-relative matching and last-glob selection agree with `gitignore.rs:188–268`.

The accumulated answer correctly reports the fixed category order shown by the `.or()` chain, improving on the specific earlier IQ1_M claim that `.or()` makes nearer rules universally win. Neither new answer repeats the earlier assertion that type filters override ignore-file exclusions, nor explicitly assigns the hidden check to `Ignore::matched`. **Those omitted details cannot be counted as demonstrated corrections.**

An additional precision caveat appears in the compact answer: it groups `.hidden()` with `--no-ignore`-style switches. The supplied `HiArgs::walk_builder` configures hidden behavior from `self.hidden` separately. Actual `NoIgnore::update` (`crates/core/flags/defs.rs:4226–4271`, checked only during grading) sets `no_ignore_*` fields, not hidden. This is imprecise CLI attribution; no corrected flag explanation was fed back into the model.

Automated grounding is 1.0 because the referenced files/symbols exist. It does not validate conditions, precedence, exact line citations or completeness. Manual review remains **partial (0.5)** for every answer under the same four-concept rubric. This is repository-grounded agent review without an independent second reviewer. No answer is counted as full diagnostic success.

## Comparison with the prior Strata diagnostic

| Observation | Flash-Next-Coder IQ1_M / Strata | 27B UD-Q4_K_M / LM Studio |
|---|---|---|
| Input task/evidence/final instructions | fixed accumulated/compact reference | identical message payloads; model ID only changed |
| Terminated synthesis | 4/4 | 4/4 |
| Full four-concept diagnostic success | 0/4 | 0/4 |
| Manual score | 0.5 throughout | 0.5 throughout |
| Pruning/state | correctly connected | correctly connected |
| Prior hidden-method false attribution | appeared in compact repeat 2 | not repeated, but condition omitted or topic unaddressed |
| Reasoning-token telemetry | unavailable | reported separately within output |
| Repeated visible answers | varied in the sampled requests | identical within each presentation |

The cross-model run does not establish a correctness improvement on this diagnostic, an IQ1_M-specific cause, or an effect of the navigation skill. A different model/quantization/runtime may select different details while still leaving required understanding unproven. The broad original question does not explicitly enumerate the four review topics, so omissions also reflect answer selection; they do not by themselves prove inability to reason about the supplied code.

**Raw cross-model token differences are not equivalent costs.** Input counts happen to match here, but model computation, quantization, runtime, speculative decoding, hardware utilization and potentially template/tokenizer behavior differ. No token-cost ratio or cost-based model winner is reported. The synthesis diagnostic is not a raw inference-performance benchmark.

## Simpler existing task for a proposed second smoke

Select **`symbol_cli_config_flow`**, category **`symbol_grounding`**, verbatim from `bench/harness/prompts/ripgrep.jsonl`:

> Locate where ripgrep parses command-line flags into runtime configuration. Show the main files and explain how parsed options flow into the search engine.

Keep its existing grounding suffix unchanged. It is a real suite prompt, not a newly constructed task or a narrowed rewrite of `symbol_ignore_logic`.

Reviewer source anchors were verified against the same ripgrep commit:

- `crates/core/main.rs:44,77`: executable calls `run(flags::parse())`, receiving `ParseResult<HiArgs>`.
- `crates/core/flags/parse.rs:48–56,64–113,221–290`: `parse`, `parse_low` and `Parser::parse` fill `LowArgs`, then use `HiArgs::from_low_args`.
- `crates/core/flags/hiargs.rs:112` and `685–719`: typed conversion and `HiArgs::search_worker`/`searcher` hand options into search builders.
- `crates/core/main.rs:160–184`: `search_parallel` consumes those builders to obtain the matcher, searcher, printer and search worker.

These are **review-only anchors**, not proposed additions to agent prompts. The task offers concrete named transformation boundaries in a mostly linear path, rather than a cross-product of hierarchical scopes, whitelists, categories and traversal-state exceptions. It remains a real discovery-and-explanation task: a correct answer must reach the search-configuration handoff, not merely find argument parsing. Native and MCP navigation can both solve it; neither tool selection nor a server-specific sequence should be part of correctness.

Relative simplicity is a source-based hypothesis, not measured agent performance. The prompt still has configuration-file and special-mode branches, so it is not trivial. It is a suitable next **instrumentation/completion smoke candidate**, not evidence that the broader ignore workload is solved. Do not add an exhaustive per-flag tour, alter completion instructions, weaken scoring or infer full-suite readiness from this smaller task. No second smoke was executed.

## Artifacts, reproduction and stopping point

```sh
# Set LM_STUDIO_BASE_URL to the existing /v1 endpoint at runtime.
.venv/bin/python -m pytest bench/harness/tests -q
.venv/bin/python -m bench.harness.experiments.compare_local_model \
  --model 'unsloth/qwen3.8-27b-gguf/qwen3.8-27b-ud-q4_k_m.gguf' \
  --model-file /path/to/existing/Qwen3.8-27B-UD-Q4_K_M.gguf \
  --source-run results/navigation-skill-evidence-presentation-20261018-1 \
  --out results/new-lmstudio-cross-model
```

**153 harness tests passed before inference**, including five new loaded-model/quantization/context, unchanged-payload and metadata-reader tests. The controller refuses an unloaded or wrong-quantization instance, so it cannot intentionally trigger JIT loading. The exact model state is checked before each request and afterward.

Local ignored artifacts: `results/navigation-skill-cross-model-lmstudio-20261018-1/`. They include native metadata, local GGUF metadata/template, preflight wire data and observations, four synthesis wire traces/raw summaries, environment samples, source-integrity checks, structured manual reviews/CSV/JSONL, summary statistics and `second_smoke_task_proposal.json`. Raw reasoning/conversations/source remain local; authentication headers are not captured. Raw summaries are preserved separately from annotations.

Both the complete previous evidence-diagnostic artifact tree and the original smoke/checkpoint tree hash identically before/after. SKILL.md, completion guidance, provisional benchmark model config and production Rust remain unchanged. No assistant model download, inference-setting change, three-arm smoke, full benchmark, commit, push or PR occurred. Results and the next-task proposal stop here for review.
