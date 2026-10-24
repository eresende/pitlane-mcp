# CLI configuration navigation smoke — 2026-10-18

**The technical readiness gate passes. The correctness gate does not pass.** All three fresh attempts finished naturally, with `finish_reason=stop`, before the reserved synthesis turn. No tool call failed. All answers cover the requested parsing → runtime configuration → search flow, but each contains positive factual errors and receives the existing conservative manual score **0.5 (partial)**. No answer is counted as a full success.

Exactly one attempt ran per arm, in seeded order **C → B → A**. No retries, further smoke, full benchmark, commit, push or PR followed. These three observations do not establish a navigation-skill benefit.

## Fixed task and environment

The existing `symbol_cli_config_flow` prompt was used verbatim:

> Locate where ripgrep parses command-line flags into runtime configuration. Show the main files and explain how parsed options flow into the search engine.

Its original grounding suffix was also unchanged:

> Ground your answer in the repository. Name the exact files and symbols you used, and say clearly when something is not found.

No ground-truth symbols, source anchors, reviewer rubric or prior diagnostic answers were inserted into any agent input. Only `smoke_prompt_id` changed in the provisional configuration; the task suite, completion guidance and skill were not edited.

| Control | Value |
|---|---|
| Model / quantization | `qwen3.8-flash-next-coder-iq1_m` / IQ1_M |
| Engine | Existing Docker Strata **0.1.44.1** |
| Reasoning | Verified server-default **xhigh**, no request override |
| Sampling / output | Temperature **0**, `max_tokens=4096` per request |
| Context / iterations / timeout | **131072**, **25** including reserved final turn, **300 s** per navigation attempt |
| Common instructions | Existing neutral system prompt plus unchanged `concise-causal-v1`; identical across A/B/C |
| Target | ripgrep `4649aa9700619f94cf9c66876e9549d83420e16c` |
| Harness | Existing `AgenticLoop` experiment runner; strict compatible provider; dirty harness source hashes pinned locally |
| A | Three native tools; no MCP server or skill |
| B | Same native tools plus default-tier MCP; no skill |
| C | Same native/MCP schemas as B plus exact navigation skill system-prompt injection |
| Hardware | User-declared RX 7900 XTX 24 GB, Ryzen 9 9950X, 32 GB RAM, 8-bit KV |

The API reports the requested model loaded and context 131072. CPU-only rendering using deployed Docker code/config/tokenizer verifies the xhigh default. Rendered input counts for the first and final request in every arm exactly match live reported `prompt_tokens`. Shared defaults are empty. API settings, template, model metadata and process start identity match before/after. No server configuration, model load or restart command was issued. Effective effort is not echoed per response; the deployed rendering path and token-count cross-check are the verification mechanism.

The structured synthetic tool-call/result/final-answer preflight passed. Its two requests used **701 input + 164 output = 865 tokens**, excluded from navigation totals. Cold MCP setup, indexing and embedding readiness took **13.558 s**, also excluded. Navigation began only after all **101 files / 3207 symbols** and embeddings were ready. Setup used the existing remote embedding configuration and a preparation-only advanced tier; B/C agent sessions received identical default-tier schemas. Watchers were disabled.

Host VRAM samples during task runs ranged **23,747,571,712–23,838,658,560 bytes**. Strata's separate before/after device telemetry reported 22296/22685 MiB used; these are different observation surfaces, not interchangeable measurements. No memory error occurred. Sparse samples cannot establish transient pressure or peak use. Declared KV precision was not independently remeasured.

## Execution and accounting

| Arm | Completion | Final finish | Requests | Input tokens | Output tokens | Cumulative total | Time (s) |
|---|---|---|---:|---:|---:|---:|---:|
| A: native | Natural | stop | 15 | 583,179 | 4,365 | 587,544 | 126.449 |
| B: MCP | Natural | stop | 12 | 197,359 | 3,245 | 200,604 | 64.483 |
| C: MCP + skill | Natural | stop | 15 | 329,330 | 4,942 | 334,272 | 91.356 |

“Natural” means the model returned a final answer with tools still available, without receiving the reserved synthesis instruction. None reached request 25. Session output can exceed 4096 because that allowance applies separately to each request. The final requests used 1467/977/1628 output tokens in A/B/C; none returned `length`.

| Arm | Native / MCP calls | Failed / calls | Invalid arguments | Exact retries | Exact redundant successful calls | Tool-response bytes |
|---|---:|---:|---:|---:|---:|---:|
| A | 28 / 0 | 0 / 28 | 0 | 0 | 2 | 321,969 |
| B | 6 / 14 | 0 / 20 | 0 | 0 | 0 | 72,951 |
| C | 0 / 26 | 0 / 26 | 0 | 0 | 0 | 86,372 |

All observed tool-failure rates are **0%**; provider protocol errors and provider retries are also zero. These small denominators do not establish general reliability.

| Arm | Peak input in one request | Peak input + 4096 allowance | Final-answer whitespace words |
|---|---:|---:|---:|
| A | 90,004 | 94,100 | 242 |
| B | 28,476 | 32,572 | 318 |
| C | 33,706 | 37,802 | 396 |

All requests fit the context window. Cumulative input includes repeated conversation history; **587,544 session tokens do not imply a 587,544-token context**. Provider-reported totals sum input and output; reasoning-token counts are **unavailable**, not estimated or added. Nonempty separate reasoning text is retained locally, but byte length is not a token count. Tool responses are measured as UTF-8 bytes; tool-response tokens are unavailable and are not added again to provider input totals.

The three navigation sessions consumed **1,109,868 input + 12,552 output = 1,122,420 total tokens**. Including preflight, new provider consumption is **1,123,285**. Indexing/embedding tokens are not reported by this agent token counter. Request timing includes tool execution and local wire-trace writes, excluding preflight, cold indexing, session startup, metadata observations and manual grading. B/C exceeded the shared 300-word guidance; that is recorded separately from factual correctness.

## Independent correctness review

Review used the unchanged ripgrep source after generation. The three dimensions below are separate: real references, coverage of requested components, and accuracy of claimed relationships. No exhaustive CLI-flag explanation was required. Optional omissions did not reduce scores. Positive false claims were retained as errors rather than silently corrected.

| Arm | Legacy grounding score | Grounding review | Task completeness | Causal / factual review | Overall manual score |
|---|---:|---|---|---|---:|
| A | 1.000 | Essential names real; qualified search ownership wrong | Main requested components covered | Main flow described; orchestration assigned to wrong file | 0.5 |
| B | 0.946 | Essential files/symbols real; some line references inaccurate | Main requested components covered | Core flow correct; parser lookup structure falsely called a trie | 0.5 |
| C | 0.951 | Essential owners real; `pre_globs()` is not a function | Main requested components covered | Core flow correct; explicit-thread branch omitted from an asserted decision rule | 0.5 |

All three identify flag definitions/updates, `LowArgs`, conversion to `HiArgs`, and construction or consumption of search components. Coverage is therefore recorded as **1** separately in the structured review. A complete inventory does not make every stated relationship correct. The existing overall 0/0.5/1 scale remains unchanged; these are partial, with **zero reviewed full successes**. In particular, B's error concerns an unnecessary implementation detail, not absence of a required exhaustive flag tour. Core-flow coverage and stricter overall factual correctness should both remain visible when interpreting these coarse scores.

Verified reviewer anchors and contradictions:

- **A:** `crates/core/main.rs:44,77,113,160` contains entry, dispatch, `search` and `search_parallel`. The answer assigns the latter two to `crates/core/search.rs`, which instead defines `SearchWorkerBuilder`/`SearchWorker`. The legacy lexical score finds real file and symbol tokens separately and misses the wrong qualified ownership.
- **B:** `crates/core/flags/parse.rs:368–396` stores `HashMap<Vec<u8>, usize>`, constructs it with `HashMap::with_capacity` and uses `self.map.get(name)`. It is not a trie. Nearby Aho-Corasick comments are stale; whether those comments caused the error is a hypothesis. The parser actually takes `&mut LowArgs`, while the answer's pseudo-call uses `&low`. Its central update/conversion/worker chain is otherwise supported by `parse.rs:48–56,221–289`, `hiargs.rs:685–719` and `main.rs:113–140`.
- **C:** `hiargs.rs:151` calls `preprocessor_globs(&state, &low)` and binds `pre_globs`; lines 91/694 show that name as a field and its consumption. There is no `pre_globs()` helper. Lines 167–173 choose one thread for sorting/single-file, otherwise explicit `low.threads`, otherwise automatic parallelism. The answer skips the explicit branch while claiming that automatic selection covers the remaining cases. This is an asserted wrong rule, not a requirement to describe thread flags in the first place.

The legacy grounding metric remains useful for real-reference checking but does not establish ownership, complete coverage, precedence or causal accuracy. It is retained unchanged. This is assistant source-grounded review, without an independent second human/reviewer; error severity and the coarse partial labels remain reviewable in local annotations. No model received corrections or reviewer anchors.

## Tool-selection observations

**A:** An initial broad `ripgrep|rg` search and large file reads generate substantial context. It reads the same truncated `defs.rs` prefix three times, including **two exact redundant successful calls**. Native `read_file` retains the existing 40,000-character limit and offers no line slices; repeating the same call does not expand evidence. This limits what this baseline represents: the existing three-tool harness, not every native coding CLI. File-path grep returns real matches, including `FLAGS` and flag-update locations.

**B:** Relevant `investigate`, `trace_path` and `read_code_unit` calls are interleaved with six native calls after MCP begins. This is a mechanical native-fallback count, not evidence of six MCP failures: none failed. Guessed `parse_args`/`parse_high` discovery and late flag-update inventory add exploration. Displayed regex escaping initially looked suspicious; the decoded patterns are valid, actual results are recorded, and no argument repair was performed. Source-file grep returns matching lines.

**C:** All 26 calls use MCP. Startup/orientation, broad discovery, tracing and focused reads match the skill's tool families. Broad `investigate` plus `locate_code`, guessed `parse_high` lookup followed by content search, and later `FlagInfo`/suggestion/trait exploration show overlap and expansion beyond the main flow. No native fallback or exact repeated call occurred. The skill was loaded, but its presence does not prove optimal stopping or selection.

Exact duplicate counts are reproducible. A unique minimum number of necessary calls is not established, so semantic-redundancy and unnecessary-MCP-call **counts remain unavailable**; qualitative observations are preserved. “Appropriate MCP selection” is annotated as true at the tool-family level for B/C, with these explicit caveats, not as a claim that every call was necessary.

## Comparisons and gates

With **n=1 per arm**, each reported value is both its arm's average and median; within-arm variability is unavailable. Positive deltas below mean the right arm used more. These are unconditioned observations, not correctness-qualified efficiency gains.

| Comparison | Manual quality delta | Total-token delta | Time delta (s) | Tool-call delta |
|---|---:|---:|---:|---:|
| A → B | 0 | -386,940 | -61.967 | -8 |
| B → C | 0 | +133,668 | +26.874 | +6 |
| A → C | 0 | -253,272 | -35.093 | -2 |

C consumed more tokens, time and calls than B in this single attempt. No skill advantage is demonstrated. Successful-only efficiency is **unavailable, n=0**; lower consumption does not turn a partial answer into a successful one. Cache state was not reset, and seeded rotation is a single C/B/A order, so warm-up/cache bias and run variability cannot be estimated.

All **12 existing technical checks pass**: three arms/one task, completion, exercised valid tools, MCP exercised in B/C, no MCP in A, skill only in C, identical task, identical A/B system, identical B/C tools, provider usage, unchanged repository and context verification. An additional wire audit confirms identical native schemas across all arms, exact task/suffix and common instructions in every request, unchanged inference request controls and unchanged skill/snapshot hashes. A/B cannot see host `AGENTS.md`, skill/config files or global agent instructions; C differs through explicit skill injection only.

The source-file grep fix is shared by the exact same `BaselineExecutor` implementation in all arms. **153 automated tests passed in 11.13 s before inference**, including 19 native-search regression cases. No harness code, skill or production Rust changed during or after the timed attempts.

The previous `symbol_ignore_logic` smoke exhausted its output allowance after reserved synthesis. This task instead achieves **3/3 natural termination**. Because the task and previously approved shared guidance differ from that historical smoke, this does not isolate which change caused the improvement and is not a matched task-quality comparison.

The **technical gate passes**, but **`correctness_review_passed=false`**, so the combined full-benchmark prerequisite remains unsatisfied. Execution stops here for review, without another experiment or automatic correction.

## Local artifacts and reproduction

Ignored private artifacts: `results/navigation-skill-cli-smoke-20261018-1/`; supplementary read-only Docker/API proof: `results/navigation-skill-cli-smoke-20261018-1-verification/`. Original per-arm `result.json` files and wire traces remain intact. Reviewed derivatives include `results.jsonl`, `results.csv`, `summary.json`, `reviews.json`, `correctness_dimensions.jsonl`/`.csv`, `review_method.json` and `isolation_audit.json`. Machine-readable summaries include mean/median, successful-only counts and comparison deltas. Raw conversations, tool evidence and reasoning remain local and ignored by Git.

```sh
.venv/bin/python -m pytest bench/harness/tests -q
# Set STRATA_BASE_URL to the existing /v1 endpoint; do not restart or reconfigure it.
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase smoke \
  --config bench/harness/configs/navigation-skill.json \
  --base-url "$STRATA_BASE_URL" \
  --env-file "$HOME/projects/strata/.env" \
  --out results/new-cli-smoke \
  --capture-traces
# After independent source review, annotate reviews.json and regenerate offline:
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase report --out results/new-cli-smoke
```

The command documents reproduction, not authorization for an additional run. Review the task-specific factual findings and score severity before deciding any next experiment. Do not infer full-suite readiness or tune this task to remove the observed errors.
