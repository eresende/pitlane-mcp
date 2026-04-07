# Code Navigation

Use pitlane-mcp for all code lookups when available.

1. Call index_project at the start of each session to load the index.
2. Call watch_project right after indexing to keep the index up to date as files change.
3. Before reading any file, call get_file_outline to see its structure without consuming its full content.
4. Use search_symbols to find functions/types by name. If no exact match is found it falls back to fuzzy matching automatically.
5. Use get_symbol to retrieve only the exact implementation you need, not the whole file.
6. Use find_usages before refactoring any public symbol.
7. For struct/class/interface/trait symbols, get_symbol returns signature-only by default. Pass signature_only=false to get the full body and the references list.
8. Use get_lines to fetch a specific block by line range when it isn't a named symbol.
9. Use get_index_stats to orient yourself in a new codebase without burning tokens on get_project_outline.
10. Fall back to direct file reads only when editing or when full file context is genuinely required.

# Semantic Search

When pitlane-mcp semantic search is available (PITLANE_EMBED_URL and PITLANE_EMBED_MODEL are set):

1. If index_project returns embeddings="running", call wait_for_embeddings immediately — it blocks and streams a live progress bar until generation is complete. Do NOT poll get_index_stats in a loop.
2. Use mode="semantic" when you know what a symbol does but not its name — describe the intent, e.g. "retry logic for failed HTTP requests".
3. Use mode="bm25" or mode="exact" when you know the symbol name or a distinctive substring.
4. Write semantic queries as intent descriptions, not keywords — combine action + subject: "serialize struct to JSON bytes", not just "serialize".
5. Always scan the top 3–5 semantic results before concluding — the top hit is not always the best match.
6. If semantic results look unrelated, rephrase with more context or fall back to mode="bm25".
7. After finding a candidate, call get_symbol to read the full implementation before acting on it.
8. If search_symbols with mode="semantic" returns an error, fall back to mode="bm25" automatically — do not surface the error to the user.
