---
name: pitlane-navigation-lightweight
description: Select focused repository navigation tools and verify source ownership and execution relationships when PitlaneMCP is available.
---

# Focused repository navigation

Choose a lookup for a specific unanswered part of the user's task. Use the supplied
project root. On a prepared repository, start with task lookup; skip readiness and
stats calls. If navigation reports an index problem, use `ensure_project_ready`
once. Request stats only to resolve an actual scope or index-state uncertainty.

Choose the starting tool by the information needed:

- Known target: `read_code_unit`; do not rediscover it.
- Unclear symbol or file: `locate_code`.
- Broad responsibility or architecture: `investigate`.
- Execution or configuration-to-effect flow: `trace_path`.
- Known text: `search_content` or a focused native search.
- Change consequences: `analyze_impact`; use `analyze_changes` for a Git diff.
- Documentation: `search_knowledge`, then `read_knowledge_document` if needed.

Use a focused native read/search when it supplies the needed evidence directly,
or when MCP is unavailable or unhelpful. After discovery, read the missing source
unit rather than launching another broad discovery. Switch tools to close a named
evidence gap, rather than repeating the same search through another interface.
On an argument error, inspect the schema/error before correcting the call.

Before making a claim, verify its file and exact symbol owner from source. A
matching method name does not establish its containing type or implementation.
For an essential relationship, check the relevant caller/configuration site and
the behavior it invokes or changes. A graph edge or signature alone may not prove
the decision, policy or state transition. Read further when this affects the
requested answer; label inference and unresolved evidence explicitly.

Synthesize once source evidence covers the requested components and essential
relationships. Additional calls should resolve a specific remaining requirement
or contradiction. Reuse returned source; expand a truncated read instead of
requesting the same prefix. Omit unnecessary optional details rather than guessing
their implementation. If required evidence remains unavailable, identify the gap.
Prefer a correct supported explanation over either fewer calls or a larger symbol
inventory; there is no fixed tool-call quota.
