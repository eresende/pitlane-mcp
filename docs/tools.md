# Tool Reference

This document describes the MCP tools exposed by `pitlane-mcp`.

## Public Tier

These tools are visible by default and are the recommended surface for AI agents.

### `ensure_project_ready`

Prepare a repo for navigation and report indexing or embedding readiness.

```json
{ "path": "/your/project" }
```

Notes:

- Ensures the on-disk index exists or is refreshed if needed
- Reports whether embeddings are still running
- Does not block on embeddings
- Accepts `exclude`, `force`, and `max_files`
- Accepts `poll_interval_ms` and `timeout_secs` for compatibility, but they are currently ignored

### `investigate`

Answer a broad code question in one call by discovering relevant symbols and returning source inline.

```json
{ "project": "/your/project", "query": "How does ignore/gitignore handling work?" }
```

Use this first for broad code questions such as subsystem, behavior, and execution-path questions.

Optional parameters:

- `language`
- `scope`

### `locate_code`

Resolve an ambiguous query into the most likely symbol, file, or content lookup path.

```json
{ "project": "/your/project", "query": "config loader", "intent": "symbol" }
```

Use this when you need discovery without full source.

### `read_code_unit`

Read the smallest useful code unit for a known target.

```json
{ "project": "/your/project", "symbol_id": "src/auth.rs::Auth::login#method" }
{ "project": "/your/project", "file_path": "src/auth.rs", "line_start": 20, "line_end": 60 }
```

Use this instead of manually choosing between symbol, file-outline, and line-slice primitives.

Responses include `read_state` with `new`, `unchanged`, or `changed` guidance.

### `trace_path`

Trace a likely execution or data-flow path from a behavior question or source/sink hints.

```json
{ "project": "/your/project", "query": "config to HTTP handler path" }
```

Use this for source-to-sink, config-to-effect, and shortest-path style questions.

### `analyze_impact`

Estimate the blast radius of changing a symbol, file, or concept.

```json
{ "project": "/your/project", "query": "Auth::login", "depth": 2 }
```

Use this before edits or refactors.

### `get_index_stats`

Return lightweight repo orientation data such as language and symbol counts.

```json
{ "project": "/your/project" }
```

Use this before broader exploration when you want orientation, not structure.

### `search_content`

Search indexed source text for a known snippet, log string, import path, or regex fragment.

```json
{ "project": "/your/project", "query": "RegexMatcherBuilder::new" }
```

Prefer this over shell `grep`.

## Advanced Tier

These tools are hidden from `tools/list` unless you start the server with:

```bash
PITLANE_MCP_TOOL_TIER=all pitlane-mcp
```

Advanced tools:

- `index_project`
- `search_symbols`
- `search_files`
- `navigate_code`
- `trace_execution_path`
- `get_symbol`
- `get_file_outline`
- `get_lines`
- `get_project_outline`
- `find_callees`
- `find_callers`
- `find_usages`
- `watch_project`
- `get_usage_stats`
- `wait_for_embeddings`

### When To Use Advanced Tools

- Use `index_project` only when you explicitly want lower-level startup control.
- Use `search_symbols` or `search_files` only when you already know the target class of lookup.
- Use `get_symbol`, `get_file_outline`, and `get_lines` only when you deliberately want the lower-level primitive instead of `read_code_unit`.
- Use `trace_execution_path` and `navigate_code` only when you deliberately want the advanced orchestration surface.
- Use `find_callers`, `find_callees`, and `find_usages` for raw graph views.
- Use `watch_project` only for long-lived sessions.
- Use `wait_for_embeddings` only after a direct `index_project` call reports `embeddings: "running"` and you explicitly need semantic readiness.

## Symbol IDs

Stable symbol IDs use:

```text
{relative_path}::{qualified_name}#{kind}
```

Examples:

```text
src/audio/engine.rs::Engine::process_block#method
src/models/user.py::UserService::authenticate#method
src/api/client.ts::fetchUser#function
```

They are returned by search and outline tools and used as input to symbol-centric tools.
