# Agent Guidance

This page provides a compact `CLAUDE.md` / `AGENTS.md` style guide for projects that use `pitlane-mcp`.

## Suggested Instructions

```markdown
# Code Navigation

Use pitlane-mcp for code lookup whenever it is available.

1. Prefer ensure_project_ready at the start of each session. It ensures the index exists and reports whether embeddings are still running, but it does not block on embeddings.
2. Use investigate first for broad code questions such as subsystem, behavior, and execution-path questions.
3. Use locate_code when you need discovery without full source.
4. Use read_code_unit once you know the target.
5. Use trace_path for explicit source-to-sink or config-to-effect questions.
6. Use analyze_impact before edits or refactors.
7. Use search_content when you know a text fragment but not the owning symbol.
8. Use get_index_stats for lightweight orientation before broader exploration.
9. Fall back to direct file reads only when editing or when full-file context is genuinely required.
10. If you explicitly exposed the advanced tool tier and use index_project directly, call wait_for_embeddings when it reports embeddings="running".
```

## Guidance Principles

- Prefer the default public tier over advanced primitives.
- Prefer `investigate` for broad questions and `read_code_unit` for precise reads.
- Avoid shell `grep`, globbing, and broad file reads when `pitlane-mcp` can answer the question directly.
- Stop searching once you have enough concrete files or symbols to explain the answer.
- Treat advanced tools as opt-in precision instruments, not the default workflow.

## Default Tool Tier

Default public tier:

- `ensure_project_ready`
- `investigate`
- `locate_code`
- `read_code_unit`
- `trace_path`
- `analyze_impact`
- `get_index_stats`
- `search_content`
