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
- Starts a background watcher by default so edits update the index incrementally; pass `"watch": false` to opt out. The response reports the watcher status in `watching`.

### `investigate`

Answer a broad code question in one call by discovering relevant symbols and returning source inline.

```json
{ "project": "/your/project", "query": "How does ignore/gitignore handling work?" }
```

Use this first for broad code questions such as subsystem, behavior, and execution-path questions.

Optional parameters:

- `language`
- `scope`
- `token_budget` — approximate token budget for the inlined source payload (default ~6000, chars/4 estimate). Symbols are inlined in discovery order; when the budget runs out the next symbol is truncated to what still fits (never below 15 lines) and the rest become metadata-only navigation targets: they stay in `symbols` with their ID, file, and line range (`presentation: "metadata_only"`, `reason: "token budget exhausted"`) and also appear in `omitted_symbols`. Fetch them with `read_code_unit` — no re-discovery needed. `limits.estimated_tokens_used` reports the estimate.
- `include_tests` — include related test symbols even when the query does not mention tests (default: tests are only pulled in for test-oriented queries)

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

### `analyze_changes`

Map a Git diff to changed symbols, likely affected graph neighbors, and test candidates.

```json
{ "project": "/your/project", "base_ref": "main", "include_working_tree": true }
```

CLI equivalent:

```bash
pitlane analyze-changes /your/project --base-ref main --include-working-tree
```

- `base_ref` is required and resolves to a commit. Comparison is directly against that commit, **not** its merge base with HEAD. Pass a merge-base SHA if that is what you need.
- `include_working_tree` defaults to `false`: compare base with HEAD, ignoring staged/unstaged changes. When `true`, compare base with files on disk, including non-ignored untracked files. Staged content is included only as reflected on disk; this is not a staged-only diff.
- Optional `depth` defaults to 2 (maximum 3); `limit` defaults to 8 (maximum 12 impacted symbols/files **per revision**).
- No prior indexing or embeddings are required. Separate in-memory revision indexes reuse the existing weighted impact traversal without modifying Git state or the cached index.

The response includes:

- `base_revision`, `head_revision`, and `target_revision` (`working_tree` when applicable).
- `changed_files` with zero-context `hunks`. Ranges use Git's one-based `start` and `count`; zero counts represent boundaries, not changed lines. Unmapped file changes remain visible, including `unmapped_hunk_indices` for partially mapped files.
- `changed_symbols` with revision, line ranges, change classification, and supporting `hunk_indices` into that file's hunks. A modified symbol can appear for both revisions; symbol IDs must be interpreted together with `revision`.
- `base_impact` and `target_impact` containing ranked `impact_symbols`, `impact_files`, support edges, and `test_candidates`. Base evidence preserves callers of deleted symbols. Base IDs and line ranges describe historical code, not necessarily readable current targets.
- Graph results are labeled `heuristic`; support edges distinguish `heuristic_call` from `uncertain_reference`. Test candidates use test-like paths/names and graph evidence, **not** measured test coverage. Candidates come from the bounded impact list plus directly changed tests.
- `omissions`, `effective_excludes`, and `limitations`. Impact results include total/omitted counts for the ranked lists.

Renames appear as deletion/addition. Current exclusion policy applies to both revisions. Unsupported/excluded source and file-level edits may not map to symbols. Files over 1 MiB, symlinks, and submodules are omitted; snapshots are capped at 128 MiB each (use a project subtree for larger repos). Non-UTF-8 Git paths are rejected. Avoid editing files during analysis: worktree snapshots are not atomic.

### `get_index_stats`

Return lightweight repo orientation data such as language and symbol counts.

```json
{ "project": "/your/project" }
```

Use this before broader exploration when you want orientation, not structure.

### `get_index_changes`

List symbols the index recorded as changed since a revision, newest first.

```json
{ "project": "/your/project", "since_revision": 4, "limit": 20 }
```

The index revision is a monotonically increasing counter persisted with the index; every navigation response includes the current value as `index_revision`. Revisions are assigned by every persisted content change: fresh indexing, watcher batch flushes, and full resyncs. Poll `get_index_changes` with the revision you last observed to learn exactly which symbols were added, removed, or touched without re-reading files. A revision becomes visible once the index flush lands — pending embedding work never delays it. The change log is bounded (most recent 50 revisions, per-list caps); responses carry `complete` so you know when the log was trimmed and a re-index is the safe re-baseline.

### `doctor`

Diagnose index health: freshness, effective exclusions, skipped oversized files, embedding completeness and store compatibility, index revision, and change-log state.

```json
{ "project": "/your/project", "repair": false }
```

Every check carries a `status` (`ok`/`warn`/`error`/`info`), a `detail`, and — when something is wrong — a targeted `repair` hint. Set `"repair": true` to apply the safe repairs automatically: force re-index when the index is stale, and kick off background embedding when vectors are incomplete. Repairs only touch rebuildable artifacts (index, embeddings), never user files. Parse failures are not persisted historically, so `doctor` re-derives structural diagnostics at call time.

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
