# Roadmap

## In Progress

_(nothing currently in progress)_

## Planned

### MCP host integration

_These items optimize how MCP host clients (Claude Code, OpenCode, Kiro, Cursor, etc.) discover, execute, and display pitlane-mcp tools._

- [x] **Tool annotations** — set `readOnlyHint`, `destructiveHint`, `openWorldHint` on all tools; enables concurrent execution and removes unnecessary permission prompts _(High priority, small effort)_
- [x] **Tool description rewrites** — front-load "what and when" into the first sentence of each description; respect ~2048 char truncation limits _(Medium priority, small effort)_
- [x] **Result size / pagination** — add `offset` to `search_symbols` and `limit`/`offset` to `find_usages`; add soft caps with actionable truncation messages _(Medium priority, medium effort)_
- [x] **Language filter bugfix** — `search_symbols` docstring only lists Rust/Python but indexer supports 8 languages; update docstring and verify filter logic _(Medium priority, small effort)_
- [x] **Structured error formatting** — return machine-readable error codes with recovery hints (e.g. `PROJECT_NOT_INDEXED` → "Call index_project first") _(Medium priority, small effort)_
- [ ] **`_meta` extensions** — set `alwaysLoad` and `searchHint` fields for tool discovery; unverified vendor extensions, speculative but harmless _(Medium priority, small effort)_
- [x] **Server instructions rewrite** — tighten the server instruction string; lead with "index first", group related tools _(Low priority, trivial effort)_
- [ ] **Progress reporting for `index_project`** — emit progress notifications, but only for large projects (>500 files) _(Low priority, medium effort)_

### Correctness & robustness

- [ ] **Resource cap on directory walks** — add a configurable max-file-count guard in `index_project` to prevent accidental or adversarial full-filesystem walks (e.g. `index_project("/")`)
- [ ] **`find_usages` early-exit file walk** — short-circuit the AST walk once `offset + limit` usages are collected; currently walks all files even when the page is already full, which wastes work on large codebases _(Medium priority, small effort)_
- [ ] **`find_usages` scope glob for all languages** — the `scope` parameter currently works but is only exercised by Rust/Python tests; validate and test it for JS/TS/C/C++

### Language support

- [ ] **Bash** — `tree-sitter-bash` exists on crates.io; useful for indexing shell scripts, dotfiles, and DevOps repos
- [x] **Java** — `tree-sitter-java` exists on crates.io; high-value target with large existing corpus of open-source Java projects
- [x] **Go** — `tree-sitter-go` exists on crates.io; high-value target given Go's prevalence in backend codebases
- [ ] **C#** — `tree-sitter-c-sharp` exists on crates.io; common in enterprise and game dev (Unity)
- [ ] **Ruby** — `tree-sitter-ruby` exists on crates.io; common in Rails codebases
- [ ] **Swift** — `tree-sitter-swift` exists on crates.io (v0.7.1); needs compatibility check against the current tree-sitter 0.26 dependency before adding
- [ ] **Objective-C** — `tree-sitter-objc` exists on crates.io; less actively maintained, lower priority

### Token efficiency

- [x] **JS/TS class body trimming** — apply the same "header + docstring only" treatment already done for Python classes to TypeScript/JavaScript classes; Hono (42×) and similar TS projects would benefit most
- [x] **C++ class body trimming** — classes/structs with inline methods trimmed to header line; LevelDB median improved from 34× to 418×

### Distribution

- [ ] **Publish to crates.io** — project is stable enough at v0.3.x; publish `pitlane-mcp` to the registry
- [ ] **Binary releases via GitHub Actions** — build Linux (x86\_64, aarch64) and macOS (x86\_64, Apple Silicon) binaries on tag push, attach to GitHub releases
- [ ] **Homebrew formula** — makes installation trivial for macOS users: `brew install pitlane-mcp`
- [ ] **`cargo-binstall` manifest** — allows `cargo binstall pitlane-mcp` to pull pre-built binaries instead of compiling from source

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
