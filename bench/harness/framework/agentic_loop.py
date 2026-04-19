"""Agentic loop implementation for the benchmark framework.

Drives a tool-calling loop between a ModelBackend and a ToolExecutor,
recording all tool calls and terminating on completion, max iterations,
or wall-clock timeout.
"""

from __future__ import annotations

import logging
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

log = logging.getLogger("bench")

_SYSTEM_PROMPT = (
    "You are a code analysis assistant running inside a benchmark harness. "
    "Your goal is to answer the user's question with the fewest useful tool calls, "
    "using concrete evidence from the repository.\n\n"
    "Working rules:\n"
    "- Use tools only when they materially improve the answer.\n"
    "- Do not repeat the same broad search or startup call unless the prior result clearly failed.\n"
    "- Do not re-index or wait for embeddings repeatedly.\n"
    "- Once you have enough evidence, stop calling tools and answer directly.\n"
    "- Prefer a small number of high-signal reads over many broad searches.\n"
    "- If a tool result is weak or empty, pivot to a different tool instead of retrying the same call.\n"
    "- When answering, name the most relevant files and symbols and explain how they relate.\n"
    "- If the repository does not contain the requested feature, say so clearly and briefly explain the evidence."
)

_MCP_SYSTEM_PROMPT_SUFFIX = (
    "Pitlane MCP guidance:\n"
    "- Prefer the default pitlane tool tier over broad search.\n"
    "- Start with locate_code for ambiguous discovery, then use one or two focused read_code_unit calls.\n"
    "- Use trace_path for behavior, source-to-sink, and execution-path questions.\n"
    "- Use search_content only when you know a text fragment but not the owning symbol.\n"
    "- Do not use generic read_file/read/read-style tools for repo analysis until pitlane has already identified the exact file or symbol to inspect.\n"
    "- Do not use generic list_directory/glob/search-style tools for repo discovery while pitlane discovery tools are available.\n"
    "- Do not use bash/shell tools for code lookup while pitlane discovery tools are available.\n"
    "- Generic file tools are escape hatches for exact known paths, not the default analysis workflow.\n"
    "- If a pitlane discovery result is weak, reformulate the pitlane query once before escaping to generic file tools.\n"
    "- Do not spend turns repeatedly calling startup tools such as ensure_project_ready, "
    "index_project, or wait_for_embeddings unless the tool output explicitly requires it.\n"
    "- If locate_code or trace_path returns a plausible target, "
    "read that target and then answer instead of branching further.\n"
    "- If you already have 2 to 4 concrete files or symbols relevant to the question, answer."
)

_MCP_PROMPT_CLASS_GUIDANCE = {
    "arch_": (
        "Architecture prompt guidance:\n"
        "- Start with get_index_stats once.\n"
        "- Use locate_code to find the central files after orientation.\n"
        "- After orientation, do at most two or three focused follow-up reads.\n"
        "- Do not read many crate root files just to build a package map.\n"
        "- Prefer read_code_unit over generic file reads when you only need roles and boundaries.\n"
        "- Do not switch to generic read or glob just because get_index_stats gave you the repo shape."
    ),
    "symbol_": (
        "Implementation lookup guidance:\n"
        "- Use one focused discovery call, then read the strongest two to four targets and answer.\n"
        "- Do not enumerate many sibling symbols in the same crate unless the previous read explicitly redirects you.\n"
        "- If the question names a concrete subsystem such as ignore handling or CLI config, stay inside that subsystem.\n"
        "- If locate_code is weak, issue one sharper subsystem query such as the concrete type, method, or file role before switching tools.\n"
        "- Do not replace locate_code or read_code_unit with repeated generic reads once the subsystem is known."
    ),
    "usage_": (
        "Call-path guidance:\n"
        "- Prefer trace_path first.\n"
        "- Once you have the main path, read only the key nodes in that path and stop.\n"
        "- Do not branch into unrelated searches after a plausible call chain is found."
    ),
    "tests_": (
        "Test-discovery guidance:\n"
        "- Find the strongest tests first, then read only the most relevant production files they exercise.\n"
        "- Do not map the whole subsystem when the task is to identify representative tests and edge cases."
    ),
    "negative_": (
        "Negative-check guidance:\n"
        "- Use one or two targeted searches to verify absence.\n"
        "- If the named feature or type is not present, answer clearly instead of continuing to explore nearby code."
    ),
}

_MCP_EXACT_PROMPT_GUIDANCE = {
    "smart_exclusions_probe": (
        "Enumeration guidance:\n"
        "- When the question asks for several named mechanisms, identify the mechanism list first and then verify one concrete file or symbol for each mechanism.\n"
        "- Do not retry the same concept with multiple broad searches after you already have a credible implementing file.\n"
        "- Do not fan out into generic glob or raw file reads unless pitlane cannot identify the owner for a named mechanism."
    ),
    "symbol_cli_config_flow": (
        "CLI flow guidance:\n"
        "- Do not search for generic terms like main function, entry point, or args without the CLI/config subsystem named in the query.\n"
        "- Prefer locate_code queries that name the concrete subsystem, such as flags parser, HiArgs, LowArgs, ParseResult, or search worker.\n"
        "- If locate_code is weak, use one narrower pitlane query before using any generic read, glob, or shell tool."
    ),
    "symbol_regex_search_path": (
        "Execution-path guidance:\n"
        "- Prefer trace_path plus focused read_code_unit calls over manual directory exploration.\n"
        "- Do not use generic read or glob to map searcher or printer directories when locate_code can target the relevant symbol directly.\n"
        "- Once you have entry, orchestration, matcher/searcher, and printer nodes, answer."
    ),
    "symbol_ignore_logic": (
        "Ignore-subsystem guidance:\n"
        "- Stay inside the ignore subsystem once locate_code identifies crates/ignore files.\n"
        "- Prefer read_code_unit line slices or symbol reads over broad file reads of dir.rs, walk.rs, or gitignore.rs.\n"
        "- Do not use shell listing or globbing once the main ignore files are known."
    ),
    "semantic_search_probe": (
        "Semantic discovery guidance:\n"
        "- Use one semantic-style discovery step to find the owning subsystem, then switch to targeted reads.\n"
        "- Do not keep broad-searching once binary handling or the closest real subsystem is identified."
    ),
    "graph_nav_call_chain": (
        "Graph navigation guidance:\n"
        "- Favor trace_execution_path or trace_path over manual multi-search exploration.\n"
        "- Once the key call chain is found, read only the main entry, orchestration, execution, and output nodes."
    ),
    "token_efficiency_probe": (
        "Token-efficiency guidance:\n"
        "- Count and group files from focused evidence; do not read extra files once each role in the pipeline has a representative source file."
    ),
    "fully_local_probe": (
        "Locality guidance:\n"
        "- Use a narrow absence check for networking primitives, then answer.\n"
        "- Do not drift into unrelated architecture mapping when the question is only about network behavior."
    ),
}


def _mcp_prompt_guidance_for(prompt_id: str) -> str:
    """Return extra MCP prompt guidance for the benchmark prompt class."""
    sections: list[str] = []
    for prefix, guidance in _MCP_PROMPT_CLASS_GUIDANCE.items():
        if prompt_id.startswith(prefix):
            sections.append(guidance)
            break
    exact = _MCP_EXACT_PROMPT_GUIDANCE.get(prompt_id)
    if exact:
        sections.append(exact)
    return "\n\n".join(sections)


def _build_system_prompt(mode: str, repo_path: str, prompt_id: str) -> str:
    """Build the benchmark system prompt, including MCP-specific guidance."""
    system_prompt = _SYSTEM_PROMPT
    if repo_path and mode == "mcp":
        extra_guidance = _mcp_prompt_guidance_for(prompt_id)
        suffix = _MCP_SYSTEM_PROMPT_SUFFIX
        if extra_guidance:
            suffix = f"{suffix}\n\n{extra_guidance}"
        system_prompt += (
            "\n\n"
            f"{suffix}\n\n"
            f"The repository is indexed at project path: {repo_path!r}. "
            "Always use this exact string as the value for the 'project' parameter in all tool calls."
        )
    return system_prompt


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
        repo_path: str = "",
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
            repo_path: Path to the repository being analyzed (injected into
                system prompt for MCP mode so model knows the project value).

        Returns:
            RunResult with full conversation, tool call log, final answer,
            and termination reason.
        """
        wall_start = time.perf_counter()
        _bytes_before = executor.total_response_bytes()

        system_prompt = _build_system_prompt(mode, repo_path, prompt_id)

        conversation: list[Message] = [
            Message(role="system", content=system_prompt),
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
                    log.warning("    [iter %d] timeout before LLM call (%.1fs)", iteration, elapsed)
                    status = "timeout"
                    break

                log.debug("    [iter %d] calling LLM...", iteration)
                t_llm = time.perf_counter()
                response = backend.chat(conversation, tools)
                log.debug("    [iter %d] LLM responded in %.1fs  tokens=%d",
                          iteration, time.perf_counter() - t_llm, response.usage.total_tokens)

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
                    final_answer = assistant_msg.content.strip()
                    if final_answer:
                        status = "completed"
                        log.debug("    [iter %d] final answer received", iteration)
                        break

                    if response.finish_reason == "length":
                        status = "max_iterations"
                        log.warning(
                            "    [iter %d] empty assistant content with finish_reason=length",
                            iteration,
                        )
                        break

                    final_answer = response.reasoning_content or ""
                    if final_answer.strip():
                        final_answer = final_answer.strip()
                        status = "completed"
                        log.debug(
                            "    [iter %d] completed from reasoning_content fallback",
                            iteration,
                        )
                    else:
                        log.warning(
                            "    [iter %d] empty assistant response without tool calls",
                            iteration,
                        )
                    break

                # Execute all tool calls (possibly parallel) in this response
                for tc in assistant_msg.tool_calls:
                    log.info("    [iter %d] tool: %s  args=%s",
                             iteration, tc.name,
                             str(tc.arguments)[:120])
                    tc_start = time.perf_counter()
                    result = executor.execute(tc.name, tc.arguments)
                    tc_latency_ms = (time.perf_counter() - tc_start) * 1000.0
                    log.debug("    [iter %d] tool %s → %d bytes in %.0fms",
                              iteration, tc.name, result.byte_size, tc_latency_ms)

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
                    log.warning("    [iter %d] timeout after tool calls (%.1fs)", iteration, elapsed)
                    status = "timeout"
                    break

        except Exception as exc:  # noqa: BLE001
            log.error("    agentic loop error: %s", exc)
            status = "error"
            error = str(exc)

        wall_clock_seconds = time.perf_counter() - wall_start
        total_context_bytes = executor.total_response_bytes() - _bytes_before

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
