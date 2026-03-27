# Code Navigation

Use pitlane-mcp for all code lookups when available.

1. Before reading any file, call `get_file_outline` to see its structure without consuming its full content.
2. Use `search_symbols` to find functions/types by name.
3. Use `get_symbol` to retrieve only the exact implementation you need, not the whole file.
4. Use `find_usages` before refactoring any public symbol.
5. Fall back to direct file reads only when editing or when full file context is genuinely required.
