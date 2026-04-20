# Code Navigation

This project is indexed by pitlane-mcp. Always use pitlane tools instead of bash, grep, glob, or reading files directly.

## Quick Start

1. Call `ensure_project_ready` once at the start.
2. For any code question, call `investigate` first — it returns source code in one call.
3. If you need more detail after `investigate`, use `read_code_unit(symbol_id=...)` with a symbol ID from the response.

## Rules

- **DO** use `investigate` for broad questions like "how does X work?" or "where is Y implemented?"
- **DO** use `locate_code` when you need to find a specific symbol or file without reading its source.
- **DO** use `read_code_unit` to read a specific symbol body or file outline.
- **DO** use `trace_path` for execution-path or source-to-sink questions.
- **DO** use `search_content` when you know exact text in the code but not which symbol contains it.
- **DO NOT** use `bash`, `grep`, `glob`, or `read` for code exploration. The pitlane tools are faster and return less noise.
- **DO NOT** read entire files. Use `read_code_unit(symbol_id=...)` to read only the symbol you need.
- **DO NOT** list directories or glob for files. Use `locate_code` or `get_index_stats` instead.

## Tool Priority

1. `investigate` — answers most questions in one call
2. `locate_code` — finds symbols/files when you just need discovery
3. `read_code_unit` — reads a specific symbol or file outline
4. `trace_path` — traces execution paths
5. `search_content` — finds text patterns in code
6. `analyze_impact` — checks blast radius before edits

## What NOT to Do

- Do not run `find`, `ls`, `cat`, `grep`, or `bash` to explore code
- Do not read whole files with `read` — use `read_code_unit` with a symbol_id
- Do not call multiple discovery tools for the same question — `investigate` does it all in one call
