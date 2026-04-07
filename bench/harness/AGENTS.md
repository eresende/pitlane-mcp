# Code Navigation

Use pitlane-mcp when it is likely to reduce total search effort.

1. Only call `index_project` once per new repo/session when Pitlane is actually needed.
2. Only call `watch_project` if the task involves file edits or the repo may change during the session.
3. Do not call `get_file_outline` before every file read. Use it only for large or unfamiliar files.
4. For exact symbol lookups, prefer the cheapest path first:
   - `search_symbols` with `mode="exact"` or `mode="bm25"`
   - direct targeted read if the file is already obvious
5. Use `get_symbol` when you need a specific implementation body without reading the full file.
6. Use `find_usages` mainly for public APIs, refactors, or cross-file behavior questions.
7. Use `get_lines` only when a symbol is unnamed or line-specific context is needed.
8. Fall back to direct file reads when they are cheaper than multiple MCP calls.

# Semantic Search

Use semantic search only when names are unknown or keyword search is weak.

1. If semantic search is available and the task is concept-based, use `mode="semantic"`.
2. If the symbol name is known, use `mode="exact"` or `mode="bm25"` first.
3. Do not scan 3–5 semantic results by default. Start with the top 1–2 and expand only if needed.
4. If semantic results look unrelated, rephrase once, then fall back to `bm25`.
5. After identifying a likely candidate, read only the minimum needed symbol or lines.
6. Avoid semantic search for obvious central files or exact class/function-name prompts.