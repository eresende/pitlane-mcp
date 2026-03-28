# pitlane-mcp

[![CI](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml)

Token-efficient code intelligence MCP server. Indexes a codebase once using tree-sitter AST parsing and lets AI agents retrieve exactly the symbols they need — instead of dumping entire files into context.

## Why

AI coding agents default to reading whole files. With pitlane-mcp, they fetch only the symbol they need — **540× less tokens** on a Rust codebase ([ripgrep](https://github.com/BurntSushi/ripgrep)), **19× less** on a Python one ([FastAPI](https://github.com/fastapi/fastapi)). Both indexed in under 60 ms.

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

Retrieve the source of one symbol by its stable ID. Much cheaper than reading the whole file.

```json
{ "project": "/your/project", "symbol_id": "src/auth.rs::Auth::login#method" }
```

> **Python classes**: for classes that contain methods, `get_symbol` returns only the class header and docstring — not the full body. Retrieve individual methods by their own symbol IDs (e.g. `models.py::MyClass::some_method#method`). Use `get_file_outline` to list all methods first.

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

## Benchmarks

Benchmarks use two pinned open-source projects as test corpora: [ripgrep 14.1.1](https://github.com/BurntSushi/ripgrep) (Rust, 98 files, 3,194 symbols) and [FastAPI 0.115.6](https://github.com/fastapi/fastapi) (Python, 1,283 files, 4,807 symbols).

### Results

| Metric | ripgrep | FastAPI |
|---|---|---|
| Indexing time (min / median, 5 runs) | 36 ms / 40 ms | 58 ms / 59 ms |
| Peak RAM (first-run) | 39 MB | 37 MB |
| Index size on disk | 1.1 MB | 1.6 MB |
| Token efficiency — median | **540×** | **19×** |
| Token efficiency — worst case | 8.9× (`LowArgs`, 2.9 KB in a 26 KB file) | 3.2× (`Schema`, 4.8 KB in a 15 KB file) |
| `search_symbols` latency | 147 µs | 40 µs |
| `get_symbol` latency | 8.8 µs | 9.4 µs |
| `get_file_outline` latency | 89 µs | 53 µs |
| `find_usages` latency | 26 ms | 42 ms |

Token efficiency is the ratio of full-file size to symbol size — how many times cheaper fetching a symbol is versus reading the whole file. Measured across all struct/class symbols; median is the typical case.

### Running the benchmarks

Clone the test corpora first (one-time setup):

```bash
bash bench/setup.sh
```

**Memory, disk, and token efficiency** (single binary, human-readable output):

```bash
cargo run --release --bin memory_bench -- bench/repos/ripgrep
cargo run --release --bin memory_bench -- bench/repos/fastapi
```

**Query latency** (Criterion, saves baseline for regression tracking):

```bash
cargo bench --bench queries
```

**Indexing throughput** (Criterion):

```bash
cargo bench --bench indexing
```

## License

[MIT License](LICENSE)
