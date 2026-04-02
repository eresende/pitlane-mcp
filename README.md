# pitlane-mcp

[![CI](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml)

Token-efficient code intelligence MCP server. Indexes a codebase once using tree-sitter AST parsing and lets AI agents retrieve exactly the symbols they need — instead of dumping entire files into context.

## Why

AI coding agents default to reading whole files. With pitlane-mcp, they fetch only the symbol they need — **532× less tokens** on a Rust codebase ([ripgrep](https://github.com/BurntSushi/ripgrep)), **418×** on C++ ([LevelDB](https://github.com/google/leveldb)), **133×** on C ([Redis](https://github.com/redis/redis)), **125×** on Go ([Gin](https://github.com/gin-gonic/gin)), **112×** on Java ([Guava](https://github.com/google/guava)), **53×** on TypeScript ([Hono](https://github.com/honojs/hono)), and **20×** on Python ([FastAPI](https://github.com/fastapi/fastapi)).

## Features

- **AST-based indexing** — tree-sitter parses Rust, Python, JavaScript, TypeScript, C, C++, Go, Java, and Bash source into structured symbols
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
| Java | `.java` | class, interface, enum, method |
| Bash | `.sh`, `.bash` | function |

TypeScript declaration files (`.d.ts`, `.d.mts`, `.d.cts`) are automatically skipped.

## Installation

Download a pre-built binary from [GitHub Releases](https://github.com/eresende/pitlane-mcp/releases/latest) for Linux (x86\_64, aarch64), macOS (x86\_64, Apple Silicon), and Windows (x86\_64).

Or install via Homebrew (macOS):

```bash
brew tap eresende/pitlane-mcp
brew install pitlane-mcp
```

Or install via [`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall) (pulls pre-built binaries, no compilation needed):

```bash
cargo binstall pitlane-mcp
```

Or install from [crates.io](https://crates.io/crates/pitlane-mcp) (requires Rust 1.75+):

```bash
cargo install pitlane-mcp
```

Or build from source:

```bash
cargo build --release
cp target/release/pitlane-mcp ~/.local/bin/
cp target/release/pitlane ~/.local/bin/
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

Optional parameters:

- `exclude` — additional glob patterns to skip during the walk (e.g. `"vendor/**"`).
- `force: true` — rebuild the index even if the on-disk copy is up to date.
- `max_files` — cap on the number of source files indexed (default: 100 000). Raise this for very large mono-repos. If the walk finds more eligible files than the cap, `index_project` returns a `FILE_LIMIT_EXCEEDED` error instead of indexing.

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

> **Python/JS/TS/Java classes and C++ classes/structs**: for classes that contain methods, `get_symbol` returns only the class header (plus docstring for Python) — not the full body. Retrieve individual methods by their own symbol IDs (e.g. `models.py::MyClass::some_method#method`). Use `get_file_outline` to list all methods first.

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

## CLI

The `pitlane` binary exposes the same code intelligence as the MCP server, useful for shell scripts, CI pipelines, or manual exploration.

### `pitlane index`

Index a project (or load from cache if up to date).

```bash
pitlane index /your/project
pitlane index /your/project --force
pitlane index /your/project --exclude "*.generated.ts" --exclude "vendor/**"
```

### `pitlane search`

Search for symbols by name with optional filters.

```bash
pitlane search /your/project authenticate
pitlane search /your/project authenticate --kind method
pitlane search /your/project authenticate --lang python
pitlane search /your/project authenticate --file "src/auth*"
pitlane search /your/project authenticate --limit 5 --offset 10
```

### `pitlane outline`

High-level directory/symbol overview of the project.

```bash
pitlane outline /your/project
pitlane outline /your/project --depth 3
```

### `pitlane file`

List all symbols in a file with kinds and line numbers.

```bash
pitlane file /your/project src/auth.rs
```

### `pitlane symbol`

Fetch the source of a single symbol by its ID.

```bash
pitlane symbol /your/project src/auth.rs::Auth::login[method]
pitlane symbol /your/project src/auth.rs::Auth::login[method] --context
pitlane symbol /your/project src/auth.rs::Auth::login[method] --sig-only
```

All commands output JSON to stdout. Logs go to stderr and can be controlled with `RUST_LOG`.

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

Benchmarks use eight pinned open-source projects as test corpora: [ripgrep 14.1.1](https://github.com/BurntSushi/ripgrep) (Rust, 98 files, 3,194 symbols), [FastAPI 0.115.6](https://github.com/fastapi/fastapi) (Python + JS docs, 1,286 files, 4,828 symbols), [Hono v4.7.4](https://github.com/honojs/hono) (TypeScript, 368 files, 992 symbols), [Redis 7.4.2](https://github.com/redis/redis) (C, 720 files, 14,591 symbols), [LevelDB 1.23](https://github.com/google/leveldb) (C++, 132 files, 1,529 symbols), [Gin v1.10.0](https://github.com/gin-gonic/gin) (Go, 92 files, 1,184 symbols), [Guava v33.4.8](https://github.com/google/guava) (Java, 3,269 files, 56,804 symbols), and [bats-core v1.11.1](https://github.com/bats-core/bats-core) (Bash, 54 files, 147 symbols).

> **Note:** pitlane-mcp is under active development. New language support and token-efficiency optimizations land frequently, so these numbers are updated with each release and may change significantly between versions.

**Test environment:** AMD Ryzen 9 9950X (16 cores / 32 threads), 32 GB RAM, NVMe SSD.

### Results

| Metric | ripgrep | FastAPI | Hono | Redis | LevelDB | Gin | Guava | bats-core |
|---|---|---|---|---|---|---|---|---|
| Indexing time (min / median, 5 runs) | 26 ms / 28 ms | 32 ms / 34 ms | 17 ms / 18 ms | 103 ms / 104 ms | 10 ms / 12 ms | 11 ms / 11 ms | 239 ms / 246 ms | 2 ms / 2 ms |
| Peak RAM (first-run) | 40.1 MB | 37.8 MB | 31.3 MB | 94.1 MB | 24.1 MB | 21.9 MB | 201.9 MB | 10.3 MB |
| Index size on disk | 1.1 MB | 1.6 MB | 275 KB | 3.9 MB | 398 KB | 354 KB | 28.4 MB | 52.5 KB |
| Token efficiency — median | **532×** | **20×** | **53×** | **133×** | **418×** | **125×** | **112×** | N/A¹ |
| Token efficiency — worst case | 8.9× (`LowArgs`, 2.9 KB in 26 KB) | 3.2× (`Schema`, 4.8 KB in 15.4 KB) | 1.4× (`RequestHeader`, 4.9 KB in 6.9 KB) | 5.1× (`redisServer`, 37.6 KB in 190 KB) | 34.4× (`TestWritableFile`, 0.5 KB in 15.9 KB) | 6.5× (`Engine`, 3.7 KB in 23.8 KB) | 1.2× (`Network`, 18.6 KB in 22.9 KB) | N/A¹ |
| `search_symbols` latency | 144 µs | 30 µs | 36 µs | 673 µs | 61 µs | 47 µs | 65 µs | 7.9 µs |
| `get_symbol` latency | 3.4 µs | 3.8 µs | 3.8 µs | 10.8 µs | 2.6 µs | 3.5 µs | 6.8 µs | 4.2 µs |
| `get_file_outline` latency | 96 µs | 52 µs | 5.0 µs | 1.06 ms | 72 µs | 59 µs | 33 µs | 16.0 µs |
| `get_project_outline` latency | 362 µs | 1.93 ms | 273 µs | 2.24 ms | 227 µs | 150 µs | 24.5 ms | 43.3 µs |
| `find_usages` latency | 19.7 ms | 29.6 ms | 11.7 ms | 29.7 ms | 2.0 ms | 139 µs | 3.3 ms | 485 µs |

Token efficiency is the ratio of full-file size to symbol size — how many times cheaper fetching a symbol is versus reading the whole file. Measured across all struct/class/interface/type-alias symbols; median is the typical case. ¹ Bash has no struct/class symbols — only functions — so the metric does not apply.

> Query latencies are median wall-clock times for a single tool call against a warm in-memory index (no disk I/O, no re-indexing). Measured with Criterion over 100–1,000+ samples depending on the operation.
>
> Redis's high `search_symbols` and `get_file_outline` latencies reflect its 14,591 symbols (4× more than any other corpus) and the `src/server.h` benchmark file being a 190 KB header dense with declarations. LevelDB's 418× median reflects C++ class body trimming: inline method bodies are stripped from the indexed symbol, leaving only the class header. FastAPI's worst-case symbol is `Schema`, a large Pydantic model; the Python median of 20× is representative of normal usage. `find_usages` latency for Hono, Redis, and LevelDB reflects full AST search across all their TypeScript, C, and C++ source files respectively. Gin's sub-millisecond `find_usages` reflects its compact 92-file codebase. Guava's worst case of 1.2× is the `Network` interface — a 18.6 KB file of pure abstract method signatures that cannot be trimmed (interface bodies are never trimmed since their signatures are the API contract); the 112× median across all classes is representative of normal usage. Guava's high `get_project_outline` latency (24.5 ms) and large index size (28.4 MB) reflect its 56,804 symbols — the largest corpus by a factor of 4×.

### Running the benchmarks

Clone the test corpora first (one-time setup):

```bash
bash bench/setup.sh
```

**Memory, disk, and token efficiency** (single binary, human-readable output):

```bash
# All repos
cargo run --release --features memory-bench --bin memory_bench

# One or more repos by name
cargo run --release --features memory-bench --bin memory_bench -- bats
cargo run --release --features memory-bench --bin memory_bench -- ripgrep fastapi
```

**Query latency** (Criterion, saves baseline for regression tracking):

```bash
# All repos
cargo bench --bench queries

# One repo (Criterion's built-in filter)
cargo bench --bench queries -- bats
cargo bench --bench queries -- "ripgrep|gin"
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

### Resource cap on directory walks

`index_project` enforces a configurable `max_files` cap (default: 100,000 source files). If the walk finds more eligible files than the cap, it returns a `FILE_LIMIT_EXCEEDED` error instead of indexing. This prevents accidental full-filesystem walks (e.g. `index_project("/")`). Raise `max_files` explicitly for very large mono-repos.

### Index storage

Indexes are stored unencrypted at `~/.pitlane/indexes/{blake3_hash}/`. If another local user or process can write to your home directory they could tamper with index files; however, deserialization failures are handled gracefully and will not execute arbitrary code.

## License

Licensed under either of [MIT License](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.
