"""ClaimReport — maps benchmark results to README claims and produces a Markdown report.

README claims:
  token_efficiency, indexing_speed, bm25_search_quality, graph_navigation,
  semantic_search_quality, incremental_reindexing, smart_exclusions,
  fully_local_operation
"""

from __future__ import annotations

from collections import defaultdict
from typing import Optional

from bench.harness.framework.models import (
    BenchmarkConfig,
    ClaimSummary,
    PromptRow,
    QualityRecord,
    RunResult,
)


class ClaimReport:
    """Generates a Markdown claim-mapped report from benchmark results."""

    CLAIM_CATEGORIES: list[str] = [
        "token_efficiency",
        "indexing_speed",
        "bm25_search_quality",
        "graph_navigation",
        "semantic_search_quality",
        "incremental_reindexing",
        "smart_exclusions",
        "fully_local_operation",
    ]

    # Maps prompt category → claim name.
    # Explicit `claim` field on a PromptRow takes precedence.
    CATEGORY_TO_CLAIM: dict[str, str] = {
        "token_efficiency_probe": "token_efficiency",
        "symbol_grounding": "bm25_search_quality",
        "find_usages": "bm25_search_quality",
        "find_tests": "bm25_search_quality",
        "search_quality_probe": "bm25_search_quality",
        "architecture": "graph_navigation",
        "graph_navigation_probe": "graph_navigation",
        "semantic_search_probe": "semantic_search_quality",
    }

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def generate(
        self,
        results: list[RunResult],
        qualities: list[QualityRecord | None],
        prompts: list[PromptRow],
        config: BenchmarkConfig,
    ) -> str:
        """Return a Markdown report string."""
        # Build prompt lookup by id
        prompt_by_id: dict[str, PromptRow] = {p.id: p for p in prompts}

        # Determine claim for each (result, quality) pair
        # Group by claim → list of (result, quality)
        claim_data: dict[str, list[tuple[RunResult, QualityRecord | None]]] = defaultdict(list)

        for result, quality in zip(results, qualities):
            prompt = prompt_by_id.get(result.prompt_id)
            claim = self._resolve_claim(prompt)
            if claim is not None:
                claim_data[claim].append((result, quality))

        # Build ClaimSummary for each known claim
        summaries: list[ClaimSummary] = []
        for claim in self.CLAIM_CATEGORIES:
            entries = claim_data.get(claim, [])
            summary = self._build_summary(claim, entries)
            summaries.append(summary)

        return self._render_markdown(summaries, config)

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _resolve_claim(self, prompt: PromptRow | None) -> str | None:
        """Return the claim name for a prompt, or None if unmapped."""
        if prompt is None:
            return None
        # Explicit claim field takes precedence
        if prompt.claim is not None:
            return prompt.claim
        return self.CATEGORY_TO_CLAIM.get(prompt.category)

    def _build_summary(
        self,
        claim: str,
        entries: list[tuple[RunResult, QualityRecord | None]],
    ) -> ClaimSummary:
        # Claim support is measured from MCP runs only; baseline is used for
        # relative efficiency and latency comparisons.
        mcp_quality_by_prompt: dict[str, list[float]] = defaultdict(list)
        mcp_bytes: dict[str, list[int]] = defaultdict(list)
        baseline_bytes: dict[str, list[int]] = defaultdict(list)
        mcp_latency: dict[str, list[float]] = defaultdict(list)
        baseline_latency: dict[str, list[float]] = defaultdict(list)

        for result, quality in entries:
            if result.mode == "mcp":
                if quality is not None:
                    mcp_quality_by_prompt[result.prompt_id].append(quality.quality_score)
                mcp_bytes[result.prompt_id].append(result.total_context_bytes)
                mcp_latency[result.prompt_id].append(result.wall_clock_seconds)
            elif result.mode == "baseline":
                baseline_bytes[result.prompt_id].append(result.total_context_bytes)
                baseline_latency[result.prompt_id].append(result.wall_clock_seconds)

        prompts_tested = len(mcp_quality_by_prompt)
        per_prompt_quality = [
            sum(scores) / len(scores)
            for scores in mcp_quality_by_prompt.values()
            if scores
        ]
        avg_quality = (
            sum(per_prompt_quality) / len(per_prompt_quality)
            if per_prompt_quality
            else 0.0
        )

        ratios: list[float] = []
        for pid in set(mcp_bytes) & set(baseline_bytes):
            avg_mcp = sum(mcp_bytes[pid]) / len(mcp_bytes[pid])
            avg_base = sum(baseline_bytes[pid]) / len(baseline_bytes[pid])
            if avg_mcp > 0:
                ratios.append(avg_base / avg_mcp)

        avg_efficiency: float | None = (
            sum(ratios) / len(ratios) if ratios else None
        )

        deltas: list[float] = []
        for pid in set(mcp_latency) & set(baseline_latency):
            avg_mcp_lat = sum(mcp_latency[pid]) / len(mcp_latency[pid])
            avg_base_lat = sum(baseline_latency[pid]) / len(baseline_latency[pid])
            deltas.append(avg_base_lat - avg_mcp_lat)

        avg_latency_delta: float | None = (
            sum(deltas) / len(deltas) if deltas else None
        )

        # Verdict
        verdict = self._compute_verdict(prompts_tested, avg_quality)

        return ClaimSummary(
            claim=claim,
            prompts_tested=prompts_tested,
            avg_efficiency_ratio=avg_efficiency,
            avg_quality_score=avg_quality,
            avg_latency_delta_seconds=avg_latency_delta,
            verdict=verdict,
        )

    @staticmethod
    def _compute_verdict(prompts_tested: int, avg_quality: float) -> str:
        if prompts_tested >= 3 and avg_quality >= 0.7:
            return "validated"
        if prompts_tested >= 1:
            return "partially_supported"
        return "insufficient_data"

    @staticmethod
    def _fmt_optional_float(value: float | None, decimals: int = 2) -> str:
        if value is None:
            return "N/A"
        return f"{value:.{decimals}f}"

    def _render_markdown(
        self, summaries: list[ClaimSummary], config: BenchmarkConfig
    ) -> str:
        lines: list[str] = []

        # Header
        lines.append("# Benchmark Claim Report")
        lines.append("")
        lines.append(f"**Model:** {config.model_name} ({config.model_provider})")
        hardware_parts: list[str] = []
        if config.gpu_name:
            vram = f" {config.gpu_vram_gb:.0f}GB" if config.gpu_vram_gb else ""
            hardware_parts.append(f"GPU: {config.gpu_name}{vram}")
        if config.cpu_model:
            hardware_parts.append(f"CPU: {config.cpu_model}")
        if config.ram_gb:
            hardware_parts.append(f"RAM: {config.ram_gb:.0f}GB")
        if hardware_parts:
            lines.append(f"**Hardware:** {', '.join(hardware_parts)}")
        lines.append(f"**Timestamp:** {config.timestamp}")
        lines.append("")

        # Summary table
        lines.append(
            "| claim | prompts_tested | avg_efficiency | avg_quality"
            " | avg_latency_delta | verdict |"
        )
        lines.append(
            "|---|---|---|---|---|---|"
        )
        for s in summaries:
            lines.append(
                f"| {s.claim}"
                f" | {s.prompts_tested}"
                f" | {self._fmt_optional_float(s.avg_efficiency_ratio)}"
                f" | {self._fmt_optional_float(s.avg_quality_score)}"
                f" | {self._fmt_optional_float(s.avg_latency_delta_seconds)}"
                f" | {s.verdict} |"
            )

        lines.append("")
        return "\n".join(lines)
