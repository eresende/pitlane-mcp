"""Agentic loop implementation for the benchmark framework.

Drives a tool-calling loop between a ModelBackend and a ToolExecutor,
recording all tool calls and terminating on completion, max iterations,
or wall-clock timeout.
"""

from __future__ import annotations

import time
from typing import TYPE_CHECKING

from bench.harness.framework.models import (
    Message,
    RunResult,
    TokenUsage,
    ToolCallRecord,
)

if TYPE_CHECKING:
    from bench.harness.framework.backends import ModelBackend
    from bench.harness.framework.executors import ToolExecutor

_SYSTEM_PROMPT = (
    "You are a helpful code analysis assistant. Use the available tools to "
    "answer questions about the codebase. Be thorough and precise."
)


class AgenticLoop:
    """Drives an agentic tool-calling loop between a backend and an executor."""

    def run(
        self,
        prompt: str,
        backend: "ModelBackend",
        executor: "ToolExecutor",
        max_iterations: int = 25,
        timeout_seconds: float = 300.0,
        *,
        prompt_id: str = "",
        mode: str = "baseline",
        run_index: int = 0,
    ) -> RunResult:
        """Run the agentic loop and return a RunResult.

        Args:
            prompt: The user prompt to answer.
            backend: LLM backend to call for chat completions.
            executor: Tool executor that provides and runs tools.
            max_iterations: Maximum number of backend.chat() calls before
                terminating with status "max_iterations".
            timeout_seconds: Wall-clock timeout in seconds before terminating
                with status "timeout".
            prompt_id: Identifier for the prompt (for RunResult).
            mode: "mcp" or "baseline" (for RunResult).
            run_index: Run index within a benchmark set (for RunResult).

        Returns:
            RunResult with full conversation, tool call log, final answer,
            and termination reason.
        """
        wall_start = time.perf_counter()

        conversation: list[Message] = [
            Message(role="system", content=_SYSTEM_PROMPT),
            Message(role="user", content=prompt),
        ]
        tool_call_records: list[ToolCallRecord] = []
        accumulated_usage = TokenUsage(
            prompt_tokens=0, completion_tokens=0, total_tokens=0
        )
        tools = executor.get_tool_definitions()

        status = "max_iterations"
        final_answer = ""
        error: str | None = None

        try:
            for iteration in range(1, max_iterations + 1):
                # Check wall-clock timeout before each backend call
                elapsed = time.perf_counter() - wall_start
                if elapsed >= timeout_seconds:
                    status = "timeout"
                    break

                response = backend.chat(conversation, tools)

                # Accumulate token usage
                accumulated_usage = TokenUsage(
                    prompt_tokens=accumulated_usage.prompt_tokens
                    + response.usage.prompt_tokens,
                    completion_tokens=accumulated_usage.completion_tokens
                    + response.usage.completion_tokens,
                    total_tokens=accumulated_usage.total_tokens
                    + response.usage.total_tokens,
                )

                assistant_msg = response.message
                conversation.append(assistant_msg)

                # No tool calls → final answer
                if not assistant_msg.tool_calls:
                    final_answer = assistant_msg.content
                    status = "completed"
                    break

                # Execute all tool calls (possibly parallel) in this response
                for tc in assistant_msg.tool_calls:
                    tc_start = time.perf_counter()
                    result = executor.execute(tc.name, tc.arguments)
                    tc_latency_ms = (time.perf_counter() - tc_start) * 1000.0

                    tool_call_records.append(
                        ToolCallRecord(
                            iteration=iteration,
                            tool_name=tc.name,
                            arguments=tc.arguments,
                            result_bytes=result.byte_size,
                            latency_ms=tc_latency_ms,
                        )
                    )

                    conversation.append(
                        Message(
                            role="tool",
                            content=result.content,
                            tool_call_id=tc.id,
                        )
                    )

                # Check timeout after executing tools
                elapsed = time.perf_counter() - wall_start
                if elapsed >= timeout_seconds:
                    status = "timeout"
                    break

        except Exception as exc:  # noqa: BLE001
            status = "error"
            error = str(exc)

        wall_clock_seconds = time.perf_counter() - wall_start
        total_context_bytes = executor.total_response_bytes()

        return RunResult(
            prompt_id=prompt_id,
            mode=mode,
            run_index=run_index,
            status=status,
            final_answer=final_answer,
            conversation=conversation,
            tool_calls=tool_call_records,
            token_usage=accumulated_usage,
            total_context_bytes=total_context_bytes,
            wall_clock_seconds=wall_clock_seconds,
            error=error,
        )
