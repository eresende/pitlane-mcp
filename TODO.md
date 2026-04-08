# Roadmap

## In Progress: `Architecture review follow-ups`

## COMPLETED!

### Optimizations

_These items improve token efficiency, usability, and observability of the MCP tools._

- [x] **BM25 camelCase tokenizer** — custom tantivy tokenizer splits at camelCase/digit boundaries (`LowerInstruction` → `["lower", "instruction"]`); improves BM25 quality on C++, Java, Swift, Kotlin, and C# codebases
- [x] **`get_symbol` signature-only default for classes** — when fetching a class/struct symbol, return signature + docstring only by default (no method bodies); agents almost always want the shape, not the implementation _(High value, low effort)_
- [x] **`search_symbols` fuzzy matching** — add Levenshtein or trigram matching as a fallback when substring search returns no results; helps agents recover from slightly wrong names _(High value, low effort)_
- [x] **Symbol cross-references in `get_symbol` response** — include a `references` list of symbols directly used by the fetched symbol (calls, type references); saves a separate `find_usages` round-trip _(High value, low effort)_
- [x] **`get_symbol` by line range** — add a `get_lines` tool that fetches a file slice by line range for blocks that aren't named symbols _(Medium effort, meaningful impact)_
- [x] **`get_index_stats` tool** — lightweight tool returning symbol counts by language and kind; lets agents orient in a new codebase without burning tokens on `get_project_outline` _(Medium effort, meaningful impact)_
- [x] **`watch_project` status query** — add a way to check whether a watcher is already running; prevents duplicate watchers _(Medium effort, meaningful impact)_

### MCP host integration

_These items optimize how MCP host clients (Claude Code, OpenCode, Kiro, Cursor, etc.) discover, execute, and display pitlane-mcp tools._

- [x] **Tool annotations** — set `readOnlyHint`, `destructiveHint`, `openWorldHint` on all tools; enables concurrent execution and removes unnecessary permission prompts _(High priority, small effort)_
- [x] **Tool description rewrites** — front-load "what and when" into the first sentence of each description; respect ~2048 char truncation limits _(Medium priority, small effort)_
- [x] **Result size / pagination** — add `offset` to `search_symbols` and `limit`/`offset` to `find_usages`; add soft caps with actionable truncation messages _(Medium priority, medium effort)_
- [x] **Language filter bugfix** — `search_symbols` docstring only lists Rust/Python but indexer supports 8 languages; update docstring and verify filter logic _(Medium priority, small effort)_
- [x] **Structured error formatting** — return machine-readable error codes with recovery hints (e.g. `PROJECT_NOT_INDEXED` → "Call index_project first") _(Medium priority, small effort)_
- [x] **`_meta` extensions** — set `alwaysLoad` and `searchHint` fields for tool discovery; unverified vendor extensions, speculative but harmless _(Medium priority, small effort)_
- [x] **Server instructions rewrite** — tighten the server instruction string; lead with "index first", group related tools _(Low priority, trivial effort)_
- [x] **Progress reporting for `index_project`** — emit progress notifications, but only for large projects (>500 files) _(Low priority, medium effort)_

### Correctness & robustness

- [x] **Resource cap on directory walks** — add a configurable max-file-count guard in `index_project` to prevent accidental or adversarial full-filesystem walks (e.g. `index_project("/")`)
- [x] **`find_usages` early-exit file walk** — short-circuit the AST walk once `offset + limit` usages are collected; currently walks all files even when the page is already full, which wastes work on large codebases _(Medium priority, small effort)_
- [x] **`find_usages` scope glob for all languages** — the `scope` parameter currently works but is only exercised by Rust/Python tests; validate and test it for JS/TS/C/C++

### Architecture review follow-ups

- [ ] **Fix stale-cache validation for newly added files** — `is_index_up_to_date` currently only compares mtimes for files already present in `meta.file_mtimes`; adding a new supported source file can still return a cached index that is missing symbols
- [x] **Harden watcher event handling under bursty changes** — `watch_project` now marks the project dirty when the debounce channel overflows and falls back to a full resync, so incremental indexing cannot silently diverge after dropped notify events
- [x] **Make exact-search pagination deterministic** — `search_symbols` exact-mode now sorts candidates by a stable key before paginating, so repeated calls return the same pages regardless of `HashMap` iteration order or insertion history
- [ ] **Rework `find_usages` to use the index more effectively** — it currently re-walks the filesystem and reparses files on every call; consider an indexed candidate-selection path so the tool matches the rest of the architecture on large repos
- [x] **Fix Svelte inline `<script>` column remapping in `find_usages`** — embedded script-block results now preserve the original file column offset, so usages reported from inline `<script>` tags point to the correct columns in the `.svelte` source

### Language support

- [x] **PHP** — `tree-sitter-php` exists on crates.io; massive web ecosystem (WordPress, Laravel, Symfony)
- [x] **Lua / Roblox Lua** — `tree-sitter-luau` exists on crates.io; covers modern Roblox `.luau` and `.lua` codebases
- [x] **Zig** — `tree-sitter-zig` exists on crates.io; growing systems-programming language with increasing adoption
- [x] **Kotlin** — `tree-sitter-kotlin-ng` (v1.1.0); primary language for Android development and popular on the JVM
- [x] **Bash** — `tree-sitter-bash` exists on crates.io; useful for indexing shell scripts, dotfiles, and DevOps repos
- [x] **Java** — `tree-sitter-java` exists on crates.io; high-value target with large existing corpus of open-source Java projects
- [x] **Go** — `tree-sitter-go` exists on crates.io; high-value target given Go's prevalence in backend codebases
- [x] **C#** — `tree-sitter-c-sharp` exists on crates.io; common in enterprise and game dev (Unity)
- [x] **Ruby** — `tree-sitter-ruby` exists on crates.io; common in Rails codebases
- [x] **Swift** — `tree-sitter-swift` exists on crates.io (v0.7.1); needs compatibility check against the current tree-sitter 0.26 dependency before adding
- [x] **Objective-C** — `tree-sitter-objc` exists on crates.io; less actively maintained, lower priority

### Token efficiency

- [x] **JS/TS class body trimming** — apply the same "header + docstring only" treatment already done for Python classes to TypeScript/JavaScript classes; Hono (42×) and similar TS projects would benefit most
- [x] **C++ class body trimming** — classes/structs with inline methods trimmed to header line; LevelDB median improved from 34× to 418×

### Distribution

- [x] **Publish to crates.io** — project is stable enough at v0.3.x; publish `pitlane-mcp` to the registry
- [x] **Binary releases via GitHub Actions** — build Linux (x86\_64, aarch64), macOS (x86\_64, Apple Silicon), and Windows (x86\_64) binaries on tag push, attach to GitHub releases
- [x] **Homebrew formula** — makes installation trivial for macOS users: `brew install pitlane-mcp`
- [x] **`cargo-binstall` manifest** — allows `cargo binstall pitlane-mcp` to pull pre-built binaries instead of compiling from source

## Done

- [x] AST-based indexing for Rust, Python, JavaScript, TypeScript, C, C++
- [x] Seven MCP tools: `index_project`, `search_symbols`, `get_symbol`, `get_file_outline`, `get_project_outline`, `find_usages`, `watch_project`
- [x] In-memory index cache (94–99% query speedup)
- [x] Rayon parallelism in indexer (3–5× indexing speedup)
- [x] `find_usages` AST-based search (replaces text search; ignores string literals and comments)
- [x] Python class body trimming (fastapi token efficiency: 13× → 19×)
- [x] `.gitignore` respect during indexing
- [x] `get_project_outline` compact JSON format (~40% latency improvement)
- [x] `find_usages` and `watch_project` extension filters fixed to cover all indexed languages (was silently skipping JS/TS/C/C++ files)
- [x] Security documentation
- [x] Benchmark suite (Criterion + memory\_bench) across five open-source corpora
