# Task quality v1 — fixed before the 15-run pilot

This rubric is evaluator-only. Never append it, reviewer source anchors, prior
answers or corrected facts to model inputs. It applies identically to A/B/C.
The exact check IDs live in `task_quality.py`; both files are hashed into the
pilot manifest before inference. Historical scores and reports are not rewritten.

## Two independent measurements

**Strict correctness remains unchanged:** 1 means complete and factually correct;
0.5 means partial/incomplete or a substantive positive factual error, including an
optional detail; 0 means incorrect or no usable task answer. Record repository
evidence. Only a completed answer with strict 1 is a strict success. The legacy
reference-grounding score remains a separate, unchanged lexical proxy.

**Task-oriented quality** measures requested work separately from incidental
precision. Score each preregistered component and essential relationship:
1 correctly identified/explained with repository-grounded ownership and meaning;
0.5 partly correct, ambiguous or incomplete; 0 absent or wrong. A relationship
requires an explanation of what invokes, configures or changes what; a list of
symbols alone is insufficient. Equivalent correct source paths/abstractions are
accepted; no one canonical navigation sequence or naming inventory is required.

Component score is the mean of component checks; relationship score is the mean
of relationship checks. Task quality is their equally weighted mean. Any factual
error changing a requested owner, operation, decision or causal edge, or any
unsupported/invented required symbol, caps task quality at 0.5. Record all such
errors with evidence. Optional invented symbols are recorded separately and as
incidental errors if they do not change the essential answer. Unsupported symbols
include real names falsely qualified with the wrong file/owner; do not rely on
the lexical grounding proxy to detect this.

An incidental error is in optional detail whose removal leaves every requested
component and essential relationship correct. It does not lower task quality,
but remains visible and can lower strict correctness. Do not excuse a wrong
required relationship as incidental merely because its explanation is detailed.
Do not penalize omission of optional details. Uncertainty is not a false claim,
but missing required evidence still lowers the corresponding coverage check.

A task-oriented success requires completed execution and task quality 1. It is
distinct from strict success and may coexist with incidental errors. Unreviewed
quality is null, never inferred from `stop`, token use or tool choice. Unfinished
answers remain unfinished; no successful status is manufactured from fragments.

## Task scope fixed before new runs

| Task | Required components | Essential relationships |
|---|---|---|
| symbol_ignore_logic | Ignore rules, traversal, central source owners | Traversal consults matching; matching affects visitation/pruning |
| symbol_cli_config_flow | Parsing, runtime configuration, search execution, central owners | Parsing updates options; conversion to runtime configuration; runtime configuration drives search |
| graph_nav_call_chain | Entry/dispatch, orchestration, regex execution, output writer, owners | Entry → search; search → matching; match → printing; printing → stdout |
| arch_package_map | CLI, traversal, ignore, regex execution, formatting, central files | CLI/orchestration; traversal/ignore; distinct matcher/searcher/printer roles |
| semantic_search_probe | Binary detection, binary handling, source owners | Detection trigger; configuration/policy; effects when binary data is encountered |

No exhaustive flags, every internal helper, every output mode, or all ignore-rule
precedence cases are required unless the answer positively claims those details.
For binary behavior, detection and the difference between skipping/quitting and
searching binary data are part of the task, not incidental trivia. For stdout,
the sink/writer connection is required; listing a printer crate alone is partial.

## Readiness, review and stopping

Technical readiness concerns connectivity, actual structured tool calling,
isolation, unchanged controls, valid accounting and recognized final termination.
It does not depend on either quality score. A reviewed technical smoke may
authorize a fresh pilot even when every historical strict score is 0.5.

During the pilot stop on timeout/truncation/no final answer, provider/protocol or
tool-transport failure, changed model/controls/schema, isolation/source mutation,
missing/inconsistent token usage, or context overflow. Correctly returned tool
errors (e.g. rejected arguments, missing native files, MCP semantic errors) are
agent observations if the session recovers and finishes; record them and do not
hide retries. They are not automatically evidence of harness malfunction.

Review after execution, preferably without arm labels, against the frozen target.
Record strict score, every check's evidence, task/incidental errors, and unsupported
symbols. A single assistant source review is not an independent second reviewer;
report that limitation. Report both strict- and task-success-conditioned efficiency
with sample sizes/task composition, as well as unconditional measurements.
One run per task/arm cannot establish repeatability or statistical significance.
