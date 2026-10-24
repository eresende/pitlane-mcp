# Matched-context synthesis and reasoning effort — 2026-10-18

**Concise causal guidance is the stronger next diagnostic candidate; lowering reasoning effort is not presently justified as the default.** Strata's medium setting was verified as an applied template control in the running Docker deployment. Generic guidance with default xhigh produced no answer before exhausting 4,096 output tokens. Both concise cases terminated and covered the previously missing traversal effects, but source review found factual errors. **No experiment delivered a fully correct answer; the navigation readiness gate remains failed.**

These experiment labels A/B/C/D refer to synthesis factors, **not** the benchmark's native/MCP/skill arms. All four inherit the exact same configuration-C checkpoint, including its skill.

## Reasoning configuration: verification against Docker

The existing container `strata` (`d8a0098489ed`, image `strata:rocm-gfx1100`) remained running. Read-only inspection established:

- Live `/props` reports Strata **0.1.44.1**, the deployed chat template and context **131072**. `/v1/models` and `/v1/status` report the existing loaded `qwen3.8-flash-next-coder-iq1_m` model.
- Live `GET /settings` returns empty shared defaults. The container's run config has no `reasoning_effort` override and uses default start-position effort. The deployed template resolves an omitted effort to **xhigh**. Thus the API's current default was verified independently of the UI label.
- The deployed `serve/frontend.py` accepts top-level `reasoning_effort`; `medium` maps to the template kwarg `reasoning_effort="medium"`. `high` maps to `xhigh`. The Chat Completions handler passes the parsed kwargs through `Service.prepare` to prompt encoding. An explicit request wins over a default.
- In this template, medium is implemented by **omitting the xhigh instruction**, while preserving thinking mode. It is not a numerical reasoning-token budget. The model may still reason at length or make mistakes.

The deployed frontend/server hashes match the inspected local source at Strata commit `296b59d2f73c8f04a805df29c2fa97655adace78`:

| File | SHA-256 |
|---|---|
| `serve/frontend.py` | `54e7c1e2bc7f2ea52fe05cdd6d5b62d9ef83ef2e2bf8919db8ec417d3cb0713e` |
| `serve/server.py` | `6c45c639e28a873b854606719d9ce2bb2e0df666f105164e78e2735878b9aa8f` |

Source locations: `frontend.py:134–148,547–558` (normalization), `server.py:3538–3563` (defaults), `3771–3800` (rendering/encoding), `3848–3856` (prepare), `5807–5846` (Chat Completions route). The template returned by the running API was checked against the container's loaded-template file.

A separate CPU-only `docker exec` process imported the deployed frontend/tokenizer and used `Service.__new__` with only rendering attributes. It did not construct an engine, load model weights, start a server, or write configuration. It independently rendered/tokenized each complete experimental payload. The default renders contained the xhigh instruction; medium renders did not, with medium kwargs confirmed. Rendered-prompt and encoded-token-ID hashes were retained locally.

| Experiment | Expected input tokens from deployed encoder | Actual API input tokens | Effective prompt control |
|---|---:|---:|---|
| A | 27,191 | 27,191 | xhigh |
| B | 27,249 | 27,249 | xhigh |
| C | 27,153 | 27,153 | medium |
| D | 27,211 | 27,211 | medium |

This verifies more than parameter acceptance: deployed parsing/rendering was inspected, medium changed the encoded model prompt, and every live request's reported input length matched independent encoding. The 38-token difference between reasoning levels is the template treatment, not removed repository evidence.

**No direct per-response effective-effort echo is exposed** by Chat Completions, `/v1/models`, `/v1/status` or `/props`. The effective labels above mean verified **template control**, not measurement of an internal amount of reasoning. Prompt-count agreement alone would not suffice without the code/template checks, and the server does not expose a live prompt-ID digest to compare with the independently generated digest. These are the practical limits of verification; no claim is made that medium forces less reasoning on every task. Current verification also does not retroactively prove every earlier run's unobserved effective settings.

## Matched-context method

Source: `results/navigation-skill-smoke-20261018-3/instances/symbol_ignore_logic/C/run_1/wire/request_17.json`. Its messages contain all tool results **through request 16**, before any request-17 generation. The [prior evidence audit](NAVIGATION_SKILL_TARGETED_DIAGNOSTICS_2026-10-18.md#checkpoint-evidence-audit) verified that the checkpoint contains the matching chain, directory pruning and matcher push/pop state. No new source or task-specific answer hints were supplied.

All four requests copied those messages unchanged and appended one synthesis instruction. Tools/tool choice were omitted, no tools were executed, and no server-side MCP flag was enabled. Model, evidence, context window, temperature **0**, non-streaming mode and **max_tokens=4096** were fixed. Each request started independently from the checkpoint; no generated answer from another case was appended. Cache/hardware state was not reset.

| Experiment | Requested effort | Guidance |
|---|---|---|
| A | omitted: existing server default | unchanged generic reserved synthesis instruction |
| B | omitted: existing server default | concise causal coverage |
| C | explicit medium | same generic instruction as A |
| D | explicit medium | same concise instruction as B |

The generic instruction is the exact existing `FINAL_TURN_PROMPT`. B/D ask for an answer of at most 300 words identifying essential owners/symbols, connecting operations into an evidence-backed flow, explaining effects of decisions/state changes, covering every requested part, stating missing evidence, and avoiding unnecessary detail. This instruction contains no ripgrep symbols, filenames, pruning hints or ground-truth answers. Its exact bytes are pinned in the manifest and implementation. No common benchmark guidance or skill was modified.

One repetition per cell, seeded order **C → A → D → B**, executed sequentially. This is a small diagnostic factorial comparison, not a reliability estimate or skill-effect measurement.

## Measured results

| Experiment | Requested → verified prompt effort | Input tokens | Output tokens | Total tokens | Finish | Time (s) |
|---|---|---:|---:|---:|---|---:|
| A | default → xhigh | 27,191 | 4,096 | 31,287 | **length** | 70.139 |
| B | default → xhigh | 27,249 | 887 | 28,136 | **stop** | 30.033 |
| C | medium → medium | 27,153 | 2,005 | 29,158 | **stop** | 42.522 |
| D | medium → medium | 27,211 | 1,211 | 28,422 | **stop** | 33.628 |

Time includes the generation HTTP request and wire logging, excluding discovery, CPU verification and manual scoring. Exact provider totals are used; no estimated reasoning-token count is added. **Separate reasoning tokens are unavailable in all four responses.** Each request reported zero cached input tokens.

| Experiment | Delivered answer / reasoning bytes (UTF-8) | Grounding score | Requested causal coverage | Manual correctness | Successful task |
|---|---:|---:|---|---:|---|
| A | 0 / 16,495 | 0.0 | absent: no delivered answer | 0 | no |
| B | 1,853 / 1,735 | 1.0 | essential flow covered; factual error remains | 0.5 | no |
| C | 6,056 / 1,681 | 1.0 | essential flow covered; factual errors remain | 0.5 | no |
| D | 3,193 / 1,472 | 1.0 | essential flow covered; precedence error remains | 0.5 | no |

A exhausted the output allowance with reasoning content and no visible answer. B/C/D ended without truncation or another intervention. All are **prompted tools-disabled synthesis**, not natural task completion during navigation. Successful-task efficiency has **n=0**. The existing grounding scorer checks reference presence; it does not validate method ownership, conditions or precedence. Its perfect scores in B/C/D do not override manual correctness.

New generation consumption across these four requests: **108,804 input + 8,199 output = 117,003 total tokens**. Historical requests used to construct the checkpoint were not re-executed and are not added to this experiment's measured cost. The largest actual request input is 27,249 tokens; cumulative consumption is not context length. Every input plus the 4,096 allowance fits context 131,072. Tool response tokens remain unavailable; new tool-response content is zero because no tools ran.

## Repository-grounded answer review

The actual frozen ripgrep source at commit `4649aa9700619f94cf9c66876e9549d83420e16c` was checked, including:

- `crates/ignore/src/walk.rs:932–1040,1796–1807`: the skip decision, pruned directories and child/parent matcher state.
- `crates/ignore/src/dir.rs:350–409,413–532`: hidden handling, override/ignore/type ordering and parent/category matching.
- `crates/ignore/src/gitignore.rs:188–268`: path normalization and reversed glob selection.
- `crates/ignore/src/lib.rs:479–485`: `Match::or` selects the first non-None result.

**A:** no answer was delivered. Private reasoning is neither an answer nor a scored substitute. All requested relationships are missing from the delivered output.

**B:** the key owners and forward matching chain are correct, as are directory entry/exit matcher changes and ignored-directory pruning. It now covers the essential relationships missing from the prior replay. However, it places hidden-file checks inside `Ignore::matched` after types. Actual hidden handling belongs to **`matched_dir_entry`**, after `matched` returns, and applies **only when the result is None**. Under the existing fully-correct requirement this is partial correctness, despite otherwise complete task coverage. It explicitly notes that exact line parsing was not retrieved. Visible answer length: 197 whitespace-delimited words.

**C:** correctly explains the central traversal stack, skip result and pruning, but claims type filters have higher precedence than ignore files. Actual `Ignore::matched` immediately returns an ignore-file exclusion before consulting types; only overrides have the stated highest priority. The answer later describes the sequence more accurately, leaving an internal contradiction. Its opening assertion that this is not upstream ripgrep's layout is also unsupported by the checkpoint/target identity. It acknowledges uninspected parser/parallel/glob internals. Visible length: 652 words; generic guidance imposed no word cap.

**D:** covers traversal state, pruning, matching and explicit uncertainty. It incorrectly says the `.or()` combination makes nearer rules win. Actual proximity is resolved **within each category** by walking parents and retaining the first match for that category; `.or()` then imposes **category priority**. A nearer rule in a lower-priority category does not universally beat a farther rule in a higher-priority category. This is a causal error, not a missing symbol. The symbol inventory remains broad. Visible length is 305 whitespace-delimited words, slightly above the requested 300; the correctness penalty is for precedence, not that small formatting overrun.

No essential traversal relationship is entirely absent from B/C/D; the remaining failures are accuracy of volunteered relationships/conditions. All three are conservatively **0.5**, not successful. The review did not loosen the rubric or count incomplete/truncated content as success. This is repository-grounded agent review, without an independent second reviewer. Detailed statement-level reviews are retained locally.

## Effects and recommendation

| Change at fixed other factor | Output-token change | Total-token change | Time change (s) | Observed result |
|---|---:|---:|---:|---|
| A → B: concise guidance at xhigh | -3,209 | -3,151 | -40.106 | truncation/no answer → terminal, partially correct answer |
| C → D: concise guidance at medium | -794 | -736 | -8.894 | both terminal and partial; shorter answer with a different error |
| A → C: medium with generic guidance | -2,091 | -2,129 | -27.617 | truncation/no answer → terminal, partially correct answer |
| B → D: medium with concise guidance | **+324** | **+286** | **+3.595** | both terminal and partial; medium did not improve measured efficiency |

Confirmed for these requests: guidance alone enabled bounded synthesis while retaining xhigh; medium alone also avoided the generic case's truncation. Medium was **not** consistently cheaper: with concise guidance it used more output/total tokens and took longer than default xhigh. No condition achieved correctness 1.0. Thus neither lower token use nor `stop` establishes a successful task, and these four results establish no navigation-skill benefit.

**Recommendation for the next reviewed diagnostic candidate: default xhigh + concise causal coverage (B).** It delivered the shortest terminal answer, used the fewest total/output tokens and had the lowest observed latency, while covering the central requested flow. This is a provisional choice, not a validated benchmark profile: its hidden-rule attribution still needs to be correct on independent/repeated evaluations before promotion. The data does not justify lowering the server default, changing the output allowance, modifying the skill, or advancing to a smoke/full benchmark.

Hypotheses requiring more evidence: concise prioritization may prevent long pre-answer reasoning; lower effort may help when guidance is broad; reduced detail may reduce opportunities for incorrect extra claims. There is no demonstrated hard reasoning-budget mechanism, context overflow, quantization cause, statistical significance or guaranteed interaction effect. One sample per cell cannot estimate repeat variability. Repetition at the same checkpoint and review of the named factual errors should precede further claims; any future prompt revision should remain task-neutral and apply identically to the navigation arms if approved.

## Environment, validity limits and preservation

Declared hardware remains RX 7900 XTX 24 GB, Ryzen 9 9950X, 32 GB RAM and 8-bit KV cache; no hardware, model, inference-server or KV setting was changed. Model/engine/context metadata and shared defaults matched before/after, and the server start timestamp stayed `1792337274`. The API request counter advanced **221 → 225**, consistent with exactly four generation requests; no extra synthetic generations were performed for verification. Observed host VRAM samples ranged from 23,741,288,448 to 23,809,527,808 bytes. Endpoint samples cannot exclude transient pressure or unobserved outside configuration changes.

All calls reported zero cached prompt tokens, but this does not remove expert-cache, filesystem, GPU warm-up or order effects. The 144-test harness suite ran for 11.46 seconds overlapping the first case, C; that host CPU activity is an additional limitation for C's timing. There were no concurrent model-generation jobs from this harness. Timing comparisons are descriptive and do not isolate raw inference performance; correctness remains primary.

The entire original smoke artifact tree is unchanged: SHA-256 `5e3674bb4f09d6d21a25bb7c7b18afafcf133e4080e43466581acf4ec71db733` before/after, including original traces and repository snapshot. Checkpoint evidence-message hashes match across all cases. This experiment contains only one task and one checkpoint inherited from C; it does not evaluate A/B/C isolation anew or show that agents will autonomously stop exploring.

## Reproduce and review

```sh
# Set STRATA_BASE_URL to the existing endpoint at runtime.
.venv/bin/python -m bench.harness.experiments.compare_synthesis \
  --source-run results/navigation-skill-smoke-20261018-3 \
  --strata-source /path/to/strata --container strata \
  --env-file /path/to/strata/.env --out results/new-matched-synthesis
.venv/bin/python -m pytest bench/harness/tests -q
```

The deployment verifier is deliberately specific to the inspected Docker setup. Other configurations require renewed inspection; failed verification skips medium rather than inferring support from HTTP acceptance. See [execution and verification details](experiments/README.md#matched-completion-guidance-and-reasoning-comparison).

Local ignored artifacts: `results/navigation-skill-matched-synthesis-20261018-1/`. These include `reasoning_verification.json`, `manifest.json`, `A|B|C|D/wire/`, observations and raw summaries, environment samples, `source_integrity.json`, `reviews.json`, `reviewed_results.jsonl`, `reviewed_results.csv` and `reviewed_summary.json`. Raw summaries/responses are preserved separately from manual annotations. Authentication headers are never captured. Raw conversations, repository payloads, templates and reasoning remain local and excluded from Git.

Automated validation: **144 harness tests passed**, including five new matched-factor/verification regression cases. No SKILL.md, common benchmark completion guidance or production Rust changes were made. No three-arm smoke, full benchmark, commit, push or PR was performed. Work stops for user review.
