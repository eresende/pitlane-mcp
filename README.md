# pitlane-mcp

[![CI](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml)

Token-efficient code intelligence MCP server. Indexes a codebase once using tree-sitter AST parsing and lets AI agents retrieve exactly the symbols they need — instead of dumping entire files into context.

## Why

AI coding agents default to reading whole files. With pitlane-mcp, they fetch only the symbol they need — **540× less tokens** on a Rust codebase ([ripgrep](https://github.com/BurntSushi/ripgrep)), **19× less** on a Python one ([FastAPI](https://github.com/fastapi/fastapi)). Both indexed in under 60 ms.

## Features

- **AST-based indexing** — tree-sitter parses Rust, Python, JavaScript, and TypeScript source into structured symbols
- **Seven MCP tools** for navigation: outline, search, fetch, find usages
- **Incremental re-indexing** — background watcher re-parses only changed files
- **Disk-persisted index** — binary format, loads in milliseconds on subsequent calls
- **Smart exclusions** — automatically skips `.venv`, `node_modules`, `target`, `__pycache__`, `dist`, `.next`, and other dependency/build trees at any depth
- **Fully local** — no network calls, no external APIs

## Supported Languages

| Language | Extensions | Symbol kinds |
|---|---|---|
| Rust | `.rs` | function, method, struct, enum, trait, impl, mod, macro, const, type alias |
| Python | `.py` | function, method, class |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | function, class, method |
| TypeScript | `.ts`, `.tsx`, `.mts`, `.cts` | function, class, method, interface, type alias, enum |

TypeScript declaration files (`.d.ts`, `.d.mts`, `.d.cts`) are automatically skipped.

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

Optional parameters:

- `signature_only: true` — returns only the indexed metadata (signature, doc comment, file, line range) with no file I/O. Use this when you only need to know what a symbol looks like, not its full body.
- `include_context: true` — includes 3 lines of surrounding source before and after the symbol.

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

> AST-based reference search — only true identifier nodes are matched. String literals, comments, and substrings of longer identifiers are never returned.

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
src/api/client.ts::fetchUser#function
src/components/Button.tsx::Button#function
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

Benchmarks use three pinned open-source projects as test corpora: [ripgrep 14.1.1](https://github.com/BurntSushi/ripgrep) (Rust, 98 files, 3,194 symbols), [FastAPI 0.115.6](https://github.com/fastapi/fastapi) (Python + JS docs, 1,286 files, 4,828 symbols), and [Hono v4.7.4](https://github.com/honojs/hono) (TypeScript, 368 files, 992 symbols).

### Results

| Metric | ripgrep | FastAPI | Hono |
|---|---|---|---|
| Indexing time (min / median, 5 runs) | 30 ms / 42 ms | 58 ms / 66 ms | 35 ms / 39 ms |
| Peak RAM (first-run) | 41 MB | 36 MB | 31 MB |
| Index size on disk | 1.1 MB | 1.6 MB | 275 KB |
| Token efficiency — median | **532×** | **19×** | **42×** |
| Token efficiency — worst case | 8.9× (`LowArgs`, 2.9 KB in a 26 KB file) | 1.1× (`Termynal`, 9 KB in a 9.5 KB file) | 1.6× (`Context`, 15 KB in a 24 KB file) |
| `search_symbols` latency | 151 µs | 283 µs | 46 µs |
| `get_symbol` latency | 9.0 µs | 11.5 µs | 14 µs |
| `get_file_outline` latency | 89 µs | 19 µs | 41 µs |
| `get_project_outline` latency | 535 µs | 2.9 ms | 523 µs |
| `find_usages` latency | 26 ms | 14.5 ms | 1.7 ms |

Token efficiency is the ratio of full-file size to symbol size — how many times cheaper fetching a symbol is versus reading the whole file. Measured across all struct/class/interface/type-alias symbols; median is the typical case.

> FastAPI's worst-case symbol is now `Termynal`, a JavaScript class in FastAPI's docs (`termynal.js`) — a dense single-class file where symbol and file are nearly the same size. The Python median of 19× is representative of normal usage.

### Running the benchmarks

Clone the test corpora first (one-time setup):

```bash
bash bench/setup.sh
```

**Memory, disk, and token efficiency** (single binary, human-readable output):

```bash
cargo run --release --bin memory_bench -- bench/repos/ripgrep
cargo run --release --bin memory_bench -- bench/repos/fastapi
cargo run --release --bin memory_bench -- bench/repos/hono
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
