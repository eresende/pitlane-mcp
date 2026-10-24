# Navigation benchmark infrastructure review — 2026-10-18

The infrastructure is ready for a standalone draft PR with offline regression
coverage. This review ran no navigation or synthesis inference. The published
navigation skill, task prompts, frozen task-quality-v1 rubric, production Rust,
historical reports and locally retained experiment artifacts remain unchanged.
The experimental lightweight skill and four-arm design are separate drafts and
are excluded from this infrastructure PR.

## Scope and reusable architecture

Reviewed all pending harness changes: the shared loop overrides, native grep
correction, opt-in experiment controller/configuration, provider, isolated tools,
technical gates, scoring/reporting, five diagnostic entrypoints, added regression
tests, reproduction instructions and nine historical Markdown reports. Existing
`AgenticLoop`, `BaselineExecutor`, MCP stdio transport, prompt loader and
`QualityScorer` remain the reusable execution components.

The opt-in A/B/C controller uses explicit API messages instead of a host agent
CLI. A/B receive the same neutral system prompt; C appends exact skill bytes.
Native tools are shared across all arms; B/C obtain the same MCP definitions.
Every attempt uses a fresh conversation/executor/MCP process. A has no MCP
process during navigation. A separate preparation process indexes the snapshot;
its timing is excluded from all arms' navigation measurements.

## Findings and corrections before the PR

| Area | Finding | Correction / disposition |
|---|---|---|
| Native search | Directory-only traversal silently missed explicit source files; missing paths looked like no matches | Shared file/directory implementation; explicit errors for invalid/missing/oversized selected files; retained directory recursion and limits; regression coverage |
| Termination | Visible content could be accepted with an unrecognized final finish reason | Strict provider requires `stop` for an answer; structured calls may use `stop` or `tool_calls`; `length` remains a failure; reasoning is never promoted to an answer |
| Malformed protocol | Some malformed response envelopes escaped protocol-error classification | Validate response/message/call shapes, string call IDs and function names; classify malformed envelopes as protocol failures |
| Usage | Preflight did not require internally consistent provider usage; smoke checked only availability | Preflight validates authoritative input/output/total sums; smoke and shared session checks validate per-request and cumulative sums |
| Context | Reported context mismatch was detected only after smoke execution | Refuse before task execution when discovered context differs from configured context; shared session checks also enforce input plus output allowance and request count |
| Binary identity | The full identity included the MCP binary, but pilot admission had no separately comparable binary fingerprint | Add explicit manifest fingerprint and require a matching fresh smoke; fail closed when it is absent |
| Input stability | Target bytes were checked, but harness/config/skill/prompt/binary changes during execution were not all checked by the controller | Verify pinned inputs before and after each attempt; stop further attempts on change |
| Wire isolation | The pilot audited the initial system message but did not reject a later injected system message | Require exactly the single expected system message; retain full per-request prompt/schema/inference-option checks |
| Readiness | Smoke/full lacked the pilot's shared checks for terminal/accounting/context/transport failures | Add a shared execution-only session gate and stop later attempts on its failure; correctness never enters it |
| Review coupling | Full admission parsed quality annotations even though quality was not its execution gate | Technical admission no longer reads manual reviews; strict and task-oriented quality stay post-execution measurements |
| Snapshot credentials | `.env` was excluded but tracked `.env.local` / `.env.production` could survive export | Exclude `.env.*` at any depth alongside existing instruction/config exclusions |
| Local trace privacy | Ignored traces inherited permissive default file modes | New output roots/wire directories use 0700; wire JSON uses 0600; no historical files are chmodded or rewritten |
| Automation | Existing CI covered Rust but not these Python regressions | Add an offline Python 3.12 harness job and pinned direct test dependencies |

These are infrastructure corrections after the pilot, not changes to the measured
pilot or evidence that the new revision has passed live technical readiness.
Execution-critical hashes changed: a future experiment needs a new technical
validation. Historical CLI smoke/pilot observations retain their original meaning
and source fingerprints. Do not retrofit the new gate into their recorded reports.

## Isolation and security review

Tracked-HEAD export omits nested instruction files, skill files, agent configuration
directories, `.git`, dotenv files and untracked files. It rejects escaping/broken
symlinks and archive paths outside the snapshot. Native/MCP argument validation
restricts repository access to that snapshot; model argument mistakes are counted,
not silently repaired. MCP receives an isolated HOME/cache, PATH and explicit
embedding variables. Watchers and repair/reindex controls are restricted during
task navigation. Cold preparation uses the advanced tier; agents receive the
default tier. This is a trusted local corpus/tool experiment, not an operating
system sandbox for malicious executables or repositories.

Endpoint selection remains runtime configuration. The dotenv reader reads a
literal selected variable without shell execution. Provider HTTP failures do not
serialize error bodies or authorization headers. Source-controlled candidates
were checked for credential patterns and the private Strata address; no matches
were found. Actual credentials are not needed for offline tests or CI. Raw
answers, reasoning, source/tool payloads, rendered prompts and arguments remain
under Git-ignored `results/`; none are included in the PR. A final staged-file
audit checks scope and credential leakage before publication.

Ignoring artifacts is distinct from making them public-safe. Do not force-add
results, upload CI artifacts containing conversations, or copy raw traces into
issues/PR descriptions. New output permissions protect diagnostic contents;
operator-controlled API metadata and public benchmark reports still require
review before publication.

## Correctness and reporting

The legacy reference-grounding proxy and strict 0/0.5/1 correctness are preserved.
Task-quality-v1 remains the separately versioned, evaluator-only component and
relationship rubric used for the pilot. Required-task errors and unsupported
required owners cap task quality; incidental errors remain visible and can lower
strict correctness. No review yields null, not assumed success. Boolean values
are rejected as manual strict scores.

Technical admission allows partial or wrong completed answers. Execution failure,
truncation, invalid accounting, transport/protocol failure or isolation/source
mutation stops execution. Successfully rejected bad tool arguments may be
recoverable agent observations. Efficiency summaries distinguish unconditional,
strict-success and task-success subsets and expose their sample sizes. Across-task
spread is not repeated-run variance. Exact repeated-call counters do not establish
semantic redundancy or a minimum necessary tool plan.

Token counts use provider usage consistently across the session, including
repeated history. A missing request metric makes the cumulative metric unknown.
Reported reasoning tokens are an output subset; unreported reasoning and
tool-response tokens are never estimated. Tool responses are measured in UTF-8
bytes. Per-request context and cumulative input consumption remain distinct.

## Tests and reproduction

Offline validation: **172 passed, 6 integration tests deselected** on Python
3.12.3. Regression coverage includes legacy prompts/default loop behavior, bounded
synthesis, skill/tool isolation, provider envelopes/finish reasons, authoritative
accounting, wire contamination, technical admission independent of correctness,
binary mismatch/missing fingerprint, snapshots/symlinks/dotenv variants, native
file/directory grep, failure accounting, rotation, rubric validation and private
trace permissions. No inference service or API credential is used by this check.

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r bench/harness/tests/requirements.txt
.venv/bin/python -m pytest bench/harness/tests -m 'not integration' -q
git diff --check
```

See [experiment instructions](experiments/README.md) for runtime endpoint/key
configuration, preflight, fresh smoke, manual review, pilot and independent report
generation. Invocation never automatically expands into a full benchmark.

## Compatibility and remaining limits

- Existing benchmark entrypoints, default prompts, provider adapters and legacy
  artifact schemas remain intact. New loop options default off. Native grep's
  source-file fix and invalid-path errors are intentional shared behavior changes.
- Additive experiment fields retain schema version 1; historical files remain
  readable. Stricter future admission deliberately rejects older evidence that
  lacks the new integrity fields; this is not a rescore of old answers.
- New source hashes and a PR commit make future runs reproducible from a clean
  checkout. Historical runs predate this commit; their dirty-source hashes and
  private artifacts, rather than the new commit alone, identify their exact code.
- General protocol/accounting logic is tested offline. The new checks have not
  been validated against live Strata in this review. Docker template verification
  is deliberately specific to the inspected deployment and needs its local
  source/tokenizer/container; it is not a generic provider-effort capability.
- Private historical checkpoints are not shipped. Public reproduction can create
  new observations from pinned repository/model/tool settings, but cannot recreate
  the exact historical raw conversations from Markdown summaries alone.
- Manual grading and qualitative tool-selection reviews still need independent
  source evidence. Pilot review was label-aware and single-reviewer. The planned
  four-arm design should blind answer quality before revealing tool traces.
- Provider cache/warm-up state and transient GPU pressure are incompletely
  observed; rotation reduces systematic order bias without eliminating it.

No model inference, skill publication, production Rust change or merge is part
of this review. The draft infrastructure PR is the review boundary.
