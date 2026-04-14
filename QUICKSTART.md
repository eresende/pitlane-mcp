# pitlane-mcp Quick Start

This guide is for a developer who wants to become productive quickly with `pitlane-mcp`.

`pitlane-mcp` is a code-intelligence MCP server. It indexes a repository once, then helps your agent retrieve the exact symbol, file outline, or execution path it needs instead of reading whole files.

## 1. Install

Install from crates.io:

```bash
cargo install pitlane-mcp
```

Install with `cargo-binstall`:

```bash
cargo binstall pitlane-mcp
```

Build from source:

```bash
cargo build --release
cp target/release/pitlane-mcp ~/.local/bin/
cp target/release/pitlane ~/.local/bin/
```

## 2. Configure Your MCP Client

### Claude Code

```bash
claude mcp add pitlane-mcp -- pitlane-mcp
```

### OpenCode

Add this to `opencode.json` or `opencode.jsonc`:

```json
{
  "mcp": {
    "pitlane-mcp": {
      "type": "local",
      "command": ["pitlane-mcp"]
    }
  }
}
```

### VS Code / Kiro

Add this to `.vscode/mcp.json` or `.kiro/settings/mcp.json`:

```json
{
  "mcp": {
    "servers": {
      "pitlane-mcp": {
        "type": "stdio",
        "command": "pitlane-mcp",
        "args": []
      }
    }
  }
}
```

## 3. Learn the Tool Hierarchy

The fastest way to use `pitlane-mcp` correctly is to use tools by intent.

- `ensure_project_ready`: startup
- `search_symbols`: find a symbol by name or responsibility
- `search_content`: find a text snippet, log line, import path, macro, or regex
- `search_files`: find a file by path or file name
- `trace_execution_path`: answer "where is this implemented?" or "how does this flow?"
- `get_symbol`: read one exact implementation
- `find_callers`, `find_callees`, `find_usages`: impact analysis

Rule of thumb: do one discovery step, then switch to `get_symbol`.

## 4. Default Workflow

For most tasks, follow this sequence:

1. Call `ensure_project_ready`.
2. Choose one discovery tool based on what you know.
3. Call `get_symbol` as soon as you have a strong candidate.
4. Use graph tools only if you need callers, callees, or usages.

Do not treat the tool like a file browser. The goal is to stop searching as soon as you have the exact symbol or execution path you need.

## 5. First Prompt to Try

Use this when opening a repo for the first time:

```text
Use `pitlane-mcp` for code lookup in this repo.
First call `ensure_project_ready`.
Then choose exactly one discovery tool based on the task.
Switch to `get_symbol` as soon as you find a strong candidate.
Avoid broad file reads, shell grep, or repeated guessed searches.
```

## 6. Practical Prompt Recipes

### A. Behavior / Flow Question

Use this when you want to understand how something works:

```text
Use `pitlane-mcp` for code lookup.
Call `ensure_project_ready`, then `trace_execution_path` for:
"how does request authentication flow?"
After that, inspect only the most relevant 1-2 symbols with `get_symbol`.
```

Examples:

- `how does request authentication flow?`
- `where is retry backoff implemented?`
- `what is the main execution path for background jobs?`

### B. Known Symbol Name

Use this when you already know the symbol:

```text
Use `pitlane-mcp`.
Call `ensure_project_ready`, then `search_symbols` for `build_query_plan`.
Use `get_symbol` on the best match and summarize the implementation.
```

Examples:

- `find the implementation of build_query_plan`
- `show me what FooService::execute does`
- `locate the method refresh_cache`

### C. Known Text Snippet or Log Line

Use this when you know a string that appears in code:

```text
Use `pitlane-mcp`.
Call `ensure_project_ready`, then `search_content` for `retrying failed job`.
Pivot to `get_symbol` once the relevant symbol is identified.
```

Examples:

- `find where "retrying failed job" comes from`
- `locate the code that emits "cache miss for tenant"`
- `search for the import path "crate::auth::jwt"`

### D. Known File Shape

Use this when you know the file name or path pattern, but not the symbol:

```text
Use `pitlane-mcp`.
Call `ensure_project_ready`, then `search_files` for `auth middleware`.
If a likely file is found, inspect the file outline, then use `get_symbol`.
```

Examples:

- `find the auth middleware file`
- `look for tests related to query planner`
- `find the repo file that likely owns cache invalidation`

### E. Safe Refactor / Impact Check

Use this before changing a public symbol:

```text
Use `pitlane-mcp`.
Call `ensure_project_ready`, then find `FooService::execute` with `search_symbols`.
Read it with `get_symbol`, then run `find_usages` and `find_callers` before proposing edits.
```

Examples:

- `before changing FooService::execute, find all usages and callers`
- `show the impact of renaming BuildPlan`
- `check who depends on refresh_cache`

### F. Orientation in an Unfamiliar Repo

Use this when you need a quick repo map:

```text
Use `pitlane-mcp`.
Call `ensure_project_ready`, then `get_index_stats`.
If needed, call `get_project_outline(summary=true)`.
Do not read whole files unless symbol-level lookup is insufficient.
```

## 7. What Good Prompts Look Like

Good prompts are specific and intent-driven.

Good:

- `where is request validation implemented?`
- `how does the background job retry path work?`
- `find the symbol that builds SQL filters`
- `locate the code behind this log line: "cache miss for tenant"`

Weak:

- `search auth`
- `look around src`
- `grep for retry`
- `read the relevant files`

If you can phrase the task as behavior, responsibility, or a concrete text anchor, `pitlane-mcp` performs much better.

## 8. Common Mistakes

Avoid:

- reading whole files too early
- running many broad searches in a row
- using shell `grep` when `search_content` fits
- blocking on `wait_for_embeddings` unless semantic search is required immediately
- using graph tools before identifying the target symbol

Prefer:

- `ensure_project_ready`
- one focused discovery call
- `get_symbol`
- graph tools only when the task requires impact analysis

## 9. Fastest Way to Learn

Do these three exercises on any repo:

1. Ask: `Where is request parsing implemented?`
   Use `trace_execution_path`, then `get_symbol`.
2. Ask: `Find the implementation of <known symbol>`
   Use `search_symbols`, then `get_symbol`.
3. Ask: `Find where this log line comes from: "<real log text>"`
   Use `search_content`, then `get_symbol`.

After those three, the workflow is usually clear.

## 10. Daily-Use Template

If you want one reusable instruction block for your agent, use this:

```text
Use `pitlane-mcp` for code lookup in this repo.

Workflow:
1. Call `ensure_project_ready` first.
2. Pick exactly one discovery tool:
   - `trace_execution_path` for behavior or flow questions
   - `search_symbols` for known names or responsibilities
   - `search_content` for text, logs, imports, macros, or regex snippets
   - `search_files` for file or path discovery
3. Switch to `get_symbol` as soon as a strong candidate is found.
4. Use `find_callers`, `find_callees`, or `find_usages` only for impact analysis.
5. Avoid broad file reads, shell grep, and repeated guessed searches when `pitlane-mcp` can answer directly.
```
