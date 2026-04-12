# pitlane-mcp

[![CI](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eresende/pitlane-mcp/actions/workflows/ci.yml)

Token-efficient code intelligence MCP server. Indexes a codebase once using tree-sitter AST parsing and lets AI agents retrieve exactly the symbols they need — instead of dumping entire files into context.

## Why

AI coding agents default to reading whole files. With pitlane-mcp, they fetch only the symbol they need — **532× less tokens** on a Rust codebase ([ripgrep](https://github.com/BurntSushi/ripgrep)), **418×** on C++ ([LevelDB](https://github.com/google/leveldb)), **133×** on C ([Redis](https://github.com/redis/redis)), **125×** on Go ([Gin](https://github.com/gin-gonic/gin)), **112×** on Java ([Guava](https://github.com/google/guava)), **65×** on C# ([Newtonsoft.Json](https://github.com/JamesNK/Newtonsoft.Json)), **61×** on Ruby ([RuboCop](https://github.com/rubocop/rubocop)), **56×** on Kotlin ([OkHttp](https://github.com/square/okhttp)), **54×** on Objective-C ([SDWebImage](https://github.com/SDWebImage/SDWebImage)), **53×** on TypeScript ([Hono](https://github.com/honojs/hono)), **52×** on Swift ([SwiftLint](https://github.com/realm/SwiftLint)), **49×** on Solidity ([OpenZeppelin Contracts](https://github.com/OpenZeppelin/openzeppelin-contracts)), **41×** on Svelte ([svelte.dev](https://github.com/sveltejs/svelte.dev)), **20×** on Python ([FastAPI](https://github.com/fastapi/fastapi)), **80×** on PHP ([Laravel](https://github.com/laravel/framework)), **801×** on Zig ([zls](https://github.com/zigtools/zls)), **90×** on Lua ([Roact](https://github.com/Roblox/roact)), and Bash ([bats-core](https://github.com/bats-core/bats-core))¹.

Recent `bench/harness` runs on the ripgrep prompt set also show a consistent quality win over a non-MCP baseline. In matched 19-prompt runs, MCP improved average quality from `0.115` to `0.326` on a local AMD Radeon RX 6800 XT / Ryzen 9 9950X system and from `0.143` to `0.277` on an AWS NVIDIA A10G / AMD EPYC 7R32 instance. See [bench/harness/RIPGREP_BENCHMARK_2026-04-12.md](bench/harness/RIPGREP_BENCHMARK_2026-04-12.md) for the full comparison, system specs, and caveats.

## Features

- **AST-based indexing** — tree-sitter parses Rust, Python, JavaScript, TypeScript, Svelte (embedded `<script>` / `<script lang="ts">` blocks only), C, C++, Go, Java, C#, Ruby, Swift, Objective-C, PHP, Zig, Kotlin, Lua, Solidity, and Bash source into structured symbols
- **BM25 full-text search** — tantivy-backed ranked search over name, qualified name, signature, and doc fields with a custom camelCase tokenizer (`LowerInstruction` → `["lower", "instruction"]`); falls back to exact substring match if the index isn't ready
- **Graph-aware navigation tools** — direct callers and callees for shallow impact checks without whole-repo back-and-forth
- **Seventeen MCP tools** spanning startup, discovery, retrieval, and impact analysis
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
| Svelte | `.svelte` | function, class, method, interface, type alias, enum (from embedded `<script>` / `<script lang=\"ts\">` blocks only; template/style sections are not indexed) |
| C | `.c`, `.h` | function, struct, enum, type alias, macro |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx` | function, method, class, struct, enum, type alias, macro |
| Go | `.go` | function, method, struct, interface, type alias |
| Java | `.java` | class, interface, enum, method |
| C# | `.cs` | class, struct, interface, enum, method, type alias |
| Bash | `.sh`, `.bash` | function |
| Ruby | `.rb` | class, module, method |
| Swift | `.swift` | class, struct, enum, protocol, method, function, init, type alias |
| Objective-C | `.m`, `.mm` | class, protocol, method, function, type alias |
| PHP | `.php` | class, interface, enum, method, function |
| Zig | `.zig` | function, method, struct, enum, const |
| Lua | `.luau`, `.lua` | function, method, type alias |
| Kotlin | `.kt`, `.kts` | class, interface, enum, object, function, method, type alias |
| Solidity | `.sol` | contract, interface, library, function, method, modifier, constructor, event, error, struct, enum |

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

### Tool Hierarchy

Use the tools by intent, not by implementation detail:

- **Startup**
  - `ensure_project_ready` — preferred one-call startup; indexes the repo and waits for embeddings only when needed
  - `index_project` + `wait_for_embeddings` — lower-level primitives when you need explicit control
- **Discovery**
  - `search_symbols` — find symbols by name or intent
  - `search_content` — find literal text, regex fragments, import paths, log strings, or macros when you do not know the symbol boundary
  - `search_files` — find repository files by file name, path fragment, or glob pattern
  - `trace_execution_path` — answer behavior/path questions with a compact set of files, symbols, and edges
- **Retrieval**
  - `get_symbol` — fetch one implementation by symbol ID
  - `get_file_outline` — inspect a file's symbol structure before choosing what to read
  - `get_lines` — fetch a specific non-symbol block by line range
- **Orientation**
  - `get_index_stats` — cheap aggregate repo overview
  - `get_project_outline` — directory/file tree overview when structure matters
- **Graph / Impact**
  - `find_callees` — direct outgoing references
  - `find_callers` — direct incoming references
  - `find_usages` — all call sites / name-based usages
- **Maintenance**
  - `watch_project` — keep the index current while a repo is changing
  - `get_usage_stats` — inspect token-efficiency stats for `get_symbol`

Recommended flow:

1. Call `ensure_project_ready`.
2. Choose one discovery tool:
   - `search_symbols` for names or intent
   - `search_content` for text snippets
   - `search_files` for file-name or path discovery
   - `trace_execution_path` for behavior and execution-path questions
3. Switch to `get_symbol` once you have the right symbol.
4. Use `find_callees`, `find_callers`, or `find_usages` only when you need graph or impact information.

Avoid shell `grep`, globbing, or broad file reads when the MCP tools can answer the question directly.

### `ensure_project_ready`

Preferred one-call startup for MCP clients and harnesses.

```json
{ "path": "/your/project" }
```

This ensures the index exists and waits for embeddings only if they are still running. In the common case it replaces a manual `index_project` + `wait_for_embeddings` sequence.

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

If the response reports `"embeddings": "running"`, call `wait_for_embeddings` before using semantic search. For normal startup, prefer `ensure_project_ready`.

### `search_symbols`

Search by name, kind, language, or file pattern.

```json
{ "project": "/your/project", "query": "authenticate", "kind": "method" }
```

Defaults to BM25 ranked full-text search (via tantivy) over name, qualified name, signature, and doc fields — results are ordered by relevance. Pass `"mode": "exact"` for substring matching or `"mode": "fuzzy"` for trigram similarity. If the BM25 index isn't ready yet (e.g. first call after an upgrade), it falls back to exact automatically.

Use `"mode": "semantic"` for behavior or intent queries when embeddings are enabled and you do not know the exact symbol name.

### `search_content`

Search literal text or regex patterns across supported source files.

```json
{ "project": "/your/project", "query": "RegexMatcherBuilder::new", "file": "crates/**/*.rs" }
```

Use this when you know text in the code but do not know the symbol boundary yet. Supports optional regex mode, case sensitivity, file/language filters, and surrounding context lines. Prefer this over shell `grep`.

### `search_files`

Search repository paths by file name, path fragment, fuzzy similarity, or glob.

```json
{ "project": "/your/project", "query": "ImmutableListTest", "mode": "substring" }
```

Useful when you know or expect a file name, test file, path suffix, or directory pattern but do not yet know the exact symbol or file contents. Prefer this over shell globbing or `find`.

Optional parameters:

- `mode` — `substring` (default), `exact`, `fuzzy`, or `glob`
- `language` — restrict matches by file extension family
- `file` — restrict the search to a subtree or path set via glob

### `trace_execution_path`

Trace a likely execution path for a behavior-level question in one step.

```json
{ "project": "/your/project", "query": "main regex search execution path" }
```

Use this for questions like "where is X implemented?", "how does Y flow?", or "what is the main execution path?" The response returns a compact set of important files, symbols, edges, and a ready-to-explain path summary so agents do not need to assemble the whole graph manually.

### `get_symbol`

Retrieve the source of one symbol by its stable ID. Much cheaper than reading the whole file.

```json
{ "project": "/your/project", "symbol_id": "src/auth.rs::Auth::login#method" }
```

Optional parameters:

- `signature_only` — returns only the indexed metadata (signature, doc comment, file, line range) with no file I/O. Defaults to `true` for struct, class, interface, and trait kinds; defaults to `false` for functions, methods, and everything else. Pass `signature_only: false` explicitly to get the full body of a container type.
- `include_context: true` — includes 3 lines of surrounding source before and after the symbol.

Full-source responses include a `references` field — a list of symbols whose names appear as identifiers in the source. This saves a separate `find_usages` call when you want to understand what a symbol depends on.

> **Python/JS/TS/Java classes, C++ classes/structs, C# classes/structs, Ruby classes/modules, and Swift classes/structs**: for classes that contain methods, `get_symbol` returns only the class header (plus docstring for Python) — not the full body. Objective-C `@interface` blocks are returned at full extent (they contain only declarations, not implementations). Retrieve individual methods by their own symbol IDs (e.g. `models.py::MyClass::some_method#method`). Use `get_file_outline` to list all methods first.

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

Optional parameters:

- `summary: true` — return only directory names with file and symbol counts, no per-file items or kind breakdowns. Use for very large codebases (>10k files) where the full outline exceeds token limits. Agents should retry with this flag if the normal response is too large.
- `path` — scope the outline to a subtree (e.g. `"kernel/sched"`). Combine with `depth` to drill into a specific area of a large repo.
- `max_dirs` — cap the number of directory entries returned (default: 50, max: 500). When the result is truncated, the response includes a `hint` suggesting `path` or `summary: true`.

### `find_usages`

Find all locations that reference a symbol by name.

```json
{ "project": "/your/project", "symbol_id": "src/auth.rs::Auth::login#method" }
```

> AST-based reference search — only true identifier nodes are matched. String literals, comments, and substrings of longer identifiers are never returned.

> **Svelte note:** reference search only covers identifiers inside embedded `<script>` / `<script lang="ts">` blocks. Template and style sections are intentionally ignored.

### `find_callees`

Return direct outgoing references for one symbol — useful for seeing what a function or method likely calls before reading more code.

```json
{ "project": "/your/project", "symbol_id": "src/auth.rs::Auth::login#method" }
```

Optional parameters:

- `limit` — maximum callees to return (default: 100).
- `offset` — offset into callees for pagination.

This is intentionally shallow and lightweight. Results are heuristic direct references, not a full semantic call graph.

### `find_callers`

Return direct incoming references for one symbol — useful for quick impact checks before changing a function or method.

```json
{ "project": "/your/project", "symbol_id": "src/auth.rs::Auth::login#method" }
```

Optional parameters:

- `scope` — restrict callers to a file or directory glob.
- `limit` — maximum callers to return (default: 100).
- `offset` — offset into callers for pagination.

Like `find_callees`, this stays shallow by design and returns heuristic direct callers, not a full transitive call graph.

### `get_lines`

Fetch a slice of a file by line range — useful for blocks that are not named symbols (macro invocation tables, initializer arrays, inline comment blocks, etc.).

```json
{ "project": "/your/project", "file_path": "fs/read_write.c", "line_start": 668, "line_end": 700 }
```

Returns `source`, `total_file_lines`, and the actual `line_end` after clamping. Capped at 500 lines per call; when the cap is hit the response includes `truncated: true` and a `truncated_note` with the next `line_start` to continue.

### `get_index_stats`

Return symbol counts by language and kind for an indexed project — lightweight orientation tool. Use instead of `get_project_outline` when you only need aggregate numbers, not the file tree.

```json
{ "project": "/your/project" }
```

Returns `total_files`, `total_symbols`, `by_language`, and `by_kind`, all sorted by count descending.

### `get_usage_stats`

Return token-efficiency statistics for `get_symbol` calls — how many tokens were saved by signature-only responses, persisted across sessions.

```json
{ "project": "/your/project" }
```

Returns global totals and a per-project breakdown with `get_symbol_calls`, `signature_only_applied`, `full_source_bytes`, `returned_bytes`, and `tokens_saved_approx`. Stats are stored at `~/.pitlane/stats.json`.

### `watch_project`

Start incremental background re-indexing on file changes.

```json
{ "project": "/your/project" }
{ "project": "/your/project", "stop": true }
{ "project": "/your/project", "status_only": true }
```

Pass `status_only: true` to check whether a watcher is already running without starting or stopping it — returns `"status": "watching"` or `"status": "not_watching"`.

### `wait_for_embeddings`

Block until background embedding generation is complete.

```json
{ "project": "/your/project" }
```

Use this only after a direct `index_project` call reports `"embeddings": "running"`. For normal startup, prefer `ensure_project_ready`.

## CLI

The `pitlane` binary exposes the same code intelligence as the MCP server, useful for shell scripts, CI pipelines, or manual exploration.

### `pitlane index`

Index a project (or load from cache if up to date).

```bash
pitlane index /your/project
pitlane index /your/project --force
pitlane index /your/project --exclude "*.generated.ts" --exclude "vendor/**"
pitlane index /your/project --max-files 200000
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
pitlane outline /your/project --path src/auth --max-dirs 100
pitlane outline /your/project --summary
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

### `pitlane callees`

Show direct outgoing references for a symbol.

```bash
pitlane callees /your/project src/auth.rs::Auth::login[method]
pitlane callees /your/project src/auth.rs::Auth::login[method] --limit 20 --offset 20
```

### `pitlane callers`

Show direct incoming references for a symbol.

```bash
pitlane callers /your/project src/auth.rs::Auth::login[method]
pitlane callers /your/project src/auth.rs::Auth::login[method] --scope "src/**" --limit 20
```

### `pitlane usages`

Find all call sites for a symbol.

```bash
pitlane usages /your/project src/auth.rs::Auth::login[method]
pitlane usages /your/project src/auth.rs::Auth::login[method] --scope "src/**" --limit 20
```

### `pitlane lines`

Fetch a specific line range from a file.

```bash
pitlane lines /your/project src/auth.rs 40 80
```

### `pitlane wait-embeddings`

Block until background embedding generation has finished.

```bash
pitlane wait-embeddings /your/project
pitlane wait-embeddings /your/project --poll-interval-ms 1000 --timeout-secs 120
```

### `pitlane watch`

Keep the project index updated until interrupted.

```bash
pitlane watch /your/project
```

Unlike the MCP server, the CLI watcher only lives for the lifetime of that `pitlane watch` process. Stop it with `Ctrl-C`.

### `pitlane usage-stats`

Show token-efficiency statistics for `get_symbol`.

```bash
pitlane usage-stats
pitlane usage-stats /your/project
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

IDs are returned by `search_symbols` and `get_file_outline` and used as input to `get_symbol`, `find_callees`, `find_callers`, and `find_usages`.

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

1. Prefer ensure_project_ready at the start of each session. If you use index_project directly and it reports embeddings="running", call wait_for_embeddings immediately.
2. Call watch_project only when you expect the repo to change during the session.
3. Use search_symbols for symbol names or intent.
4. Use search_content when you know a text snippet but not the symbol boundary.
5. Use search_files when you know a file name, test file, path suffix, or directory pattern.
6. Use trace_execution_path for behavior or execution-path questions like "where is X implemented?".
7. Use get_symbol to retrieve the exact implementation you need instead of reading whole files.
8. Use get_file_outline when you know the file but not the symbol, or need file structure before choosing symbols.
9. Use find_callees, find_callers, and find_usages for graph or impact analysis.
10. For struct/class/interface/trait symbols, get_symbol returns signature-only by default. Pass signature_only=false to get the full body and references.
11. Use get_lines only for non-symbol code blocks.
12. Use get_index_stats to orient yourself before reaching for get_project_outline.
13. Fall back to direct file reads only when editing or when full-file context is genuinely required.
```

## Benchmarks

Each language is benchmarked against a pinned open-source project chosen for real-world representativeness. New corpora are added as language support grows.

> **Note:** pitlane-mcp is under active development. New language support and token-efficiency optimizations land frequently, so these numbers are updated with each release and may change significantly between versions.

**Test environment:** AMD Ryzen 9 9950X (16 cores / 32 threads), 32 GB RAM, NVMe SSD.

### Results

| Corpus | Language | Files | Symbols | Index time¹ | Token eff.² | `search_symbols` | `get_symbol` |
|---|---|---|---|---|---|---|---|
| [ripgrep 14.1.1](https://github.com/BurntSushi/ripgrep) | Rust | 101 | 3,207 | 248 ms | **532×** | 55.8 µs | 17.5 µs |
| [FastAPI 0.115.6](https://github.com/fastapi/fastapi) | Python | 1,290 | 4,828 | 256 ms | **20×** | 54.8 µs | 17.5 µs |
| [Hono v4.7.4](https://github.com/honojs/hono) | TypeScript | 368 | 992 | 240 ms | **53×** | 25.0 µs | 60.5 µs |
| [svelte.dev @ 44823b4](https://github.com/sveltejs/svelte.dev) | Svelte | 841 | 685 | 240 ms | **41×** | 26.9 µs | 17.5 µs |
| [Redis 7.4.2](https://github.com/redis/redis) | C | 818 | 14,648 | 344 ms | **133×** | 35.7 µs | 17.5 µs |
| [LevelDB 1.23](https://github.com/google/leveldb) | C++ | 132 | 1,531 | 231 ms | **418×** | 38.5 µs | 17.5 µs |
| [Gin v1.10.0](https://github.com/gin-gonic/gin) | Go | 92 | 1,184 | 235 ms | **125×** | 50.5 µs | 17.5 µs |
| [Guava v33.4.8](https://github.com/google/guava) | Java | 3,275 | 56,805 | 975 ms | **112×** | 88.5 µs | 17.5 µs |
| [Newtonsoft.Json 13.0.3](https://github.com/JamesNK/Newtonsoft.Json) | C# | 933 | 7,284 | 312 ms | **65×** | 22.2 µs | 17.5 µs |
| [bats-core v1.11.1](https://github.com/bats-core/bats-core) | Bash | 54 | 147 | 222 ms | N/A³ | 21.3 µs | 30.7 µs |
| [RuboCop v1.65.0](https://github.com/rubocop/rubocop) | Ruby | 1,539 | 9,122 | 290 ms | **61×** | 56.1 µs | 17.5 µs |
| [SwiftLint 0.57.0](https://github.com/realm/SwiftLint) | Swift | 667 | 3,781 | 248 ms | **52×** | 36.6 µs | 17.5 µs |
| [SDWebImage 5.19.0](https://github.com/SDWebImage/SDWebImage) | Objective-C | 271 | 1,564 | 237 ms | **54×** | 20.8 µs | 17.5 µs |
| [Laravel v11.9.2](https://github.com/laravel/framework) | PHP | 2,331 | 26,127 | 612 ms | **80×** | 66.9 µs | 17.6 µs |
| [OpenZeppelin Contracts v5.6.1](https://github.com/OpenZeppelin/openzeppelin-contracts) | Solidity | 661 | 4,073 | 23.4 ms | **49×** | 80.2 µs | 23.0 µs |
| [zls 0.13.0](https://github.com/zigtools/zls) | Zig | 67 | 2,422 | 240 ms | **801×** | 51.4 µs | 17.7 µs |
| [OkHttp 5.3.2](https://github.com/square/okhttp) | Kotlin | 636 | 6,680 | 278 ms | **56×** | 52.3 µs | 17.9 µs |
| [Roact v1.4.4](https://github.com/Roblox/roact) | Lua | 95 | 93 | 223 ms | **90×** | 21.3 µs | 22.7 µs |

¹ Median of 5 runs. ² Token efficiency is the median ratio of full-file size to symbol size across all class/struct/interface/type-alias symbols — how many times cheaper `get_symbol` is versus reading the whole file. ³ Bash has no class/struct symbols, only functions, so the metric does not apply.

> `search_symbols` latencies use the default BM25 mode (tantivy ranked full-text). Measured with Criterion over 100 samples per corpus. BM25 query time remains largely independent of corpus size — 21–89 µs across all 18 repos — because tantivy's inverted index avoids a linear symbol scan. The exact substring path now ranges from faster than BM25 on the tiniest corpora to 61× slower on Guava, because deterministic pagination sorts the full match set before slicing. Fuzzy (trigram) ranges from 4× to 792× slower and remains an explicit opt-in.
>
> LevelDB's 418× median reflects C++ class body trimming — inline method bodies are stripped, leaving only the class header. FastAPI's 20× median is lower than most because Pydantic models are large by nature (`Schema` alone is 4.8 KB). svelte.dev's 41× median reflects meaningful symbol extraction from embedded `<script>` blocks across a large Svelte-heavy monorepo while still excluding template/style sections. Guava's 975 ms index time and 56,805 symbols make it the heaviest corpus by a factor of 4×; `get_project_outline` against it takes ~13.4 ms vs. sub-1.8 ms for every corpus except Laravel. Laravel's `get_symbol` latency of 17.6 µs reflects the benchmark target being `Enumerable` — a 36 KB interface that is nearly the entire file it lives in; interface bodies are never trimmed since their signatures are the API contract. OpenZeppelin Contracts lands at a 48.8× median because large Solidity interfaces such as `IAccessManager` are intentionally returned at full extent; unlike contracts with executable bodies, interface definitions are not trimmed. zls's 801× median reflects Zig's tendency toward large files with many small struct/enum declarations; `src/lsp.zig` alone is 347 KB and contains hundreds of compact LSP message types.

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

## Experimental Features

### Semantic Search

pitlane-mcp supports opt-in semantic search powered by locally-running embedding models via [Ollama](https://ollama.com) or [LM Studio](https://lmstudio.ai). When enabled, `search_symbols` gains a `"semantic"` mode that ranks results by meaning rather than keyword overlap — useful for finding symbols by concept when you don't know their exact names.

See [SEMANTIC_SEARCH.md](SEMANTIC_SEARCH.md) for setup instructions, model recommendations, and known limitations.

## Security

pitlane-mcp is a fully local tool with no network calls. The following design properties are intentional but worth understanding before deploying it with AI agents.

### Filesystem access scope

By default, `index_project`, `find_usages`, and `watch_project` accept any path accessible to the running process. An AI agent (or a prompt-injected instruction) can call these tools with sensitive directories such as `~/.ssh`, `~/.config`, or `/etc`.

To opt into confinement, set `PITLANE_ALLOWED_ROOTS` to a platform-native path list (`:`-separated on Unix, `;`-separated on Windows). Use fully expanded absolute paths; config values are not shell-expanded. When set, pitlane-mcp rejects project paths outside those roots, and file-level tools reject absolute paths or traversal outside the indexed project root.

Example:

```bash
export PITLANE_ALLOWED_ROOTS="/home/alice/src:/home/alice/work"
```

Mitigating factors:
- Only files with recognized source extensions are indexed or read (`.rs`, `.py`, `.js`, `.ts`, `.tsx`, `.c`, `.cpp`, `.h`, `.hpp`, `.go`, `.swift`, `.m`, `.mm`, `.php`, `.zig`, `.luau`, `.lua`, `.sol`, etc.). Most sensitive files — SSH keys, certificates, `.env` files — have no matching extension and are silently skipped.
- Symbolic links are never followed (`follow_links: false` in all directory walks).
- Files larger than 1 MiB are skipped.

**Recommendation:** If you deploy pitlane-mcp with an AI agent in an environment where prompt injection is a concern, treat it as having read access to any source file the OS user can read.

### Resource cap on directory walks

`index_project` enforces a configurable `max_files` cap (default: 100,000 source files). If the walk finds more eligible files than the cap, it returns a `FILE_LIMIT_EXCEEDED` error instead of indexing. This prevents accidental full-filesystem walks (e.g. `index_project("/")`). Raise `max_files` explicitly for very large mono-repos.

### Index storage

Indexes are stored unencrypted at `~/.pitlane/indexes/{blake3_hash}/`. If another local user or process can write to your home directory they could tamper with index files; however, deserialization failures are handled gracefully and will not execute arbitrary code.

## License

Licensed under either of [MIT License](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.
