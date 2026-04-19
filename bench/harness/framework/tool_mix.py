"""Tool-mix analysis for benchmark runs.

Tracks non-Pitlane tool escapes in with-mcp runs and produces derived
metrics that catch regressions even when answer quality remains flat.

Metrics produced per run:
  - total_tool_calls: total number of tool calls
  - pitlane_tool_calls: number of Pitlane MCP tool calls
  - generic_tool_calls: number of non-Pitlane tool calls (read, glob, bash, etc.)
  - pitlane_tool_pct: percent of tool calls that are Pitlane tools
  - first_generic_escape_iteration: iteration of the first non-Pitlane tool call (or None)
  - generic_tool_names: set of generic tool names used
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Sequence

from bench.harness.framework.models import RunResult, ToolCallRecord


# Tools exposed by pitlane-mcp (public tier).
# Note: opencode normalizes MCP tool names by stripping the server prefix,
# so these are the bare tool names as they appear in run artifacts.
PITLANE_TOOL_NAMES: frozenset[str] = frozenset({
    # Public tier
    "ensure_project_ready",
    "locate_code",
    "read_code_unit",
    "trace_path",
    "analyze_impact",
    "get_index_stats",
    "search_content",
    # Advanced tier (exposed with PITLANE_MCP_TOOL_TIER=all)
    "index_project",
    "wait_for_embeddings",
    "watch_project",
    "search_symbols",
    "search_files",
    "get_symbol",
    "get_file_outline",
    "get_lines",
    "get_project_outline",
    "find_callers",
    "find_callees",
    "find_usages",
    "navigate_code",
    "get_usage_stats",
    # Also match with mcp_ prefix (some runtimes keep it)
    "mcp_ensure_project_ready",
    "mcp_locate_code",
    "mcp_read_code_unit",
    "mcp_trace_path",
    "mcp_analyze_impact",
    "mcp_get_index_stats",
    "mcp_search_content",
    "mcp_index_project",
    "mcp_wait_for_embeddings",
    "mcp_watch_project",
    "mcp_search_symbols",
    "mcp_search_files",
    "mcp_get_symbol",
    "mcp_get_file_outline",
    "mcp_get_lines",
    "mcp_get_project_outline",
    "mcp_find_callers",
    "mcp_find_callees",
    "mcp_find_usages",
    "mcp_navigate_code",
    "mcp_get_usage_stats",
})

# Known generic / baseline tools that indicate an "escape" from Pitlane.
# Includes both opencode-style names and other common runtime names.
GENERIC_TOOL_NAMES: frozenset[str] = frozenset({
    "read",
    "read_file",
    "read_multiple_files",
    "list_directory",
    "glob",
    "bash",
    "grep",
    "find",
    "cat",
    "ls",
    "write",
    "edit",
})


def is_pitlane_tool(name: str) -> bool:
    """Return True if the tool name belongs to pitlane-mcp."""
    return name in PITLANE_TOOL_NAMES


def is_generic_tool(name: str) -> bool:
    """Return True if the tool name is a known generic/baseline tool."""
    return name in GENERIC_TOOL_NAMES


@dataclass
class ToolMixSummary:
    """Per-run tool-mix metrics."""

    prompt_id: str
    mode: str
    run_index: int
    total_tool_calls: int
    pitlane_tool_calls: int
    generic_tool_calls: int
    pitlane_tool_pct: float
    first_generic_escape_iteration: int | None
    generic_tool_names: list[str] = field(default_factory=list)

    def to_dict(self) -> dict:
        return asdict(self)


def analyze_tool_mix(result: RunResult) -> ToolMixSummary:
    """Compute tool-mix metrics for a single RunResult."""
    total = len(result.tool_calls)
    pitlane = 0
    generic = 0
    first_escape: int | None = None
    generic_names: set[str] = set()

    for tc in result.tool_calls:
        if is_pitlane_tool(tc.tool_name):
            pitlane += 1
        elif is_generic_tool(tc.tool_name):
            generic += 1
            generic_names.add(tc.tool_name)
            if first_escape is None:
                first_escape = tc.iteration

    pct = (pitlane / total * 100.0) if total > 0 else 0.0

    return ToolMixSummary(
        prompt_id=result.prompt_id,
        mode=result.mode,
        run_index=result.run_index,
        total_tool_calls=total,
        pitlane_tool_calls=pitlane,
        generic_tool_calls=generic,
        pitlane_tool_pct=round(pct, 1),
        first_generic_escape_iteration=first_escape,
        generic_tool_names=sorted(generic_names),
    )


def analyze_tool_mix_batch(
    results: Sequence[RunResult],
) -> list[ToolMixSummary]:
    """Compute tool-mix metrics for a batch of RunResults."""
    return [analyze_tool_mix(r) for r in results]


def format_tool_mix_report(summaries: Sequence[ToolMixSummary]) -> str:
    """Format a human-readable tool-mix report."""
    if not summaries:
        return "No tool-mix data available.\n"

    lines: list[str] = ["# Tool-Mix Analysis\n"]

    mcp_summaries = [s for s in summaries if "mcp" in s.mode and "no" not in s.mode]
    baseline_summaries = [s for s in summaries if "no" in s.mode or s.mode == "baseline"]

    if mcp_summaries:
        lines.append("## MCP Runs\n")
        avg_pct = sum(s.pitlane_tool_pct for s in mcp_summaries) / len(mcp_summaries)
        avg_generic = sum(s.generic_tool_calls for s in mcp_summaries) / len(mcp_summaries)
        escapes = [s for s in mcp_summaries if s.first_generic_escape_iteration is not None]
        lines.append(f"- Runs: {len(mcp_summaries)}")
        lines.append(f"- Avg Pitlane tool %: {avg_pct:.1f}%")
        lines.append(f"- Avg generic tool calls per run: {avg_generic:.1f}")
        lines.append(f"- Runs with generic escapes: {len(escapes)}/{len(mcp_summaries)}")
        if escapes:
            avg_first = sum(
                s.first_generic_escape_iteration for s in escapes  # type: ignore[arg-type]
            ) / len(escapes)
            lines.append(f"- Avg first escape iteration: {avg_first:.1f}")
        lines.append("")

        # Per-run detail
        lines.append("| prompt_id | pitlane% | generic | first_escape | generic_tools |")
        lines.append("|-----------|----------|---------|--------------|---------------|")
        for s in mcp_summaries:
            escape_str = str(s.first_generic_escape_iteration) if s.first_generic_escape_iteration is not None else "-"
            tools_str = ", ".join(s.generic_tool_names) if s.generic_tool_names else "-"
            lines.append(
                f"| {s.prompt_id} | {s.pitlane_tool_pct:.1f}% | {s.generic_tool_calls} | {escape_str} | {tools_str} |"
            )
        lines.append("")

    if baseline_summaries:
        lines.append("## Baseline Runs\n")
        lines.append(f"- Runs: {len(baseline_summaries)}")
        avg_total = sum(s.total_tool_calls for s in baseline_summaries) / len(baseline_summaries)
        lines.append(f"- Avg total tool calls per run: {avg_total:.1f}")
        lines.append("")

    return "\n".join(lines)
