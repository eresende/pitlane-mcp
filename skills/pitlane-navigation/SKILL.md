---
name: pitlane-navigation
description: Use PitlaneMCP for token-efficient codebase exploration, symbol discovery, execution-path tracing, change-impact analysis, and indexed documentation lookup when its MCP tools are configured and available.
---

# PitlaneMCP navigation

Use this skill when exploring a codebase and the `pitlane-mcp` MCP server is available. PitlaneMCP indexes source using tree-sitter and provides focused reads and graph-based navigation. This skill does **not** install the server; see [setup](README.md) and [config examples](configs/).

## Choose the right tool

- For a simple exact string lookup or an edit requiring whole-file context, the agent's native tools may be faster. Do not route every operation through PitlaneMCP without reason.
- For ambiguous symbol or file discovery, use `locate_code`.
- For broad "how does this work?" questions requiring source evidence, use `investigate`.
- For a known target, use `read_code_unit` to retrieve the smallest useful unit.
- For entry-to-output, source-to-sink, or config-to-effect paths, use `trace_path`.
- For refactoring blast radius, use `analyze_impact`; for a Git diff, use `analyze_changes`.
- For a known log string, import, macro, text fragment, or regex, use `search_content`.
- For repository Markdown or runbooks, use `search_knowledge`, then `read_knowledge_document` for the relevant section.
- If MCP is missing, failing, or clearly unhelpful for the task, fall back to native navigation. Do not block completion.

## Start and discover

1. Confirm PitlaneMCP tools are exposed to this agent. If absent, refer the user to setup rather than imagining tool results.
2. Call `ensure_project_ready` once for the project when indexing is needed. It creates/checks the index and reports whether embeddings are still running; it does not block for them.
3. Use `get_index_stats` only when repository scope or index state is useful to the question. Do not repeatedly poll it to wait for embeddings.
4. Use default-tier tools by default. Advanced primitives require the server to run with `PITLANE_MCP_TOOL_TIER=all`.

## Search deliberately

1. Use `locate_code` rather than guessing whether a query is a symbol, file or text fragment.
2. `locate_code` selects its search strategy automatically; it has no `mode` parameter. Describe behaviour or responsibility in the query for semantic discovery, which falls back to lexical search when embeddings are unavailable.
3. For a known symbol, give `locate_code` its concrete name; for a distinctive text fragment, use `search_content`.
4. Write semantic queries as an action plus subject (for example, "build regex matcher from CLI flags"), not a bag of generic keywords.
5. If semantic matches are weak, rephrase once; then use a concrete symbol name with `locate_code` or a known text fragment with `search_content` rather than many broad queries.
6. Use `read_code_unit` after finding a target, and expand only as necessary. Honor `read_state.status`: `unchanged` means expand rather than repeat an identical read; `changed` means use refreshed content.
7. Pay attention to `session_state` from `locate_code`, `trace_path`, and `analyze_impact` when present, especially whether an unseen relevant alternative was promoted.

## Trace and explain

- Prefer the smallest evidence-backed path from entry point through orchestration, input/scanning, execution/matching, and output as relevant.
- Start with `investigate` for a broad question or `trace_path` for an explicitly path-oriented question.
- Stop when enough named files and symbols support the answer; avoid unrelated branches and speculative hops.
- When a path hinges on a string rather than a symbol, use `search_content` before pivoting to structured navigation.
- Report exact symbols and files, and distinguish verified code evidence from inference.

## Documentation and change analysis

- Use `search_knowledge` for documentation discovery, including OKF metadata filters where applicable. Read focused sections with `read_knowledge_document`.
- Use `analyze_impact` for dependency/blast-radius questions and `analyze_changes` for evidence tied to an actual diff.
- Keep output bounded to the user's question. Prefer a few high-relevance symbols over an exhaustive dump.

## Advanced tier (only when explicitly enabled)

- After direct `index_project`, if it reports `embeddings="running"`, use `wait_for_embeddings` rather than polling stats.
- Use `watch_project` when repository changes need monitoring during the session.
- Use `search_files` for known file fragments, `search_symbols`/`get_symbol` for known symbols, and `find_callers`/`find_callees` for small graph checks.
- With `search_symbols`, use `mode="semantic"` for behaviour/responsibility descriptions when embeddings are available, or `mode="exact"` / `mode="bm25"` for concrete symbol names or distinctive substrings.
- Avoid expensive broad outlines unless repository structure is the question.

## Scope and safety

Follow the host agent's permissions and the user's instructions. This skill guides navigation only; it does not authorize running arbitrary commands, changing files or taking destructive actions.

Project: https://github.com/eresende/pitlane-mcp
