# Lightweight navigation skill and four-arm proposal — 2026-10-18

Status: **design for review; no new inference authorized or executed**. The
infrastructure is in [draft PR #155](https://github.com/eresende/pitlane-mcp/pull/155).
This proposal and the [experimental skill](experimental-skills/pitlane-navigation-lightweight/SKILL.md)
remain separate, uncommitted local drafts outside that PR. Nothing is installed,
published or automatically discovered from the normal skill directory.

## What the original skill and pilot establish

The [15-run pilot](NAVIGATION_SKILL_PILOT_2026-10-18.md) supports no overall benefit
for C over B on those observations. It does not prove that the skill can never
help, or that every additional MCP call was unnecessary.

| Five-task total or mean | B: MCP only | C: original skill | C versus B |
|---|---:|---:|---:|
| Strict correctness mean | 0.600 | 0.500 | -0.100 |
| Task-oriented quality mean | 0.9325 | 0.8000 | -0.1325 |
| Task-oriented successes | 4/5 | 3/5 | -1 |
| MCP calls | 54 | 125 | +71 |
| Native calls | 82 | 29 | -53 |
| Total tool calls | 136 | 154 | +18 |
| Cumulative provider tokens | 1,609,155 | 1,975,012 | +22.7% |
| Navigation seconds | 453.654 | 493.113 | +8.7% |
| Natural / reserved completion | 4 / 1 | 5 / 0 | More natural completion; quality still lower |

| Existing task | Task quality B → C | MCP calls B → C | Total-call change | Interpretation |
|---|---|---|---:|---|
| `symbol_ignore_logic` | 1 → 0.5 | 15 → 30 | -3 | More MCP use coexisted with an incorrect required decision rule |
| `symbol_cli_config_flow` | 1 → 1 | 12 → 25 | +6 | No task-quality gain; strict incidental errors remained |
| `graph_nav_call_chain` | 0.6625 → 1 | 13 → 35 | +15 | Greater essential coverage; C completed naturally |
| `arch_package_map` | 1 → 1 | 2 → 18 | +3 | No quality gain, but C used fewer tokens/time on this task |
| `semantic_search_probe` | 1 → 0.5 | 12 → 17 | -3 | Incorrect qualified ownership affected the required answer |

C selected more MCP calls on all five tasks, but more **total** calls on only
three. Quality improved once, tied twice and worsened twice. C's zero exact
duplicate-call count does not establish absence of overlapping navigation.
Different queries, discovery tools and focused reads can revisit the same question.
There is no oracle for the minimum necessary plan, so do not convert all additional
calls into an invented unnecessary-call count.

C requested readiness/stats for every already-prepared task, versus two calls to
each in B. It used more discovery and reading across several tool families. These
are observed behaviors; the pilot does not isolate which instruction caused them.
One label-aware review and one run per task/arm do not establish significance,
repeatability, or an intrinsic model/quantization limitation.

The published skill already permits native tools, makes startup conditional and
asks the agent to stop after sufficient evidence. It should not be described as
requiring native-tool avoidance or unconditional initialization. Its potential
limitations are overlapping tool/startup/trace guidance and a stopping instruction
that is less operational than an explicit remaining-requirements check. General
advice to verify symbols did not prevent wrong qualified ownership or incorrect
essential behavior. More exploration alone did not reliably fix those errors.

## Lightweight treatment

Keep the original [SKILL.md](../../skills/pitlane-navigation/SKILL.md) unchanged,
including its recorded SHA-256
`09556ef23680fdde79a6436bd6cf9e4b3e606e08f7099985187ed5de9e5506e9`.
The experimental alternative lives under `bench/harness/experimental-skills/`,
outside the published distribution. Its unique frontmatter name allows separate
identification. Neither version imports or refers to the other during execution.

The draft changes decisions rather than adding a long API tutorial:

| Design choice | Intended behavior to test |
|---|---|
| Route by task and known evidence | One relevant starting operation; known targets go directly to a focused read |
| Skip startup on prepared repositories | Readiness/stats only address an actual index/scope problem |
| Track missing requirements | Further navigation closes a specific gap or contradiction, rather than broadening the inventory |
| Verify exact ownership | Inspect the containing source/type/implementation before qualified claims |
| Verify essential causal links | Check invocation/configuration and its behavioral effect; a graph/signature alone is not a policy proof |
| Permit focused native tools | Choose evidence availability rather than maximize MCP use; switch for a specific reason |
| Synthesize after sufficient evidence | Cover required components and relationships; explicitly identify unavailable evidence |
| Prioritize supported correctness | No hard symbol/call quota and no reward for a shorter incorrect answer |

These are **hypotheses**, not measured improvements. The whole shorter skill is one
treatment: any C/D difference includes instruction length and all content changes.
It cannot isolate routing versus stopping versus ownership guidance individually.
No repository-specific source owner, symbol, policy ordering, corrected pilot fact
or answer is included in the draft. No further tuning on the ignore task is planned.

## Four-arm controls

| Arm | Native tools | MCP tools | Skill instructions |
|---|---|---|---|
| A | Same shared implementation | Unavailable; no task server | None |
| B | Same shared implementation | Default tier | None |
| C | Same shared implementation | Identical default tier | Exact original file |
| D | Same shared implementation | Identical default tier | Exact lightweight file |

B/C/D must expose byte-identical ordered tool definitions, descriptions, schemas,
argument validation, response budgets and read-only policy. C/D differ **only** in
the appended skill-file bytes, including frontmatter. Their neutral system prefix,
original task/suffix, common completion guidance, reserved turn, tools and repository
evidence access are identical. Do not put arm-specific routing into tool descriptions,
server configuration, task suffixes, global instructions or readiness messages.
Instruction length is measured as part of the treatment, not equalized with padding.

Preserve Strata `qwen3.8-flash-next-coder-iq1_m`, IQ1_M, Docker 0.1.44.1, default
verified xhigh with no request override, 131072 context, 8-bit KV declaration,
temperature 0, 4096 output allowance, 25 requests including the existing reserved
synthesis, and the existing 300-second attempt timeout / 600-second setup timeout.
Use the exact common concise causal guidance. Keep hardware, ripgrep commit
`4649aa9700619f94cf9c66876e9549d83420e16c`, MCP binary, embedding settings and cache
preparation fixed. Do not download/load models, mutate settings or restart Strata.

Export a new instruction-free tracked-HEAD snapshot. Prepare its MCP index before
timing; record cold setup separately. Every attempt gets a fresh API conversation,
tool executor and (B/C/D) MCP process over the prepared snapshot/cache. No historical
answer/context is supplied. Hash each skill separately; reject an empty treatment.
Audit actual wire messages, not just intended prompt construction. Verify A/B have
no skill instructions and C/D contain exactly their designated file, without leakage
from either skill's containing directory. Pin source/config/prompt/binary hashes.
Observe model/settings/process/template identity before and after every attempt.

Use a saved seed, shuffled task order and four-arm cyclic/Latin-square rotation.
For three repetitions, each task covers three of the four arm-position rotations;
balance offsets across tasks and report the remaining imbalance. Do not claim
perfect per-task positional balance or clear caches/reset the server between arms.
Avoid concurrent model jobs, builds or grading during timed attempts.

## Proposed small validation: eight fresh runs

Use two **unchanged existing prompts**, with their exact grounding suffixes:

| Prompt ID | Why it adds useful validation |
|---|---|
| `usage_json_output` | A focused execution-flow task for another output mode; tests evidence-backed connections rather than the repeatedly reviewed ignore/CLI answers |
| `tests_multiline_search` | Test discovery plus explanation of validated scenarios; exercises task-specific navigation and interpretation beyond implementation inventories |

Neither ID appears in the navigation smoke/pilot run-manifest schedules audited
locally on 2026-10-18; those schedules contain only the five pilot IDs. This is
**unused in these navigation experiments**, not a claim that nobody ran them in
older repository-wide benchmarks. Source overlap with old tasks is still possible.

Two tasks × four arms × one fresh attempt = **8 navigation runs**. Review the first
complete four-arm task group for technical isolation before starting the second.
Stop on technical execution/accounting/isolation failure; retain wrong or partial
completed answers. A naturally completed wrong answer is a valid observation.
Reserved synthesis is reported separately and does not certify quality. Do not
increase budgets or change guidance to obtain a passing answer.

The current infrastructure runner accepts A/B/C, not D: this proposal is not an
executable four-arm configuration yet. Before authorized validation, add a generic
per-arm skill mapping, separate skill fingerprints, four-arm scheduling/reporting
and the additional rubric checks. Preserve existing A/B/C commands/artifacts.
Offline tests must show B/C/D schema equality, C/D prefix and prompt equality,
exact skill placement, no published-skill drift, balanced rotation, independent
session setup and technical-only stopping. No special paths for these answers.
Execution-critical changes require fresh validation; do not pretend the old CLI
smoke admitted this new code or reuse any historical result as a new observation.

## Quality rubric and blinded review before expansion

Freeze evaluator-only task checks **before** the first request. Reuse the unchanged
strict 0/0.5/1 metric and task-quality-v1 scoring rules. Version any extended task
registry separately; do not edit the historical frozen v1 document or rescore old
results. Derive each new checklist from its unchanged question: required components,
essential relationships/behaviors, task errors, incidental errors and unsupported
or falsely qualified symbols. Accept equivalent correct paths and justified absence
claims; do not demand exhaustive unrelated flags or every helper.

For test-discovery questions, checks must include real relevant tests and accurate
scenario/assertion explanations, rather than mere symbol existence. For negative
controls, score supported absence and truthful nearest alternatives; lexical
grounding alone cannot establish absence. None of this evaluator material is added
to model messages. Independent source review must check essential relationships
and exact ownership rather than rely on the existing lexical proxy.

Where practical, an independent reviewer receives randomized opaque answer IDs,
the original task/suffix and frozen source, without arm, skill, performance or tool
trace labels. Preserve answer text verbatim; strip only container metadata. If an
answer reveals its tools/treatment, record the possible unblinding. Keep the ID/arm
key private until quality annotations are frozen. A second reviewer should examine
all task-affecting errors and disputed completeness claims. If only one reviewer
is available, disclose that; do not call it independent double review.

Review navigation/tool sequences separately after quality is frozen. Annotate
readiness calls, discovery after sufficient evidence, overlapping questions,
native/MCP switching, argument errors, retries and verification of essential edges.
Give qualitative unnecessary/redundant labels only when supported by the trace
and the task; otherwise leave the scalar unavailable. A further call that verifies
a required causal edge is not waste merely because a shorter answer existed.

Retain provider input/output/total usage per request and cumulatively, reasoning
only when reported, tool-response bytes, native/MCP counts/errors, execution time,
termination kind and source-grounded quality. Keep tool/quality reviews and raw
results separate. Compare B/D and C/D primarily; also show A/B, B/C and A/D.
Report per-task paired differences, quality-first aggregate means/medians and
shared-task success-conditioned efficiency. Do not compare different success
cohorts as if task difficulty were matched. Fewer calls/tokens without maintained
quality is not a supported benefit.

## Expansion proposal and run estimate

After separate review of the eight-run validation, a possible repeated comparison
uses five tasks: `symbol_cli_config_flow`, `graph_nav_call_chain`,
`arch_search_subsystem`, `negative_search_session` and `smart_exclusions_probe`.
The first two retain a prior anchor and the one observed original-skill quality
gain; the other three are unused in the audited navigation runs and stay held out
from the proposed validation. This covers configuration navigation, execution,
architecture, an absence control and cross-cutting filtering. It does not establish
general change-impact/refactoring performance; the suite has no dedicated task.

| Stage | Tasks × arms × repetitions | Fresh navigation runs |
|---|---|---:|
| Initial technical/behavior validation | 2 × 4 × 1 | 8 |
| Optional repeated comparison, after review | 5 × 4 × 3 | 60 |
| Total if both stages are later approved | | **68** |

The eight validation observations are not pooled into the 60-run comparison.
Three repetitions support descriptive within-task variability but remain a small
sample. Report each repetition and the protocol/environment failures, and avoid
claims of significance based on a few tasks. No automatic escalation to the old
45-run benchmark or the proposed 60 runs is authorized.

With one two-request synthetic tool preflight per stage, expect **4 additional
generation requests** if both stages execute as single invocations. Agent sessions
use 1–25 generation requests each: at most 200 + 1500 navigation requests, plus
those 4 preflight requests, before any explicitly approved retry. These are request
bounds, not token or runtime predictions. Cold indexing/embedding work is separate.
Provider verification must not silently add uncounted generation requests.

Decide whether to expand after reviewing technical validity, blinded quality and
costs. Negative or inconclusive results are acceptable. A technical failure pauses
the design for investigation; it does not justify a hidden rerun. Freeze the skill
before validation and preserve failures. Any revision after seeing held-out answers
creates a new treatment/version and requires new held-out evaluation.
