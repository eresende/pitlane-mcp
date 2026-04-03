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
