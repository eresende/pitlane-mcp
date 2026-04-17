# Pitlane MCP Usage

Use `pitlane-mcp` for code lookup whenever it is available.

# Startup

1. Call `index_project` at the start of a session before using lookup tools.
2. Prefer `ensure_project_ready` for normal startup. It indexes the project and waits for embeddings only when needed.
3. If you use `index_project` directly and it returns `embeddings="running"`, call `wait_for_embeddings` immediately. Do not poll `get_index_stats` to wait for embeddings.
4. Call `watch_project` only when you expect the repo to change during the session. Do not start a watcher for a one-off read-only investigation.

# Navigation

1. Use `navigate_code` when the user intent is still fuzzy and you want the server to choose the best next navigation step.
2. Use `locate_code` when the user wants to find code but it is not yet clear whether the target is a symbol, file, or text fragment.
3. Use `read_code_unit` once you know the target and want the smallest useful read instead of manually choosing between `get_symbol`, `get_file_outline`, and `get_lines`.
4. Use `trace_execution_path` for behavior-level questions such as "where is X implemented?", "how does Y flow?", or "what is the main execution path?"
5. Use `trace_path` for source-to-sink, config-to-effect, shortest-path, and other explicit path questions.
6. Use `analyze_impact` for blast-radius questions before edits or refactors.
7. Use `search_symbols` when you know the target is a symbol and need direct symbol discovery by name or intent.
8. Use `search_content` when you know a text snippet, log string, import path, macro name, or regex fragment but do not know the symbol boundary yet.
9. Use `search_files` when you know or expect a file name, test file, directory pattern, or path suffix but do not yet know the exact symbol or file contents.
10. Use `get_index_stats` or `get_project_outline(summary=true)` to orient yourself in unfamiliar repos. Prefer `get_index_stats` first.
11. Use `find_usages` before refactoring a public symbol.
12. Fall back to direct file reads only when editing or when full-file context is genuinely required.
13. Treat `read_code_unit` as the preferred diff-aware read surface. Use its `read_state.status` field to decide whether to reuse the payload, expand, or reread:
   `new` means first read in this session
   `unchanged` means the same target was reread with identical content, so expand instead of rereading again
   `changed` means the same target changed since the previous read, so use the refreshed payload before expanding
14. When `locate_code`, `trace_path`, `analyze_impact`, or `navigate_code` return `session_state`, use it to understand whether the top target was already seen and whether the server intentionally promoted an unseen nearby alternative.

# Search Strategy

1. Prefer `locate_code` over manually choosing between `search_symbols`, `search_files`, and `search_content` when the query is ambiguous.
2. Use `mode="semantic"` when the user describes behavior, responsibility, or an execution path without naming an exact symbol.
3. Use `mode="exact"` or `mode="bm25"` when the user gives a concrete symbol name or a distinctive substring.
4. Write semantic queries as intent descriptions, not keyword bags. Use action + subject, for example `build regex matcher from CLI flags`.
5. If semantic results are weak, rephrase once with more context. Then fall back to one focused `bm25` or `exact` search.
6. Do not replace one semantic search with several broad guessed-keyword searches like `search`, `regex`, `walk`, or `printer`.
7. If you know text in the code but not the symbol, use `search_content` instead of shell `grep` or repeated guessed symbol searches.
8. If you know a file name, path fragment, or glob-like file pattern, use `search_files` instead of shell globbing or broad symbol searches.
9. Prefer `read_code_unit` over manually choosing between `get_symbol`, `get_file_outline`, and `get_lines` when the target is known.
10. After finding a promising symbol, use `read_code_unit` or `get_symbol` and use the returned references or steering hints before launching more searches.
11. For struct, class, interface, and trait symbols, `get_symbol` returns signature-only by default. Pass `signature_only=false` when you need the full body and references.
12. Do not use shell `grep`, globbing, or direct file-content search for code lookup when `pitlane-mcp` can answer the question.
13. If `locate_code` reports `session_state.novelty_bias_applied = true`, prefer the promoted unseen candidate before falling back to the older already-seen exact match.

# Execution-Path Questions

For architecture, pipeline, and execution-path questions:

1. Start with `navigate_code`, `trace_execution_path`, or `trace_path` depending on how explicit the path question is. Use `trace_path` when the user is asking for source-to-sink, config-to-effect, or shortest-path style tracing.
2. If the repo layout is unclear, use one root `get_project_outline(summary=true)` before adding `file=` filters. Do not assume a `src/` layout.
3. Identify the smallest useful path through the code. Usually this means:
   entry point
   orchestration layer
   scanning or input enumeration layer
   matcher or execution layer
   output layer
4. Stop searching once you can explain the path with concrete files and symbols.
5. Do not call `get_project_outline(summary=false)` unless the repo layout itself is the question.
6. Do not use `include_context=true` on `get_symbol` unless the symbol body alone is insufficient.
7. Do not start with broad single-word searches like `search`, `print`, `regex`, or `walk`.
8. Prefer `read_code_unit` once you know the target instead of broad file reads or manually selecting low-level read primitives.
9. After you have identified about 4 relevant symbols across the path, stop searching and synthesize the answer.
10. If the path hinges on a log string, import path, macro, or other text fragment rather than a symbol name, use `search_content` first, then pivot back to `locate_code`, `trace_execution_path`, `trace_path`, or `read_code_unit` once you know the relevant file or symbol.
11. Treat `find_callers` and `find_callees` as filtered graph views for quick checks. Use `trace_path` and `analyze_impact` when you need stronger path or blast-radius ranking.
