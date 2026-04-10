# Pitlane MCP Usage

Use `pitlane-mcp` for code lookup whenever it is available.

# Startup

1. Call `index_project` at the start of a session before using lookup tools.
2. Prefer `ensure_project_ready` for normal startup. It indexes the project and waits for embeddings only when needed.
3. If you use `index_project` directly and it returns `embeddings="running"`, call `wait_for_embeddings` immediately. Do not poll `get_index_stats` to wait for embeddings.
4. Call `watch_project` only when you expect the repo to change during the session. Do not start a watcher for a one-off read-only investigation.

# Navigation

1. Use `search_symbols` to discover symbols by name or intent.
2. Use `search_content` when you know a text snippet, log string, import path, macro name, or regex fragment but do not know the symbol boundary yet.
3. Use `search_files` when you know or expect a file name, test file, directory pattern, or path suffix but do not yet know the exact symbol or file contents.
4. Use `trace_execution_path` when the user asks for a behavior-level path such as "where is X implemented?", "how does Y flow?", or "what is the main execution path?" and you want a compact set of key files, symbols, and edges in one step.
5. Use `get_symbol` to read the exact implementation you need instead of reading whole files.
6. Use `get_file_outline` when you know the file but not the symbol, or when you need to inspect file structure before choosing symbols.
7. Use `get_lines` only for non-symbol code blocks or when symbol boundaries are not enough.
8. Use `get_index_stats` or `get_project_outline(summary=true)` to orient yourself in unfamiliar repos. Prefer `get_index_stats` first.
9. Use `find_usages` before refactoring a public symbol.
10. Fall back to direct file reads only when editing or when full-file context is genuinely required.

# Search Strategy

1. Use `mode="semantic"` when the user describes behavior, responsibility, an execution path, or "where is X implemented?" without naming an exact symbol.
2. Use `mode="exact"` or `mode="bm25"` when the user gives a concrete symbol name or a distinctive substring.
3. Write semantic queries as intent descriptions, not keyword bags. Use action + subject, for example `build regex matcher from CLI flags`.
4. If semantic results are weak, rephrase once with more context. Then fall back to one focused `bm25` or `exact` search.
5. Do not replace one semantic search with several broad guessed-keyword searches like `search`, `regex`, `walk`, or `printer`.
6. If you know text in the code but not the symbol, use `search_content` instead of shell `grep` or repeated guessed symbol searches.
7. If you know a file name, path fragment, or glob-like file pattern, use `search_files` instead of shell globbing or broad symbol searches.
8. For behavior or execution-path questions, prefer `trace_execution_path` before manually chaining many `search_symbols` and `get_symbol` calls.
9. After finding a promising symbol, switch to `get_symbol` and use its `references` to trace related layers before launching more searches.
10. For struct, class, interface, and trait symbols, `get_symbol` returns signature-only by default. Pass `signature_only=false` when you need the full body and references.
11. Do not use shell `grep`, globbing, or direct file-content search for code lookup when `pitlane-mcp` can answer the question.

# Execution-Path Questions

For architecture, pipeline, and execution-path questions:

1. Start with `trace_execution_path` when the user is asking for a behavior-level path through the codebase. If you need lower-level symbol discovery first, start with one semantic search unless the prompt already names the symbol.
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
8. Do not call `get_file_outline` until after you have found at least one relevant symbol, unless the user explicitly asked about file structure.
9. After you have identified about 4 relevant symbols across the path, stop searching and synthesize the answer.
10. If the path hinges on a log string, import path, macro, or other text fragment rather than a symbol name, use `search_content` first, then pivot back to `search_symbols`, `trace_execution_path`, or `get_symbol` once you know the relevant file or symbol.
