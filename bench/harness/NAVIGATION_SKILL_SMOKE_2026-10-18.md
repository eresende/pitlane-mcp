# Navigation skill smoke test — 2026-10-18

The harness extension is implemented, but the smoke test failed its readiness gate. All three arms reached the 25-iteration limit without a final answer. The 45-run experiment was not executed. These observations do not establish an effectiveness benefit for either MCP or the skill.

## Environment and inputs

- Target: ripgrep commit `4649aa9700619f94cf9c66876e9549d83420e16c`; tracked HEAD snapshot.
- Harness base commit: `dfa5796a2e07f8dc35aec43a899ee0386561810a` with uncommitted experiment changes; exact Python source hashes are in the local manifest.
- Model: `qwen3.8-flash-next-coder-iq1_m`; quantization `iq1_m` (user supplied model designation).
- Inference engine: Strata v0.1.44.1; 8-bit KV cache; RX 7900 XTX 24 GB, Ryzen 9 9950X, 32 GB RAM (user declarations).
- API discovery verified the requested model loaded with context `131072`. The engine version and KV settings were not independently verified.
- Observed client: AMD Ryzen 9 9950X 16-Core Processor; 30.7 GiB RAM.
- PitlaneMCP: 0.17.1. Existing remote embeddings: Qwen/Qwen3-Embedding-8B through the configured DeepInfra endpoint; embeddings were ready before navigation. No models were downloaded; Strata was not reconfigured or restarted.
- Prompt-set SHA-256: `d0ff8ebe7022a4770feef22a178a52bf8baf5c951c544f05b7873702df8f5216`.
- Skill SHA-256: `09556ef23680fdde79a6436bd6cf9e4b3e606e08f7099985187ed5de9e5506e9`.
- Endpoint and credential location are runtime inputs. Neither the local IP address nor the API key is stored in committed configuration.

## Methodology

One unchanged existing prompt, `symbol_ignore_logic`, was executed once in each arm. Its task is to find ignore/gitignore handling and explain the connection between traversal and ignore matching. Execution order was seeded C → B → A. Each run used a fresh conversation and, in B/C, a fresh MCP process over a shared prebuilt index.

| Arm | Native tools | MCP | Skill |
|---|---|---|---|
| A | read/search/list | unavailable | absent |
| B | identical native tools | default tier | absent |
| C | identical native tools | identical to B | entire SKILL.md appended to system prompt |

A/B used byte-identical neutral system prompts. Every arm received the exact same task prompt and suffix. No historical MCP-specific or task-specific benchmark instructions were used. B/C retained the server-provided tool descriptions, which themselves provide navigation guidance; the skill treatment measures additional guidance beyond those descriptions. The tracked-file snapshot excludes untracked AGENTS.md and .gemini files in the original fixture. Host/global agent settings were never loaded.

The same model, context, temperature 0, server-default reasoning configuration, 4,096 output-token request limit, 25 model-request iterations and 300-second navigation timeout applied in all arms. An iteration can request multiple tools. No automatic HTTP retries or argument repair were used.

Cold MCP setup was 11.539s, including indexing and waiting for embeddings; source indexing reported 1.349s. These times and per-session setup overhead are excluded from navigation timings below.

The first preparation attempt stopped on ripgrep’s internal HomebrewFormula → pkg/brew symlink before any task ran. The exporter was corrected to preserve internal links and reject escaping/broken links, then the three-task smoke was run in a fresh artifact directory. The failed setup artifacts were retained.

## Measured results

Each arm has n=1, so every mean equals its median. Sample standard deviations and repeat variability are unavailable. Correctness is 0 because no final answer was produced, rather than because any specific factual claim was found false. The historical grounding score is also 0 for an empty answer.

| Arm | Status | Correctness / grounding | Input tokens | Output tokens | Total tokens | Navigation seconds | Native / MCP attempts | Failed / invalid calls | Tool bytes |
|---|---|---|---:|---:|---:|---:|---|---|---:|
| A | max_iterations | 0 / 0 | 1,097,025 | 6,425 | 1,103,450 | 145.283 | 38 / 0 | 0 / 0 | 232,741 |
| B | max_iterations | 0 / 0 | 874,993 | 5,509 | 880,502 | 123.553 | 18 / 28 | 2 / 2 | 186,863 |
| C | max_iterations | 0 / 0 | 428,005 | 3,005 | 431,010 | 74.062 | 6 / 19 | 0 / 0 | 93,946 |

Provider usage is summed over 25 model requests per arm, including repeatedly submitted history. Cumulative input can therefore exceed the context window without an individual request exceeding it. Reasoning-token counts and tool-response token counts are unavailable; tool bytes are exact UTF-8 response sizes. Reasoning, if separately reported in future, is an output subset and must not be added to total again.

| Comparison | Quality mean/median delta | Total-token mean/median delta | Time mean/median delta (s) |
|---|---:|---:|---:|
| A vs B, B minus A | 0 | -222,948 | -21.731 |
| B vs C, C minus B | 0 | -449,492 | -49.491 |
| A vs C, C minus A | 0 | -672,440 | -71.222 |

These are descriptive differences between unsuccessful runs. They are not evidence of better efficiency at equivalent correctness. There are no successful tasks from which to calculate conditional efficiency.

## Tool reliability and experimental controls

- Structured protocol round trips passed before implementation and during the experiment preflight: valid JSON arguments, call IDs and tool-result consumption.
- A: 0 failed calls out of 38 attempts. B: 2/46 (4.35%), both schema-invalid search_content arguments. C: 0/25. Provider protocol errors were 0 in every arm.
- All 12 control checks are recorded in summary.json. Ten passed, including prompt/skill/tool isolation, context, usage availability and repository integrity. The completion and valid-call checks failed; technical_passed is false.
- Skill text was actually present only in C, identified by its exact hash. MCP tools were exposed in B/C with identical schema hashes and were unavailable in A.
- Exact retries after an identical failed call were 0. Exact repeated successful calls were A=1, B=0, C=0. These proxies do not detect semantic repetition.
- Native calls after the first MCP call: B=18 and C=6. All native calls in A were baseline navigation.
- Loaded-model metadata was unchanged in all before/after observations. Host VRAM observations ranged from 23,901,622,272 to 24,051,945,472 bytes used, on a 25,753,026,560-byte device. These sparse host samples cannot rule out transient pressure or prove inference-server memory use.
- The original checkout and snapshot bytes remained unchanged throughout all runs.

## Limitations and interpretation

The historical scorer checks whether extracted references exist. It cannot establish completeness, causal correctness or task success; it remains a secondary grounding proxy. Manual review records the absence of every final answer as correctness 0. Future completed answers require repository-backed review before being treated as successful.

Only one task and one repetition per arm were run. The smoke cannot establish statistical significance, repeat variability, or performance across navigation categories. All arms failed task completion; why the agent continued exploring is not established by this sample. Quantization, stopping behavior, task difficulty and tool guidance are hypotheses, not measured causes. Increasing limits or changing guidance would require a new, consistently configured smoke across all arms.

Initial tool logs retained ordered names, argument hashes, response sizes and error categories, without conversation or tool-response bodies. Detailed appropriate-selection, unnecessary-MCP-call and semantic-redundancy metrics remain unavailable; they were not guessed. After the smoke, local argument logging was added for future audits, and infrastructure failures were hardened to retain unknown measurements as null. These changes do not alter model requests, but change the harness source hash; the full-run gate requires a fresh matching smoke. No additional task runs were made after these changes.

Global inference cache state was not reset. Seeded order reduces systematic ordering bias but cannot eliminate it in a three-run smoke. Sampling, reasoning request policy and engine settings were held identical by this harness; server configuration-change events and model-loading events were unavailable beyond sampled metadata.

The planned five-task subset uses existing symbol_ignore_logic, graph_nav_call_chain, arch_package_map, usage_cli_to_search and semantic_search_probe prompts. usage_cli_to_search provides dependency tracing, not a dedicated change-impact test. The suite contains no explicit hypothetical change-impact prompt.

## Reproduction and artifacts

See [the experiment instructions](experiments/README.md) and [A/B/C configuration](configs/navigation-skill.json). The command below runs exactly three tasks and stops; the endpoint comes from STRATA_BASE_URL.

```sh
.venv/bin/python -m bench.harness.experiments.navigation_skill \
  --phase smoke --env-file /path/to/strata/.env --out results/skill-smoke
.venv/bin/python -m pytest bench/harness/tests -q
```

Local initial-attempt artifacts: results/navigation-skill-smoke-20261018-1/ (preflight and failed snapshot preparation only). Local measured-run artifacts: results/navigation-skill-smoke-20261018-2/, including run_manifest.json, index_setup.json, reviews.json, per-instance results, results.jsonl/results.csv, summary.json and report.md. These ignored artifacts are not intended for version control; no API keys or raw conversations are in this Markdown report.

Automated validation: 108 harness tests passed after the final instrumentation hardening; no production Rust changes. The full command requires reviewed successful smoke results with matching inputs and is blocked for this experiment. No commit, push or pull request was made.

## Conclusion

The experiment controls and accounting worked, and the smoke exposed both task noncompletion and invalid tool arguments. It did not validate readiness for the 45-run benchmark. C consumed fewer tokens and less time in this sample, but no effectiveness or successful-task efficiency benefit can be claimed. User review is the next step.

## Subsequent termination review

See [the termination diagnosis](NAVIGATION_SKILL_TERMINATION_DIAGNOSIS_2026-10-18.md)
for the completion-code review, per-request context analysis, before/after minimal
Strata diagnostics and the generic bounded-synthesis fix. The original smoke above
has not been rerun or rescored. “No final answer” here means no answer recognized by
the original loop; the discarded raw responses cannot be audited retrospectively
for answer-like prose alongside pending tools or reasoning-only text. Another
complete three-arm smoke remains pending user review.

The subsequently authorized [three-arm retest](NAVIGATION_SKILL_SMOKE_RETEST_2026-10-18.md)
has now been executed with common completion guidance and a reserved synthesis
turn. Its results and old-to-new comparison are separate from this original run.
