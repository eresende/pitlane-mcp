# pitlane-mcp

[![CI](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml)

Token-efficient code intelligence MCP server. Indexes a codebase once using tree-sitter AST parsing and lets AI agents retrieve exactly the symbols they need — instead of dumping entire files into context.

## Why

AI coding agents default to reading whole files. With pitlane-mcp, they fetch only the symbol they need — **532× less tokens** on a Rust codebase ([ripgrep](https://github.com/BurntSushi/ripgrep)), **133×** on C ([Redis](https://github.com/redis/redis)), **125×** on Go ([Gin](https://github.com/gin-gonic/gin)), **53×** on TypeScript ([Hono](https://github.com/honojs/hono)), **34×** on C++ ([LevelDB](https://github.com/google/leveldb)), and **19×** on Python ([FastAPI](https://github.com/fastapi/fastapi)).

## Features

- **AST-based indexing** — tree-sitter parses Rust, Python, JavaScript, TypeScript, C, C++, and Go source into structured symbols
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
| C | `.c`, `.h` | function, struct, enum, type alias, macro |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx` | function, method, class, struct, enum, type alias, macro |
| Go | `.go` | function, method, struct, interface, type alias |

TypeScript declaration files (`.d.ts`, `.d.mts`, `.d.cts`) are automatically skipped.

## Installation

Build from source (requires Rust 1.75+):

```bash
cargo build --release
cp target/release/pitlane-mcp ~/.local/bin/
```

## MCP Client Configuration

### Claude Code

```bash
claude mcp add pitlane-mcp -- pitlane-mcp
```

### OpenCode

Add to your `opencode.json` or `opencode.jsonc`:

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

> **Python/JS/TS classes**: for classes that contain methods, `get_symbol` returns only the class header (plus docstring for Python) — not the full body. Retrieve individual methods by their own symbol IDs (e.g. `models.py::MyClass::some_method#method`). Use `get_file_outline` to list all methods first.

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

1. Call index_project at the start of each session to load the index.
2. Call watch_project right after indexing to keep the index up to date as files change.
3. Before reading any file, call get_file_outline to see its structure.
4. Use search_symbols to find functions/types by name.
5. Use get_symbol to retrieve only the exact implementation you need.
6. Use find_usages before refactoring any public symbol.
7. Fall back to direct file reads only when editing or when full file context is required.
```

## Benchmarks

Benchmarks use six pinned open-source projects as test corpora: [ripgrep 14.1.1](https://github.com/BurntSushi/ripgrep) (Rust, 98 files, 3,194 symbols), [FastAPI 0.115.6](https://github.com/fastapi/fastapi) (Python + JS docs, 1,286 files, 4,828 symbols), [Hono v4.7.4](https://github.com/honojs/hono) (TypeScript, 368 files, 992 symbols), [Redis 7.4.2](https://github.com/redis/redis) (C, 720 files, 14,591 symbols), [LevelDB 1.23](https://github.com/google/leveldb) (C++, 132 files, 1,529 symbols), and [Gin v1.10.0](https://github.com/gin-gonic/gin) (Go, 92 files, 1,184 symbols).

> **Note:** pitlane-mcp is under active development. New language support and token-efficiency optimizations land frequently, so these numbers are updated with each release and may change significantly between versions.

### Results

| Metric | ripgrep | FastAPI | Hono | Redis | LevelDB | Gin |
|---|---|---|---|---|---|---|
| Indexing time (min / median, 5 runs) | 34 ms / 34 ms | 51 ms / 52 ms | 35 ms / 36 ms | 135 ms / 139 ms | 19 ms / 20 ms | 13 ms / 16 ms |
| Peak RAM (first-run) | 38.6 MB | 34.9 MB | 30.9 MB | 91.7 MB | 22.1 MB | 20.3 MB |
| Index size on disk | 1.1 MB | 1.6 MB | 275 KB | 3.9 MB | 397 KB | 354 KB |
| Token efficiency — median | **532×** | **19×** | **53×** | **133×** | **34×** | **125×** |
| Token efficiency — worst case | 8.9× (`LowArgs`, 2.9 KB in 26 KB) | 1.1× (`Termynal`, 9 KB in 9.5 KB) | 1.4× (`RequestHeader`, 4.7 KB in 6.7 KB) | 5.1× (`redisServer`, 37.6 KB in 190 KB) | 1.7× (`Benchmark`, 19.8 KB in 33.3 KB) | 6.5× (`Engine`, 3.6 KB in 23.8 KB) |
| `search_symbols` latency | 164 µs | 302 µs | 43 µs | 918 µs | 49 µs | 61.6 µs |
| `get_symbol` latency | 9.0 µs | 11.5 µs | 13.9 µs | 23.7 µs | 15.9 µs | 10.1 µs |
| `get_file_outline` latency | 78 µs | 17.5 µs | 37 µs | 583 µs | 74 µs | 57.3 µs |
| `get_project_outline` latency | 318 µs | 1.67 ms | 278 µs | 1.91 ms | 240 µs | 155 µs |
| `find_usages` latency | 25.6 ms | 16.0 ms | 104.9 ms | 37.5 ms | 18.0 ms | 0.45 ms |

Token efficiency is the ratio of full-file size to symbol size — how many times cheaper fetching a symbol is versus reading the whole file. Measured across all struct/class/interface/type-alias symbols; median is the typical case.

> Redis's high `search_symbols` and `get_file_outline` latencies reflect its 14,591 symbols (4× more than any other corpus) and the `src/server.h` benchmark file being a 190 KB header dense with declarations. FastAPI's worst-case symbol is `Termynal`, a JavaScript class in FastAPI's docs where symbol and file are nearly the same size; the Python median of 19× is representative of normal usage. `find_usages` latency for Hono, Redis, and LevelDB reflects full AST search across all their TypeScript, C, and C++ source files respectively. Gin's sub-millisecond `find_usages` reflects its compact 92-file codebase.

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
cargo run --release --bin memory_bench -- bench/repos/redis
cargo run --release --bin memory_bench -- bench/repos/leveldb
cargo run --release --bin memory_bench -- bench/repos/gin
```

**Query latency** (Criterion, saves baseline for regression tracking):

```bash
cargo bench --bench queries
```

**Indexing throughput** (Criterion):

```bash
cargo bench --bench indexing
```

## Security

pitlane-mcp is a fully local tool with no network calls. The following design properties are intentional but worth understanding before deploying it with AI agents.

### Filesystem access scope

`index_project`, `find_usages`, and `watch_project` accept any path accessible to the running process — there is no allowlist or project-root confinement. An AI agent (or a prompt-injected instruction) can call these tools with sensitive directories such as `~/.ssh`, `~/.config`, or `/etc`.

Mitigating factors:
- Only files with recognized source extensions are indexed or read (`.rs`, `.py`, `.js`, `.ts`, `.tsx`, `.c`, `.cpp`, `.h`, `.hpp`, `.go`, etc.). Most sensitive files — SSH keys, certificates, `.env` files — have no matching extension and are silently skipped.
- Symbolic links are never followed (`follow_links: false` in all directory walks).
- Files larger than 1 MiB are skipped.

**Recommendation:** If you deploy pitlane-mcp with an AI agent in an environment where prompt injection is a concern, treat it as having read access to any source file the OS user can read.

### No resource cap on directory walks

There is no limit on the number of files indexed or total memory consumed during a walk. Calling `index_project` on a very large tree (e.g. `/`) will attempt to walk the entire filesystem and may cause high CPU/memory usage until it completes or the process is killed. Only invoke `index_project` on bounded project directories.

### Index storage

Indexes are stored unencrypted at `~/.pitlane/indexes/{blake3_hash}/`. If another local user or process can write to your home directory they could tamper with index files; however, deserialization failures are handled gracefully and will not execute arbitrary code.

## License

Licensed under either of [MIT License](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.
