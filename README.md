# pitlane-mcp

[![CI](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml)

Token-efficient code intelligence MCP server. Indexes a codebase once using tree-sitter AST parsing and lets AI agents retrieve exactly the symbols they need — instead of dumping entire files into context.

## Why

AI coding agents default to reading whole files. A 500-line Rust file costs ~2,000 tokens every time. With pitlane-mcp, fetching a single method costs ~120 tokens. Index once, query forever.

## Features

- **AST-based indexing** — tree-sitter parses Rust and Python source into structured symbols
- **Seven MCP tools** for navigation: outline, search, fetch, find usages
- **Incremental re-indexing** — background watcher re-parses only changed files
- **Disk-persisted index** — binary format, loads in milliseconds on subsequent calls
- **Smart exclusions** — automatically skips `.venv`, `node_modules`, `target`, `__pycache__`, and other dependency trees at any depth
- **Fully local** — no network calls, no external APIs

## Supported Languages

| Language | Symbol kinds |
|---|---|
| Rust | function, method, struct, enum, trait, impl, mod, macro, const, type alias |
| Python | function, method, class |

## Installation

Build from source (requires Rust 1.75+):

```bash
cargo build --release
cp target/release/pitlane-mcp ~/.local/bin/
```

## MCP Client Configuration

### VS Code / Kiro IDE

Add to your MCP settings (`.kiro/settings/mcp.json` or `.vscode/mcp.json`):

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

### Claude Code

```bash
claude mcp add pitlane-mcp -- pitlane-mcp
```

## Tools

### `index_project`

Parse and index all supported source files under a path.

```json
{ "path": "/your/project", "force": false }
```

Returns symbol count, file count, index path, and elapsed time. Subsequent calls return cached results unless `force: true`.

### `search_symbols`

Search by name, kind, language, or file pattern.

```json
{ "project": "/your/project", "query": "authenticate", "kind": "method" }
```

### `get_symbol`

Retrieve the full source of one symbol by its stable ID. Much cheaper than reading the whole file.

```json
{ "project": "/your/project", "symbol_id": "src/auth.rs::Auth::login#method" }
```

### `get_file_outline`

List all symbols in a file with kinds and line numbers — no source returned.

```json
{ "project": "/your/project", "file_path": "src/auth.rs" }
```

### `get_project_outline`

High-level overview of the project: files grouped by directory with symbol counts per kind.

```json
{ "project": "/your/project", "depth": 2 }
```

### `find_usages`

Find all locations that reference a symbol by name.

```json
{ "project": "/your/project", "symbol_id": "src/auth.rs::Auth::login#method" }
```

> Text-based reference search — finds name occurrences in indexed files. False positives are possible for short or common names.

### `watch_project`

Start incremental background re-indexing on file changes.

```json
{ "project": "/your/project" }
{ "project": "/your/project", "stop": true }
```

## Symbol IDs

Symbol IDs are stable string keys derived from the file path, qualified name, and kind:

```
{relative_path}::{qualified_name}#{kind}

src/audio/engine.rs::Engine::process_block#method
src/models/user.py::UserService::authenticate#method
```

IDs are returned by `search_symbols` and `get_file_outline` and used as input to `get_symbol` and `find_usages`.

## Index Storage

Indexes are stored at:

```
~/.pitlane/indexes/{project_hash}/index.bin
~/.pitlane/indexes/{project_hash}/meta.json
```

The project hash is a BLAKE3 hash of the canonical project path. The index is invalidated automatically when source files change (mtime-based). Use `force: true` to rebuild unconditionally.

## Recommended Agent Instructions

Add a `CLAUDE.md` at your project root to guide the agent:

```markdown
# Code Navigation

Use pitlane-mcp for all code lookups when available.

1. Before reading any file, call get_file_outline to see its structure.
2. Use search_symbols to find functions/types by name.
3. Use get_symbol to retrieve only the exact implementation you need.
4. Use find_usages before refactoring any public symbol.
5. Fall back to direct file reads only when editing or when full file context is required.
```

## License

[MIT License](LICENSE)
