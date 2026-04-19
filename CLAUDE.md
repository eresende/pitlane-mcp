# Pitlane MCP Usage

Use `pitlane-mcp` for code lookup whenever it is available.

By default, the public tool tier is small:
- `ensure_project_ready`
- `locate_code`
- `read_code_unit`
- `trace_path`
- `analyze_impact`
- `get_index_stats`
- `search_content`

Advanced primitive tools are hidden from `tools/list` unless the server is started with `PITLANE_MCP_TOOL_TIER=all`.

# Startup

1. Prefer `ensure_project_ready` for normal startup.
2. If you explicitly exposed the advanced tool tier and use `index_project` directly, call `wait_for_embeddings` immediately when it returns `embeddings="running"`. Do not poll `get_index_stats` to wait.
3. Call `watch_project` only when you explicitly exposed advanced tools and expect the repo to change during the session.

# Navigation

1. Use `locate_code` when the user wants to find code but it is not yet clear whether the target is a symbol, file, or text fragment.
2. Use `read_code_unit` once you know the target and want the smallest useful read instead of manually choosing between lower-level read primitives.
3. Use `trace_path` for behavior, source-to-sink, config-to-effect, shortest-path, and other execution-path questions.
4. Use `analyze_impact` for blast-radius questions before edits or refactors.
5. Use `search_content` when you know a text snippet, log string, import path, macro name, or regex fragment but do not know the symbol boundary yet.
6. Use `get_index_stats` to orient yourself in unfamiliar repos before broader exploration.
7. Fall back to direct file reads only when editing or when full-file context is genuinely required.
8. Treat `read_code_unit` as the preferred diff-aware read surface. Use its `read_state.status` field to decide whether to reuse the payload, expand, or reread:
   `new` means first read in this session
   `unchanged` means the same target was reread with identical content, so expand instead of rereading again
   `changed` means the same target changed since the previous read, so use the refreshed payload before expanding
9. When `locate_code`, `trace_path`, or `analyze_impact` return `session_state`, use it to understand whether the top target was already seen and whether the server intentionally promoted an unseen nearby alternative.

# Search Strategy

1. Prefer `locate_code` over manually choosing between `search_symbols`, `search_files`, and `search_content` when the query is ambiguous.
2. Use `mode="semantic"` when the user describes behavior, responsibility, or an execution path without naming an exact symbol.
3. Use `mode="exact"` or `mode="bm25"` when the user gives a concrete symbol name or a distinctive substring.
4. Write semantic queries as intent descriptions, not keyword bags. Use action + subject, for example `build regex matcher from CLI flags`.
5. If semantic results are weak, rephrase once with more context. Then fall back to one focused `bm25` or `exact` search.
6. Do not replace one semantic search with several broad guessed-keyword searches like `search`, `regex`, `walk`, or `printer`.
7. If you know text in the code but not the symbol, use `search_content` instead of shell `grep` or repeated guessed symbol searches.
8. If you have explicitly exposed advanced tools and know a file name or path fragment, use `search_files` instead of shell globbing or broad symbol searches.
9. Prefer `read_code_unit` over low-level read primitives when the target is known.
10. If you have explicitly exposed advanced tools and know the target is a symbol, use `search_symbols` or `get_symbol` directly.
11. Do not use shell `grep`, globbing, or direct file-content search for code lookup when `pitlane-mcp` can answer the question.
12. If `locate_code` reports `session_state.novelty_bias_applied = true`, prefer the promoted unseen candidate before falling back to the older already-seen exact match.

# Execution-Path Questions

For architecture, pipeline, and execution-path questions:

1. Start with `trace_path` for execution-path, source-to-sink, config-to-effect, and shortest-path style tracing.
2. If the repo layout is unclear, use `get_index_stats` first. If you have explicitly exposed advanced tools and need structure detail, then use one root `get_project_outline(summary=true)` before adding `file=` filters.
3. Identify the smallest useful path through the code. Usually this means:
   entry point
   orchestration layer
   scanning or input enumeration layer
   matcher or execution layer
   output layer
4. Stop searching once you can explain the path with concrete files and symbols.
5. Do not call `get_project_outline(summary=false)` unless the repo layout itself is the question and you have exposed advanced tools.
6. Do not use `include_context=true` on low-level symbol reads unless the symbol body alone is insufficient.
7. Do not start with broad single-word searches like `search`, `print`, `regex`, or `walk`.
8. Prefer `read_code_unit` once you know the target instead of broad file reads or manually selecting low-level read primitives.
9. After you have identified about 4 relevant symbols across the path, stop searching and synthesize the answer.
10. If the path hinges on a log string, import path, macro, or other text fragment rather than a symbol name, use `search_content` first, then pivot back to `locate_code`, `trace_path`, or `read_code_unit` once you know the relevant file or symbol.
11. If you have explicitly exposed advanced tools, treat `find_callers` and `find_callees` as filtered graph views for quick checks. Otherwise prefer `trace_path` and `analyze_impact`.
