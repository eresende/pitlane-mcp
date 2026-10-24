# Evidence-presentation synthesis diagnostic — 2026-10-18

**Reducing the context did not reliably resolve the remaining factual issue.** All four tools-disabled responses terminated, but one compact-context answer still placed hidden-file handling in the wrong method despite receiving the full relevant implementation. Other answers omitted important conditions rather than making that false claim. Pruning and matcher-state transitions were correctly connected in both presentations. The next recommended check is cross-model validation with **Qwen3.8-27B Q4_K_M**; it was not downloaded, loaded or run.

The approved provisional benchmark policy is now implemented: **default xhigh reasoning plus concise causal completion guidance**, common to A/B/C. This experiment is strictly a synthesis diagnostic, not a navigation benchmark. No smoke or full benchmark was executed, and the readiness gate remains failed.

## Provisional harness policy

`experiments/completion_guidance.py` defines the approved causal instruction, byte-identical to experiment B in the reviewed matched-context comparison. Its task-neutral answer guidance is included in the neutral system prompt for every navigation arm. The same full concise instruction is supplied at the reserved synthesis turn in every arm. A/B base prompts remain byte-identical; only C appends the unchanged skill.

`AgenticLoop.run` now accepts an optional `final_turn_prompt`; its legacy default remains the original generic instruction. The navigation experiment opts into the approved concise version and recognizes it when recording budget-prompted termination. Model, temperature, context, output allowance, iteration budget and timeout remain unchanged. Config `request_options` stays empty: default xhigh is used without a per-request override. The config records the provisional completion policy and the date on which the default was verified.

These implementation changes have not been validated by a new navigation smoke. Historical reports/artifacts are preserved, and their old hashes cannot certify this changed profile.

## Controlled inputs

The source is the verified C checkpoint **after request 16**, contained in `results/navigation-skill-smoke-20261018-3/instances/symbol_ignore_logic/C/run_1/wire/request_17.json`. It includes the original question and C system prompt/skill. Both presentations keep those exact system/task bytes and append the same approved concise causal instruction.

- **Accumulated:** all recorded messages through that checkpoint, including tool calls, tool responses, outlines, metadata and unsuccessful searches.
- **Compact:** the original system/task, followed by a mechanically extracted source collection and the same completion instruction. It contains source fields from reads, investigation Rust blocks, positive native/MCP text hits and source signatures from every recorded source-bearing response. All snippets retain first-seen order; only identical snippets are deduplicated.

No reviewer selected a favorable subset. No source was added from outside the checkpoint. Snapshot reads validated provenance only; their contents did not enrich the model input. Generated truncation markers, navigation transcript, empty searches, tool guidance and other metadata were removed. File/line labels are mechanical provenance. The original source comments remain; no reviewer explanations, corrected facts, topic-specific questions or answers were inserted.

The extraction retained **86 distinct source/signature snippets from 16 tool responses**, including the full recorded `dir.rs:340–544` slice, `walk.rs:920–1080`, Gitignore matching and the returned `should_skip_entry` context. This includes direct evidence of the hidden-check owner/predicate, ignore/type short-circuiting, pruning and child/parent matcher transitions. The source collection itself is 48,517 UTF-8 bytes. Including system/task/guidance, message content falls from **89,674 to 55,367 bytes**; message count falls from 48 to 4.

This is an evidence-presentation treatment: length, repetition, roles and metadata/history change together. It is not a pure context-length ablation. In particular, removing the earlier misleading empty native searches is part of the treatment, not a new corrected search result.

## Execution and controls

Exactly four fresh inference requests, two per presentation, in balanced order:

1. accumulated 1
2. compact 1
3. compact 2
4. accumulated 2

All use the existing `qwen3.8-flash-next-coder-iq1_m`, temperature **0**, `max_tokens=4096`, non-streaming mode, context **131072**, no reasoning override and **no tool access**. No generated answer is carried into another request. The historical checkpoint's original system prompt is deliberately retained in both conditions; this diagnostic tests presentation with the approved final instruction, not the newly updated benchmark system prompt or autonomous navigation.

Default xhigh was reverified against the running Docker deployment using the prior reviewed source hashes, current `/props` template and empty `/settings` defaults. A CPU-only Docker process rendered/tokenized both inputs without constructing an engine or loading weights. Predicted input lengths—**27,249 accumulated / 13,770 compact**—matched every actual API response. Strata still exposes no direct per-response effective-effort echo; verification establishes its applied template control, not an exact quantity of reasoning.

The existing Strata engine remained **0.1.44.1**. The server start timestamp stayed `1792337274`, shared defaults remained empty, and its generation-request counter advanced **225 → 229**. No server restart/configuration/model change occurred. Declared hardware remains RX 7900 XTX 24 GB, Ryzen 9 9950X, 32 GB RAM and 8-bit KV cache. No additional model was downloaded.

## Per-request results

| Presentation / repeat | Input | Output | Total tokens | Finish | Time (s) | Cached input | Grounding | Manual diagnostic score |
|---|---:|---:|---:|---|---:|---:|---:|---:|
| Accumulated 1 | 27,249 | 1,195 | 28,444 | stop | 15.464 | 27,244 | 1.0 | 0.5 |
| Compact 1 | 13,770 | 1,952 | 15,722 | stop | 33.611 | 0 | 1.0 | 0.5 |
| Compact 2 | 13,770 | 1,774 | 15,544 | stop | 21.995 | 13,765 | 1.0 | 0.5 |
| Accumulated 2 | 27,249 | 2,246 | 29,495 | stop | 45.617 | 0 | 1.0 | 0.5 |

Separate reasoning tokens are **unavailable** in every response. Provider input/output/total counts are used directly; no reasoning-token estimate is added. New consumption is **82,038 input + 7,167 output = 89,205 total tokens**. Historical requests were not re-executed or counted as new consumption. Cumulative totals are not context length. Every request fits comfortably inside the existing context window.

Times include generation HTTP and wire logging, excluding CPU verification and grading. No automated test suite or other inference job from this harness ran during timed requests. Cache was not reset: each presentation has one nearly fully cached request and one uncached request. Cached tokens are not subtracted from consumption. Warm expert/filesystem/GPU state and differing generated lengths still limit timing attribution.

| Presentation, n=2 | Mean / median total tokens | Mean / median output | Mean / median time (s) | Token sample SD | Time sample SD (s) | Fully demonstrated all four concepts |
|---|---:|---:|---:|---:|---:|---:|
| Accumulated | 28,969.5 | 1,720.5 | 30.541 | 743.17 | 21.321 | 0/2 |
| Compact | 15,633.0 | 1,863.0 | 27.803 | 125.87 | 8.214 | 0/2 |

Compact input uses **49.5% fewer input tokens** and **46.0% fewer mean total tokens**, but **more mean output tokens**, with no improvement in full diagnostic correctness. These are not successful-task efficiency gains. Two observations per condition do not support significance or a reliable speed claim. Temperature 0 did not make repeated outputs identical.

All responses ended after an explicit synthesis instruction with tools disabled. These are **prompted synthesis completions**, not natural navigation termination or readiness passes.

## Four-concept source review

The reviewer-only criteria assess hidden-check ownership/condition, override/ignore/type precedence, directory skip/prune effects, and child/parent matcher-state changes. These topics were **not added to model input**. Because the original question is broad, omission does not prove misunderstanding; it is recorded separately from a false statement. The 0.5 scores reflect partial four-concept diagnostic coverage/correctness, not a retroactive rescore of historical benchmark runs.

| Answer | Hidden-file handling | Rule precedence | Directory pruning | Matcher-state transitions |
|---|---|---|---|---|
| Accumulated 1 | incomplete: correct owner, missing no-prior-match condition | incomplete: check order without exclusion short-circuit | correct | correct |
| Compact 1 | incomplete: correct owner, missing condition | incomplete: overrides/glob order, incomplete ignore/type relationship | correct | correct |
| Compact 2 | **incorrect: hidden check attributed to `Ignore::matched`** | incomplete: check sequence without short-circuit | correct | correct |
| Accumulated 2 | unaddressed | incomplete: last-glob rule only | correct | correct |

**Adequate direct evidence for the persistent error:** `crates/ignore/src/dir.rs:350–359` defines `matched_dir_entry`, calls `matched`, then checks `m.is_none() && opts.hidden && is_hidden`. `Ignore::matched` at 365–409 handles overrides, ignore files and types, with no hidden check. That complete source is in both inputs. Compact 2 nevertheless assigns hidden handling to `matched`, repeating the earlier error without the accumulated navigation transcript. This is a confirmed false attribution, rather than merely an omitted caveat.

**Precedence:** the supplied `matched` body immediately returns an override match, and an ignore-file exclusion returns before types. None of these four answers fully explains that exclusion/type relationship. Some correctly state override priority or last-glob matching, which is useful partial evidence. No answer here repeats the earlier categorical claim that types override ignore-file exclusions, nor the earlier claim that `.or()` makes proximity universally win. Their absence does not demonstrate correct understanding of those subtleties; the answers omit them. The `Match::or` implementation itself was not in this checkpoint and was not added to the compact input.

**Pruning/state:** the supplied `Walk::next` body shows ignored directories invoking `skip_current_dir`, the child-state push—including on skipped directories to balance an eventual Exit—and restoration via `parent()` on Exit. Both compact answers connect these changes correctly. Both accumulated answers also connect ignored-directory pruning and directory entry/exit state. These relationships are supported by actual `walk.rs:932–1040` and the returned helper context.

All file/symbol references extracted by the unchanged scorer are grounded: automated grounding is 1.0 throughout. That metric cannot establish conditional or causal correctness. Manual review is repository-grounded agent review, without an independent second reviewer. Raw answers/summaries remain separate from annotations.

## Attribution and next experiment

**Confirmed:** core source evidence is present in both inputs; compact presentation halves input-token consumption; all four requests terminate; compact presentation does not eliminate the hidden-owner error or reliably recover omitted conditions. No presentation fully demonstrates all four evaluation topics. A larger accumulated context is therefore not a sufficient explanation for the persistent factual error, and shrinking it is not a demonstrated correctness fix.

**Still hypotheses:** presentation may affect which details are selected; concise answer selection may omit conditions; this model/profile may fail to apply evidence it has received. One false compact answer does not prove an intrinsic model limitation or identify private reasoning, quantization, attention, cache or decoding as the cause. Omitted topics remain unassessed. No robust quality ranking between presentations is supported by n=2.

**Recommend cross-model validation with Qwen3.8-27B Q4_K_M before further navigation smoke testing.** Use the same accumulated and compact payloads, original question and concise instruction, with fresh requests and source-grounded review of the same four topics. A small two-per-presentation comparison would be sufficient as the next diagnostic, not as proof of general reliability. Verify its runtime compatibility, context capacity, API accounting and effective reasoning policy first; equivalent effort labels across models must not be assumed. Keep the current Strata server unchanged until a separate deployment/run is approved.

This would compare model profiles, changing both model and quantization; even a better result would not isolate quantization as the cause. Different model tokenizers also make raw cross-model token ratios unsuitable as a direct inference-cost equivalence. Correctness should lead the comparison. No model switch/download or cross-model run was performed here.

## Reproduction, tests and retained artifacts

```sh
# Set STRATA_BASE_URL to the existing endpoint at runtime.
.venv/bin/python -m pytest bench/harness/tests -q
.venv/bin/python -m bench.harness.experiments.compare_evidence \
  --source-run results/navigation-skill-smoke-20261018-3 \
  --reasoning-proof results/navigation-skill-matched-synthesis-20261018-1/reasoning_verification.json \
  --container strata --env-file /path/to/strata/.env \
  --out results/new-evidence-diagnostic
```

**148 harness tests passed before inference.** New tests verify recorded-only extraction, rejection of unrecorded/corrected source, removal of generated truncation markers, shared natural/reserved guidance through all three arm paths, unchanged reasoning requests and C-only skill isolation. Legacy defaults remain covered by existing tests.

Local artifacts: `results/navigation-skill-evidence-presentation-20261018-1/`, including the compact source collection, provenance audit, manifest, Docker verification, wire traces/observations, raw summaries, reviewer-only criteria, `reviews.json`, `reviewed_results.jsonl`, `reviewed_results.csv` and `reviewed_summary.json`. All remain ignored by Git; authentication headers are not captured. Raw conversations/source/reasoning are not included in this report or source changes.

The entire original smoke artifact tree, including its snapshot and traces, remains unchanged: SHA-256 `5e3674bb4f09d6d21a25bb7c7b18afafcf133e4080e43466581acf4ec71db733` before/after. SKILL.md and production Rust code were not modified. No three-arm smoke, full benchmark, commit, push or PR was performed. Work stops for review.
