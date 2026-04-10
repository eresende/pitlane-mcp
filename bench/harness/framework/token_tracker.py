"""Token and context byte tracking for benchmark runs."""

from __future__ import annotations

from dataclasses import dataclass, field

from bench.harness.framework.models import TokenUsage


@dataclass
class ToolCallEntry:
    """Record of a single tool call's resource usage."""

    tool_name: str
    response_bytes: int
    latency_ms: float


class TokenTracker:
    """Accumulates token usage and tool-call metrics across a benchmark run."""

    def __init__(self) -> None:
        self._prompt_tokens: int = 0
        self._completion_tokens: int = 0
        self._total_tokens: int = 0
        self._tool_entries: list[ToolCallEntry] = []

    # ------------------------------------------------------------------
    # Recording
    # ------------------------------------------------------------------

    def record_llm_call(self, usage: TokenUsage) -> None:
        """Accumulate token counts from a single LLM call."""
        self._prompt_tokens += usage.prompt_tokens
        self._completion_tokens += usage.completion_tokens
        self._total_tokens += usage.total_tokens

    def record_tool_call(
        self, tool_name: str, response_bytes: int, latency_ms: float
    ) -> None:
        """Record a single tool call's response size and latency."""
        self._tool_entries.append(
            ToolCallEntry(
                tool_name=tool_name,
                response_bytes=response_bytes,
                latency_ms=latency_ms,
            )
        )

    # ------------------------------------------------------------------
    # Aggregated properties
    # ------------------------------------------------------------------

    @property
    def total_context_bytes(self) -> int:
        """Sum of all tool response bytes accumulated so far."""
        return sum(e.response_bytes for e in self._tool_entries)

    @property
    def total_token_usage(self) -> TokenUsage:
        """Accumulated token counts across all recorded LLM calls."""
        return TokenUsage(
            prompt_tokens=self._prompt_tokens,
            completion_tokens=self._completion_tokens,
            total_tokens=self._total_tokens,
        )

    @property
    def tool_call_details(self) -> list[ToolCallEntry]:
        """Per-tool-call records in the order they were recorded."""
        return list(self._tool_entries)

    # ------------------------------------------------------------------
    # Efficiency ratio
    # ------------------------------------------------------------------

    @staticmethod
    def compute_efficiency_ratio(baseline_bytes: int, mcp_bytes: int) -> float:
        """Return baseline_bytes / mcp_bytes.

        Raises ZeroDivisionError if mcp_bytes is zero.
        """
        return baseline_bytes / mcp_bytes
