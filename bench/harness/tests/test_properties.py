"""Property-based tests for the benchmark framework.

Uses hypothesis with @settings(max_examples=100) for all properties.
"""

from __future__ import annotations

import json
import re
import tempfile
from pathlib import Path

import hypothesis.strategies as st
from hypothesis import given, settings

from bench.harness.framework.models import BenchmarkConfig, PromptRow
from bench.harness.framework.prompt_loader import (
    PromptValidationError,
    load_prompts,
)


# ---------------------------------------------------------------------------
# Strategies
# ---------------------------------------------------------------------------

# Printable text that won't break JSONL (no newlines, no bare backslashes that
# could produce invalid JSON, and non-empty).
_safe_text = st.text(
    alphabet=st.characters(
        whitelist_categories=("L", "N", "P", "S", "Z"),
        blacklist_characters="\n\r\x00\\",
    ),
    min_size=1,
    max_size=120,
)

_prompt_row_strategy = st.builds(
    PromptRow,
    id=_safe_text,
    category=_safe_text,
    prompt=_safe_text,
    prompt_suffix=st.one_of(st.none(), _safe_text),
    claim=st.one_of(st.none(), _safe_text),
)

_timestamp_strategy = st.from_regex(
    r"20[0-9]{2}-[01][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-5][0-9]Z",
    fullmatch=True,
)

_benchmark_config_strategy = st.builds(
    BenchmarkConfig,
    model_name=_safe_text,
    model_provider=_safe_text,
    backend_type=st.sampled_from(["ollama", "openrouter"]),
    repo_path=_safe_text,
    repo_commit=st.one_of(st.none(), _safe_text),
    repo_clean=st.one_of(st.none(), st.booleans()),
    harness_commit=st.one_of(st.none(), _safe_text),
    harness_clean=st.one_of(st.none(), st.booleans()),
    pitlane_version=st.one_of(st.none(), _safe_text),
    ollama_version=st.one_of(st.none(), _safe_text),
    prompt_set_path=_safe_text,
    prompt_set_sha256=st.from_regex(r"[0-9a-f]{64}", fullmatch=True),
    prompt_count=st.integers(min_value=0, max_value=1000),
    runs_per_prompt=st.integers(min_value=1, max_value=100),
    max_iterations=st.integers(min_value=1, max_value=200),
    timeout_seconds=st.floats(min_value=1.0, max_value=3600.0, allow_nan=False, allow_infinity=False),
    temperature=st.floats(min_value=0.0, max_value=2.0, allow_nan=False, allow_infinity=False),
    context_window=st.integers(min_value=512, max_value=131072),
    gpu_name=st.one_of(st.none(), _safe_text),
    gpu_vram_gb=st.one_of(
        st.none(),
        st.floats(min_value=0.0, max_value=128.0, allow_nan=False, allow_infinity=False),
    ),
    cpu_model=st.one_of(st.none(), _safe_text),
    ram_gb=st.one_of(
        st.none(),
        st.floats(min_value=0.0, max_value=2048.0, allow_nan=False, allow_infinity=False),
    ),
    timestamp=_timestamp_strategy,
)


# ---------------------------------------------------------------------------
# Property 15: JSONL prompt loading round-trip
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 15: JSONL prompt loading round-trip
# **Validates: Requirements 10.1**


@given(rows=st.lists(_prompt_row_strategy, min_size=1, max_size=20))
@settings(max_examples=100)
def test_jsonl_prompt_loading_round_trip(rows: list[PromptRow]) -> None:
    """For any valid JSONL file containing prompt rows, loading should
    produce PromptRow objects with all field values preserved exactly."""
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".jsonl", delete=False, encoding="utf-8"
    ) as f:
        for row in rows:
            obj: dict = {"id": row.id, "category": row.category, "prompt": row.prompt}
            if row.prompt_suffix is not None:
                obj["prompt_suffix"] = row.prompt_suffix
            if row.claim is not None:
                obj["claim"] = row.claim
            f.write(json.dumps(obj, ensure_ascii=False) + "\n")
        tmp_path = f.name

    try:
        loaded = load_prompts(tmp_path)
        assert len(loaded) == len(rows)
        for original, loaded_row in zip(rows, loaded):
            assert loaded_row.id == original.id
            assert loaded_row.category == original.category
            assert loaded_row.prompt == original.prompt
            assert loaded_row.prompt_suffix == original.prompt_suffix
            assert loaded_row.claim == original.claim
    finally:
        Path(tmp_path).unlink(missing_ok=True)


# ---------------------------------------------------------------------------
# Property 16: Prompt validation error reporting
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 16: Prompt validation error reporting
# **Validates: Requirements 10.5**

_required_fields = ["id", "category", "prompt"]


@given(
    valid_row=_prompt_row_strategy,
    field_to_remove=st.sampled_from(_required_fields),
    prefix_rows=st.lists(_prompt_row_strategy, min_size=0, max_size=5),
)
@settings(max_examples=100)
def test_prompt_validation_error_reporting(
    valid_row: PromptRow,
    field_to_remove: str,
    prefix_rows: list[PromptRow],
) -> None:
    """For any JSONL row missing a required field, loading should raise a
    PromptValidationError that identifies the missing field and line number."""
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".jsonl", delete=False, encoding="utf-8"
    ) as f:
        # Write valid prefix rows first
        for row in prefix_rows:
            obj = {"id": row.id, "category": row.category, "prompt": row.prompt}
            if row.prompt_suffix is not None:
                obj["prompt_suffix"] = row.prompt_suffix
            if row.claim is not None:
                obj["claim"] = row.claim
            f.write(json.dumps(obj, ensure_ascii=False) + "\n")

        # Write the bad row with one required field removed
        bad_obj: dict = {
            "id": valid_row.id,
            "category": valid_row.category,
            "prompt": valid_row.prompt,
        }
        del bad_obj[field_to_remove]
        f.write(json.dumps(bad_obj, ensure_ascii=False) + "\n")
        tmp_path = f.name

    expected_line = len(prefix_rows) + 1

    try:
        try:
            load_prompts(tmp_path)
            raise AssertionError("Expected PromptValidationError was not raised")
        except PromptValidationError as exc:
            assert exc.line_number == expected_line, (
                f"Expected line {expected_line}, got {exc.line_number}"
            )
            assert exc.field_name == field_to_remove, (
                f"Expected field '{field_to_remove}', got '{exc.field_name}'"
            )
    finally:
        Path(tmp_path).unlink(missing_ok=True)


# ---------------------------------------------------------------------------
# Property 17: Config serialization round-trip
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 17: Config serialization round-trip
# **Validates: Requirements 12.1**


@given(config=_benchmark_config_strategy)
@settings(max_examples=100)
def test_config_serialization_round_trip(config: BenchmarkConfig) -> None:
    """For any BenchmarkConfig object, serializing via to_dict() and
    deserializing via from_dict() should produce an equivalent object."""
    serialized = config.to_dict()
    restored = BenchmarkConfig.from_dict(serialized)

    assert restored.model_name == config.model_name
    assert restored.model_provider == config.model_provider
    assert restored.backend_type == config.backend_type
    assert restored.repo_path == config.repo_path
    assert restored.repo_commit == config.repo_commit
    assert restored.repo_clean == config.repo_clean
    assert restored.harness_commit == config.harness_commit
    assert restored.harness_clean == config.harness_clean
    assert restored.pitlane_version == config.pitlane_version
    assert restored.ollama_version == config.ollama_version
    assert restored.prompt_set_path == config.prompt_set_path
    assert restored.prompt_set_sha256 == config.prompt_set_sha256
    assert restored.prompt_count == config.prompt_count
    assert restored.runs_per_prompt == config.runs_per_prompt
    assert restored.max_iterations == config.max_iterations
    assert restored.timeout_seconds == config.timeout_seconds
    assert restored.temperature == config.temperature
    assert restored.context_window == config.context_window
    assert restored.gpu_name == config.gpu_name
    assert restored.gpu_vram_gb == config.gpu_vram_gb
    assert restored.cpu_model == config.cpu_model
    assert restored.ram_gb == config.ram_gb
    assert restored.timestamp == config.timestamp


# ---------------------------------------------------------------------------
# Property 1: Ollama response parsing preserves all fields
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 1: Ollama response parsing preserves all fields
# **Validates: Requirements 1.4**

from bench.harness.framework.backends import OllamaBackend

# Strategy for tool-call argument dicts (simple JSON-safe values)
_tool_arg_strategy = st.dictionaries(
    keys=st.text(
        alphabet=st.characters(whitelist_categories=("L", "N"), blacklist_characters="\x00"),
        min_size=1,
        max_size=20,
    ),
    values=st.one_of(
        st.text(min_size=0, max_size=50),
        st.integers(min_value=-1000, max_value=1000),
        st.booleans(),
        st.floats(allow_nan=False, allow_infinity=False, min_value=-1e6, max_value=1e6),
    ),
    min_size=0,
    max_size=5,
)

# Strategy for a single Ollama-style tool call
_ollama_tool_call_strategy = st.fixed_dictionaries({
    "function": st.fixed_dictionaries({
        "name": st.text(
            alphabet=st.characters(whitelist_categories=("L", "N", "P"), blacklist_characters="\x00\n"),
            min_size=1,
            max_size=30,
        ),
        "arguments": _tool_arg_strategy,
    }),
})

# Strategy for a complete valid Ollama /api/chat response
_ollama_response_strategy = st.fixed_dictionaries({
    "model": st.text(min_size=1, max_size=30),
    "message": st.fixed_dictionaries({
        "role": st.just("assistant"),
        "content": st.text(min_size=0, max_size=200),
        "tool_calls": st.lists(_ollama_tool_call_strategy, min_size=0, max_size=5),
    }),
    "prompt_eval_count": st.integers(min_value=0, max_value=100000),
    "eval_count": st.integers(min_value=0, max_value=100000),
})


@given(response_data=_ollama_response_strategy)
@settings(max_examples=100)
def test_ollama_response_parsing_preserves_all_fields(response_data: dict) -> None:
    """For any valid Ollama /api/chat JSON response containing an assistant
    message, zero or more tool calls, and token usage counts, parsing it into
    a ChatResponse should preserve the message content, all tool call
    names/arguments, and exact token counts."""
    result = OllamaBackend.parse_response(response_data)

    # Message content preserved
    assert result.message.content == response_data["message"]["content"]
    assert result.message.role == "assistant"

    # Tool calls preserved
    raw_tool_calls = response_data["message"]["tool_calls"]
    if raw_tool_calls:
        assert result.message.tool_calls is not None
        assert len(result.message.tool_calls) == len(raw_tool_calls)
        for parsed_tc, raw_tc in zip(result.message.tool_calls, raw_tool_calls):
            assert parsed_tc.name == raw_tc["function"]["name"]
            assert parsed_tc.arguments == raw_tc["function"]["arguments"]
            # ID should be a non-empty string (generated UUID)
            assert isinstance(parsed_tc.id, str)
            assert len(parsed_tc.id) > 0
    else:
        assert result.message.tool_calls is None

    # Token usage preserved exactly
    assert result.usage.prompt_tokens == response_data["prompt_eval_count"]
    assert result.usage.completion_tokens == response_data["eval_count"]
    assert result.usage.total_tokens == (
        response_data["prompt_eval_count"] + response_data["eval_count"]
    )


# ---------------------------------------------------------------------------
# Property 5: Baseline read_file correctness
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 5: Baseline read_file correctness
# **Validates: Requirements 5.2**

from bench.harness.framework.executors import BaselineExecutor

# Strategy for file content: printable text with possible newlines
_file_content_strategy = st.text(
    alphabet=st.characters(
        whitelist_categories=("L", "N", "P", "S", "Z"),
        blacklist_characters="\x00",
    ),
    min_size=0,
    max_size=500,
)

# Strategy for safe filenames (no path separators, no dots-only, no null)
_safe_filename = st.text(
    alphabet=st.characters(
        whitelist_categories=("L", "N"),
    ),
    min_size=1,
    max_size=20,
).map(lambda s: s + ".txt")


@given(
    filenames=st.lists(_safe_filename, min_size=1, max_size=5, unique=True),
    contents=st.lists(_file_content_strategy, min_size=1, max_size=5),
)
@settings(max_examples=100)
def test_baseline_read_file_correctness(
    filenames: list[str], contents: list[str]
) -> None:
    """For any file in the target repository, BaselineExecutor.execute("read_file",
    {"path": file_path}) should return content identical to reading the file
    directly from disk, and byte_size should equal len(content.encode("utf-8"))."""
    # Pair filenames with contents (zip to shortest)
    pairs = list(zip(filenames, contents))

    with tempfile.TemporaryDirectory() as tmp_dir:
        # Write files
        for fname, content in pairs:
            fpath = Path(tmp_dir) / fname
            fpath.write_text(content, encoding="utf-8")

        executor = BaselineExecutor()
        executor.startup(tmp_dir)

        for fname, expected_content in pairs:
            result = executor.execute("read_file", {"path": fname})
            assert result.content == expected_content, (
                f"Content mismatch for {fname}"
            )
            assert result.byte_size == len(expected_content.encode("utf-8")), (
                f"byte_size mismatch for {fname}"
            )

        executor.shutdown()


# ---------------------------------------------------------------------------
# Property 6: Baseline grep_search correctness
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 6: Baseline grep_search correctness
# **Validates: Requirements 5.3**

# Strategy for simple regex-safe search terms (literal words)
_search_word = st.text(
    alphabet=st.characters(whitelist_categories=("L",)),
    min_size=1,
    max_size=10,
)

# Strategy for file lines (no newlines)
_file_line = st.text(
    alphabet=st.characters(
        whitelist_categories=("L", "N", "P", "S", "Z"),
        blacklist_characters="\n\r\x00",
    ),
    min_size=0,
    max_size=80,
)


@given(
    search_word=_search_word,
    filenames=st.lists(_safe_filename, min_size=1, max_size=3, unique=True),
    file_lines=st.lists(
        st.lists(_file_line, min_size=1, max_size=10),
        min_size=1,
        max_size=3,
    ),
)
@settings(max_examples=100)
def test_baseline_grep_search_correctness(
    search_word: str,
    filenames: list[str],
    file_lines: list[list[str]],
) -> None:
    """For any valid regex pattern and set of files, BaselineExecutor.execute(
    "grep_search", ...) should return exactly the lines that match the pattern,
    with correct file paths and line numbers."""
    pairs = list(zip(filenames, file_lines))

    with tempfile.TemporaryDirectory() as tmp_dir:
        # Write files
        for fname, lines in pairs:
            fpath = Path(tmp_dir) / fname
            fpath.write_text("\n".join(lines), encoding="utf-8")

        # Compute expected matches manually using re.escape for literal match
        pattern = re.escape(search_word)
        compiled = re.compile(pattern)
        expected_matches: list[str] = []
        for fname, lines in pairs:
            for line_no, line in enumerate(lines, start=1):
                if compiled.search(line):
                    expected_matches.append(f"{fname}:{line_no}:{line}")

        executor = BaselineExecutor()
        executor.startup(tmp_dir)
        result = executor.execute("grep_search", {"pattern": pattern})
        executor.shutdown()

        if expected_matches:
            actual_lines = set(result.content.strip().split("\n"))
            expected_set = set(expected_matches)
            assert actual_lines == expected_set, (
                f"Mismatch:\nExpected: {expected_set}\nActual: {actual_lines}"
            )
        else:
            assert result.content == "No matches found."


# ---------------------------------------------------------------------------
# Property 4: Byte tracking accuracy
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 4: Byte tracking accuracy
# **Validates: Requirements 4.4, 5.4, 6.2, 6.3**


@given(
    filenames=st.lists(_safe_filename, min_size=1, max_size=5, unique=True),
    contents=st.lists(_file_content_strategy, min_size=1, max_size=5),
    call_indices=st.lists(st.integers(min_value=0, max_value=4), min_size=1, max_size=10),
)
@settings(max_examples=100)
def test_byte_tracking_accuracy(
    filenames: list[str],
    contents: list[str],
    call_indices: list[int],
) -> None:
    """For any sequence of tool executions, the reported total_response_bytes
    should equal the sum of individual ToolResult.byte_size values."""
    pairs = list(zip(filenames, contents))
    if not pairs:
        return

    with tempfile.TemporaryDirectory() as tmp_dir:
        # Write files
        for fname, content in pairs:
            fpath = Path(tmp_dir) / fname
            fpath.write_text(content, encoding="utf-8")

        executor = BaselineExecutor()
        executor.startup(tmp_dir)

        cumulative = 0
        for idx in call_indices:
            # Pick a file to read (mod to stay in range)
            fname, _ = pairs[idx % len(pairs)]
            result = executor.execute("read_file", {"path": fname})
            cumulative += result.byte_size

        assert executor.total_response_bytes() == cumulative, (
            f"Expected {cumulative}, got {executor.total_response_bytes()}"
        )

        executor.shutdown()


# ---------------------------------------------------------------------------
# Property 2: Agentic loop termination
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 2: Agentic loop termination
# **Validates: Requirements 3.1, 3.2**

from bench.harness.framework.agentic_loop import AgenticLoop
from bench.harness.framework.models import (
    ChatResponse,
    Message,
    ModelMetadata,
    ToolCall,
    ToolDef,
    ToolResult,
    TokenUsage,
)


class _MockBackend:
    """Mock backend that returns a pre-configured sequence of ChatResponses."""

    def __init__(self, responses: list[ChatResponse]) -> None:
        self._responses = responses
        self._index = 0

    def chat(self, messages, tools) -> ChatResponse:  # noqa: ANN001
        if self._index < len(self._responses):
            resp = self._responses[self._index]
            self._index += 1
            return resp
        # If exhausted, return a final text response
        return ChatResponse(
            message=Message(role="assistant", content="done", tool_calls=None),
            usage=TokenUsage(prompt_tokens=0, completion_tokens=1, total_tokens=1),
        )

    def metadata(self) -> ModelMetadata:
        return ModelMetadata(name="mock", provider="mock", parameter_count=None, context_window=4096)


class _MockExecutor:
    """Mock executor that returns a fixed ToolResult for any call."""

    def __init__(self, result_content: str = "ok") -> None:
        self._result_content = result_content
        self._total_bytes = 0

    def get_tool_definitions(self) -> list[ToolDef]:
        return [ToolDef(name="mock_tool", description="A mock tool.", parameters={})]

    def execute(self, tool_name: str, arguments: dict) -> ToolResult:
        content = self._result_content
        byte_size = len(content.encode("utf-8"))
        self._total_bytes += byte_size
        return ToolResult(content=content, byte_size=byte_size, latency_ms=0.0)

    def total_response_bytes(self) -> int:
        return self._total_bytes

    def startup(self, repo_path: str) -> None:
        pass

    def shutdown(self) -> None:
        pass


def _tool_response(iteration_tool_name: str = "mock_tool") -> ChatResponse:
    """Build a ChatResponse that contains a single tool call."""
    return ChatResponse(
        message=Message(
            role="assistant",
            content="",
            tool_calls=[ToolCall(id="tc-1", name=iteration_tool_name, arguments={})],
        ),
        usage=TokenUsage(prompt_tokens=1, completion_tokens=1, total_tokens=2),
    )


def _final_response(content: str = "final answer") -> ChatResponse:
    """Build a ChatResponse with no tool calls (final text response)."""
    return ChatResponse(
        message=Message(role="assistant", content=content, tool_calls=None),
        usage=TokenUsage(prompt_tokens=1, completion_tokens=1, total_tokens=2),
    )


@given(
    max_iterations=st.integers(min_value=1, max_value=20),
    final_at=st.integers(min_value=0, max_value=20),
)
@settings(max_examples=100)
def test_agentic_loop_termination(max_iterations: int, final_at: int) -> None:
    """For any max_iterations N and any sequence of model responses, the
    agentic loop should terminate after at most N iterations.

    - If the model produces a final text response at iteration K <= N,
      the loop terminates at K with status "completed".
    - If all N iterations contain tool_calls, the loop terminates with
      status "max_iterations".
    """
    # Build response sequence: `final_at` tool-call responses, then a final response
    responses: list[ChatResponse] = [_tool_response() for _ in range(final_at)]
    responses.append(_final_response())

    backend = _MockBackend(responses)
    executor = _MockExecutor()
    loop = AgenticLoop()

    result = loop.run(
        prompt="test prompt",
        backend=backend,
        executor=executor,
        max_iterations=max_iterations,
        timeout_seconds=300.0,
    )

    if final_at < max_iterations:
        # Final response reached before hitting the limit
        assert result.status == "completed", (
            f"Expected 'completed' when final_at={final_at} < max_iterations={max_iterations}, "
            f"got {result.status!r}"
        )
        assert result.final_answer == "final answer"
    else:
        # All N iterations were tool calls — should hit max_iterations
        assert result.status == "max_iterations", (
            f"Expected 'max_iterations' when final_at={final_at} >= max_iterations={max_iterations}, "
            f"got {result.status!r}"
        )

    # In all cases, the number of tool call records must not exceed max_iterations
    assert len(result.tool_calls) <= max_iterations, (
        f"tool_calls count {len(result.tool_calls)} exceeds max_iterations {max_iterations}"
    )


# ---------------------------------------------------------------------------
# Property 3: Tool call recording completeness
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 3: Tool call recording completeness
# **Validates: Requirements 3.3, 3.4**

# Strategy for tool names (simple identifiers)
_tool_name_strategy = st.text(
    alphabet=st.characters(whitelist_categories=("L", "N"), blacklist_characters="\x00"),
    min_size=1,
    max_size=20,
)

# Strategy for tool argument dicts
_tool_args_strategy = st.dictionaries(
    keys=st.text(
        alphabet=st.characters(whitelist_categories=("L",)),
        min_size=1,
        max_size=10,
    ),
    values=st.one_of(st.text(min_size=0, max_size=20), st.integers()),
    min_size=0,
    max_size=4,
)

# Strategy for a single iteration's tool calls: 1–4 tool calls in one response
_iteration_tool_calls_strategy = st.lists(
    st.builds(
        lambda name, args: ToolCall(id=f"tc-{name}", name=name, arguments=args),
        name=_tool_name_strategy,
        args=_tool_args_strategy,
    ),
    min_size=1,
    max_size=4,
)

# Strategy for result content
_result_content_strategy = st.text(
    alphabet=st.characters(whitelist_categories=("L", "N", "P", "Z")),
    min_size=0,
    max_size=100,
)


class _ConfigurableExecutor:
    """Mock executor that returns configurable content per tool name."""

    def __init__(self, result_content: str) -> None:
        self._result_content = result_content
        self._total_bytes = 0

    def get_tool_definitions(self) -> list[ToolDef]:
        return []

    def execute(self, tool_name: str, arguments: dict) -> ToolResult:
        content = self._result_content
        byte_size = len(content.encode("utf-8"))
        self._total_bytes += byte_size
        return ToolResult(content=content, byte_size=byte_size, latency_ms=1.0)

    def total_response_bytes(self) -> int:
        return self._total_bytes

    def startup(self, repo_path: str) -> None:
        pass

    def shutdown(self) -> None:
        pass


@given(
    iterations_tool_calls=st.lists(
        _iteration_tool_calls_strategy,
        min_size=1,
        max_size=5,
    ),
    result_content=_result_content_strategy,
)
@settings(max_examples=100)
def test_tool_call_recording_completeness(
    iterations_tool_calls: list[list[ToolCall]],
    result_content: str,
) -> None:
    """For any agentic loop run where the model makes tool calls (including
    multiple tool calls in a single response), every tool call should appear
    in the recorded tool call log with correct name, arguments, result byte
    size, and non-negative latency.
    """
    # Build responses: each iteration has tool calls, final response has none
    responses: list[ChatResponse] = []
    for tool_calls in iterations_tool_calls:
        responses.append(
            ChatResponse(
                message=Message(
                    role="assistant",
                    content="",
                    tool_calls=tool_calls,
                ),
                usage=TokenUsage(prompt_tokens=1, completion_tokens=1, total_tokens=2),
            )
        )
    # Add a final text response to terminate cleanly
    responses.append(_final_response())

    backend = _MockBackend(responses)
    executor = _ConfigurableExecutor(result_content)
    loop = AgenticLoop()

    result = loop.run(
        prompt="test prompt",
        backend=backend,
        executor=executor,
        max_iterations=len(iterations_tool_calls) + 1,
        timeout_seconds=300.0,
    )

    # Flatten expected tool calls across all iterations
    expected_calls = [tc for tcs in iterations_tool_calls for tc in tcs]
    expected_byte_size = len(result_content.encode("utf-8"))

    assert len(result.tool_calls) == len(expected_calls), (
        f"Expected {len(expected_calls)} tool call records, got {len(result.tool_calls)}"
    )

    for record, expected_tc in zip(result.tool_calls, expected_calls):
        assert record.tool_name == expected_tc.name, (
            f"tool_name mismatch: expected {expected_tc.name!r}, got {record.tool_name!r}"
        )
        assert record.arguments == expected_tc.arguments, (
            f"arguments mismatch for {expected_tc.name!r}"
        )
        assert record.result_bytes == expected_byte_size, (
            f"result_bytes mismatch: expected {expected_byte_size}, got {record.result_bytes}"
        )
        assert record.latency_ms >= 0.0, (
            f"latency_ms should be non-negative, got {record.latency_ms}"
        )


# ---------------------------------------------------------------------------
# Property 7: Token efficiency ratio computation
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 7: Token efficiency ratio computation
# **Validates: Requirements 6.4**

from bench.harness.framework.token_tracker import TokenTracker


@given(
    baseline_bytes=st.integers(min_value=1, max_value=10_000_000),
    mcp_bytes=st.integers(min_value=1, max_value=10_000_000),
)
@settings(max_examples=100)
def test_token_efficiency_ratio_computation(
    baseline_bytes: int,
    mcp_bytes: int,
) -> None:
    """For any two positive context byte totals (baseline_bytes, mcp_bytes),
    the computed efficiency ratio should equal baseline_bytes / mcp_bytes."""
    ratio = TokenTracker.compute_efficiency_ratio(baseline_bytes, mcp_bytes)
    assert ratio == baseline_bytes / mcp_bytes, (
        f"Expected {baseline_bytes / mcp_bytes}, got {ratio} "
        f"(baseline={baseline_bytes}, mcp={mcp_bytes})"
    )


# ---------------------------------------------------------------------------
# Property 8: Grounding check accuracy
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 8: Grounding check accuracy
# **Validates: Requirements 7.1, 7.2**

from bench.harness.framework.quality_scorer import QualityScorer

# Strategy for valid file extensions
_source_extensions = st.sampled_from(["py", "rs", "go", "ts", "js", "java", "c", "cpp"])

# Strategy for a single path component (ASCII alphanumeric only, no slashes, no dots)
_path_component = st.text(
    alphabet="abcdefghijklmnopqrstuvwxyz0123456789",
    min_size=1,
    max_size=12,
)

# Strategy for a relative file path like "src/foo.py"
_rel_file_path = st.builds(
    lambda parts, ext: "/".join(parts) + "." + ext,
    parts=st.lists(_path_component, min_size=2, max_size=3),
    ext=_source_extensions,
)

# Strategy for a CamelCase symbol name (ASCII only so regex \b and [A-Z][a-z]+ match)
_ascii_lower = st.text(
    alphabet="abcdefghijklmnopqrstuvwxyz",
    min_size=2,
    max_size=8,
)
_camel_symbol = st.builds(
    lambda a, b: a.capitalize() + b.capitalize(),
    a=_ascii_lower,
    b=_ascii_lower,
)


@given(
    all_paths=st.lists(_rel_file_path, min_size=0, max_size=8, unique=True),
    existing_count=st.integers(min_value=0, max_value=8),
    all_symbols=st.lists(_camel_symbol, min_size=0, max_size=6, unique=True),
    existing_sym_count=st.integers(min_value=0, max_value=6),
)
@settings(max_examples=100)
def test_grounding_check_accuracy(
    all_paths: list[str],
    existing_count: int,
    all_symbols: list[str],
    existing_sym_count: int,
) -> None:
    """For any answer text and repository file listing, grounded_files_count
    should equal the number of file paths mentioned in the answer that exist
    in the repository, and grounded_symbols_count should equal the number of
    symbol names mentioned that appear in repository source files."""
    # Split into existing and missing (no overlap by construction)
    existing_paths = all_paths[: min(existing_count, len(all_paths))]
    missing_paths = all_paths[len(existing_paths):]
    existing_symbols = all_symbols[: min(existing_sym_count, len(all_symbols))]
    missing_symbols = all_symbols[len(existing_symbols):]

    with tempfile.TemporaryDirectory() as tmp_dir:
        repo = Path(tmp_dir)

        # Create existing files
        for rel_path in existing_paths:
            full = repo / rel_path
            full.parent.mkdir(parents=True, exist_ok=True)
            full.write_text("# placeholder", encoding="utf-8")

        # Create a source file containing existing symbols
        if existing_symbols:
            sym_file = repo / "symbols.py"
            sym_file.write_text(
                "\n".join(f"class {s}: pass" for s in existing_symbols),
                encoding="utf-8",
            )

        # Build answer text mentioning all paths and symbols
        answer_parts = list(all_paths) + list(all_symbols)
        answer = " ".join(answer_parts) if answer_parts else "no references here"

        scorer = QualityScorer()
        record = scorer.score(answer, tmp_dir, category="general")

        # grounded_files_count must equal the number of existing paths mentioned
        assert record.grounded_files_count == len(existing_paths), (
            f"Expected grounded_files_count={len(existing_paths)}, "
            f"got {record.grounded_files_count} "
            f"(existing={existing_paths}, missing={missing_paths})"
        )

        # grounded_symbols_count must equal the number of existing symbols mentioned
        assert record.grounded_symbols_count == len(existing_symbols), (
            f"Expected grounded_symbols_count={len(existing_symbols)}, "
            f"got {record.grounded_symbols_count} "
            f"(existing={existing_symbols}, missing={missing_symbols})"
        )


# ---------------------------------------------------------------------------
# Property 9: Negative control detection
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 9: Negative control detection
# **Validates: Requirements 7.4**

_NEGATION_PHRASES_TEST = [
    "does not exist",
    "doesn't exist",
    "not found",
    "no such",
    "cannot find",
    "can't find",
    "not present",
]

_POSITIVE_PHRASES_TEST = [
    "the function is defined in",
    "you can find it at",
    "it exists here",
    "the symbol is present",
    "located in the file",
]

_filler_text = st.text(
    alphabet="abcdefghijklmnopqrstuvwxyz0123456789",
    min_size=0,
    max_size=20,
)


@given(
    negation_phrase=st.sampled_from(_NEGATION_PHRASES_TEST),
    positive_phrase=st.sampled_from(_POSITIVE_PHRASES_TEST),
    prefix=_filler_text,
    suffix=_filler_text,
    use_negation=st.booleans(),
)
@settings(max_examples=100)
def test_negative_control_detection(
    negation_phrase: str,
    positive_phrase: str,
    prefix: str,
    suffix: str,
    use_negation: bool,
) -> None:
    """For any answer to a negative_control prompt, is_negative_correct should
    be True if and only if the answer contains language indicating the
    feature/symbol does not exist."""
    with tempfile.TemporaryDirectory() as tmp_dir:
        if use_negation:
            answer = f"{prefix} {negation_phrase} {suffix}"
        else:
            answer = f"{prefix} {positive_phrase} {suffix}"

        scorer = QualityScorer()
        record = scorer.score(answer, tmp_dir, category="negative_control")

        assert record.is_negative_correct is not None, (
            "is_negative_correct should not be None for negative_control category"
        )

        if use_negation:
            assert record.is_negative_correct is True, (
                f"Expected is_negative_correct=True for answer containing {negation_phrase!r}, "
                f"got {record.is_negative_correct}"
            )
        else:
            # Positive-only phrases should not trigger negation detection
            # (unless the filler text accidentally contains a negation phrase)
            answer_lower = answer.lower()
            has_negation = any(p in answer_lower for p in _NEGATION_PHRASES_TEST)
            assert record.is_negative_correct == has_negation, (
                f"is_negative_correct={record.is_negative_correct} but "
                f"has_negation={has_negation} for answer={answer!r}"
            )


# ---------------------------------------------------------------------------
# Property 10: Quality score output invariant
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 10: Quality score output invariant
# **Validates: Requirements 7.5**

_answer_text = st.text(
    alphabet=st.characters(
        whitelist_categories=("L", "N", "P", "S", "Z"),
        blacklist_characters="\x00",
    ),
    min_size=0,
    max_size=300,
)

_category_strategy = st.one_of(
    st.just("negative_control"),
    st.just("symbol_grounding"),
    st.just("find_usages"),
    st.just("architecture"),
    st.just("general"),
    _safe_text,
)


@given(
    answer=_answer_text,
    category=_category_strategy,
)
@settings(max_examples=100)
def test_quality_score_output_invariant(
    answer: str,
    category: str,
) -> None:
    """For any answer and repository context, the QualityRecord should contain
    all required fields and quality_score should be in the range [0.0, 1.0]."""
    with tempfile.TemporaryDirectory() as tmp_dir:
        scorer = QualityScorer()
        record = scorer.score(answer, tmp_dir, category=category)

        # All required fields must be present and correctly typed
        assert isinstance(record.grounded_files_count, int), (
            f"grounded_files_count should be int, got {type(record.grounded_files_count)}"
        )
        assert isinstance(record.grounded_symbols_count, int), (
            f"grounded_symbols_count should be int, got {type(record.grounded_symbols_count)}"
        )
        assert isinstance(record.ungrounded_references_count, int), (
            f"ungrounded_references_count should be int, got {type(record.ungrounded_references_count)}"
        )
        assert record.grounded_files_count >= 0
        assert record.grounded_symbols_count >= 0
        assert record.ungrounded_references_count >= 0

        # is_negative_correct: None for non-negative_control, bool for negative_control
        if category == "negative_control":
            assert isinstance(record.is_negative_correct, bool), (
                f"is_negative_correct should be bool for negative_control, "
                f"got {type(record.is_negative_correct)}"
            )
        else:
            assert record.is_negative_correct is None, (
                f"is_negative_correct should be None for category={category!r}, "
                f"got {record.is_negative_correct}"
            )

        # quality_score must be in [0.0, 1.0]
        assert isinstance(record.quality_score, float), (
            f"quality_score should be float, got {type(record.quality_score)}"
        )
        assert 0.0 <= record.quality_score <= 1.0, (
            f"quality_score={record.quality_score} is outside [0.0, 1.0]"
        )


# ---------------------------------------------------------------------------
# Property 11: Output format correctness
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 11: Output format correctness
# **Validates: Requirements 8.3, 8.4**

from bench.harness.framework.output_writer import OutputWriter, _CSV_HEADERS
from bench.harness.framework.models import (
    QualityRecord,
    RunResult,
    ToolCallRecord,
    TokenUsage as _TokenUsage,
    Message as _Message,
)

_run_status = st.sampled_from(["completed", "max_iterations", "timeout", "error"])
_mode_strategy = st.sampled_from(["mcp", "baseline"])

_token_usage_strategy = st.builds(
    _TokenUsage,
    prompt_tokens=st.integers(min_value=0, max_value=10000),
    completion_tokens=st.integers(min_value=0, max_value=10000),
    total_tokens=st.integers(min_value=0, max_value=20000),
)

_tool_call_record_strategy = st.builds(
    ToolCallRecord,
    iteration=st.integers(min_value=0, max_value=25),
    tool_name=_safe_text,
    arguments=st.dictionaries(
        keys=st.text(alphabet="abcdefghijklmnopqrstuvwxyz", min_size=1, max_size=10),
        values=st.one_of(st.text(min_size=0, max_size=20), st.integers()),
        min_size=0,
        max_size=3,
    ),
    result_bytes=st.integers(min_value=0, max_value=100000),
    latency_ms=st.floats(min_value=0.0, max_value=60000.0, allow_nan=False, allow_infinity=False),
)

_message_strategy = st.builds(
    _Message,
    role=st.sampled_from(["system", "user", "assistant", "tool"]),
    content=st.text(
        alphabet=st.characters(
            whitelist_categories=("L", "N", "P", "S", "Z"),
            blacklist_characters="\x00",
        ),
        min_size=0,
        max_size=100,
    ),
    tool_calls=st.none(),
    tool_call_id=st.none(),
)

_run_result_strategy = st.builds(
    RunResult,
    prompt_id=_safe_text,
    mode=_mode_strategy,
    run_index=st.integers(min_value=0, max_value=10),
    status=_run_status,
    final_answer=st.text(min_size=0, max_size=200),
    conversation=st.lists(_message_strategy, min_size=0, max_size=3),
    tool_calls=st.lists(_tool_call_record_strategy, min_size=0, max_size=3),
    token_usage=_token_usage_strategy,
    total_context_bytes=st.integers(min_value=0, max_value=10_000_000),
    wall_clock_seconds=st.floats(min_value=0.0, max_value=3600.0, allow_nan=False, allow_infinity=False),
    error=st.one_of(st.none(), _safe_text),
)


@given(results=st.lists(_run_result_strategy, min_size=1, max_size=10))
@settings(max_examples=50)
def test_output_format_correctness(results: list[RunResult]) -> None:
    """For any list of RunResult objects, the output writer should produce:
    - results.jsonl where each line is valid JSON with all run fields
    - results.csv that is valid CSV with correct headers
    - raw/<prompt_id>/<mode>/run_<n>/ directories for each run
    """
    with tempfile.TemporaryDirectory() as tmp_dir:
        writer = OutputWriter(tmp_dir)

        # Write each run
        for result in results:
            writer.write_run(result)

        # Write CSV summary (no quality records)
        writer.write_csv_summary(results, [None] * len(results))

        out = Path(tmp_dir)

        # --- Validate results.jsonl ---
        jsonl_path = out / "results.jsonl"
        assert jsonl_path.exists(), "results.jsonl should exist"
        lines = jsonl_path.read_text(encoding="utf-8").strip().split("\n")
        assert len(lines) == len(results), (
            f"Expected {len(results)} lines in results.jsonl, got {len(lines)}"
        )
        for i, (line, result) in enumerate(zip(lines, results)):
            obj = json.loads(line)  # must be valid JSON
            assert obj["prompt_id"] == result.prompt_id, f"Line {i}: prompt_id mismatch"
            assert obj["mode"] == result.mode, f"Line {i}: mode mismatch"
            assert obj["run_index"] == result.run_index, f"Line {i}: run_index mismatch"
            assert obj["status"] == result.status, f"Line {i}: status mismatch"
            assert "token_usage" in obj, f"Line {i}: missing token_usage"
            assert "conversation" in obj, f"Line {i}: missing conversation"
            assert "tool_calls" in obj, f"Line {i}: missing tool_calls"

        # --- Validate results.csv ---
        csv_path = out / "results.csv"
        assert csv_path.exists(), "results.csv should exist"
        import csv as _csv
        with csv_path.open(encoding="utf-8") as f:
            reader = _csv.DictReader(f)
            assert reader.fieldnames is not None
            for header in _CSV_HEADERS:
                assert header in reader.fieldnames, f"CSV missing header: {header}"
            csv_rows = list(reader)
        assert len(csv_rows) == len(results), (
            f"Expected {len(results)} CSV rows, got {len(csv_rows)}"
        )

        # --- Validate raw directories ---
        for result in results:
            raw_dir = (
                out / "raw" / result.prompt_id / result.mode / f"run_{result.run_index}"
            )
            assert raw_dir.exists(), f"Raw dir missing: {raw_dir}"
            assert (raw_dir / "conversation.json").exists(), (
                f"conversation.json missing in {raw_dir}"
            )
            assert (raw_dir / "tool_calls.json").exists(), (
                f"tool_calls.json missing in {raw_dir}"
            )
            # Validate conversation.json is valid JSON list
            conv = json.loads((raw_dir / "conversation.json").read_text(encoding="utf-8"))
            assert isinstance(conv, list)
            assert len(conv) == len(result.conversation)
            # Validate tool_calls.json is valid JSON list
            tcs = json.loads((raw_dir / "tool_calls.json").read_text(encoding="utf-8"))
            assert isinstance(tcs, list)
            assert len(tcs) == len(result.tool_calls)


# ---------------------------------------------------------------------------
# Property 13: Claim mapping correctness
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 13: Claim mapping correctness
# **Validates: Requirements 9.2, 10.4**

from bench.harness.framework.claim_report import ClaimReport

_known_category_strategy = st.sampled_from(list(ClaimReport.CATEGORY_TO_CLAIM.keys()))
_claim_name_strategy = st.sampled_from(ClaimReport.CLAIM_CATEGORIES)


@given(
    category=_known_category_strategy,
    explicit_claim=st.one_of(st.none(), _claim_name_strategy),
)
@settings(max_examples=100)
def test_claim_mapping_correctness(
    category: str,
    explicit_claim: str | None,
) -> None:
    """For any prompt with a claim field and any prompt with a known category,
    the result should be associated with the correct README claim.
    Explicit claim fields take precedence over category-based mapping.
    """
    report = ClaimReport()

    prompt = PromptRow(
        id="test-prompt",
        category=category,
        prompt="test",
        claim=explicit_claim,
    )

    resolved = report._resolve_claim(prompt)

    if explicit_claim is not None:
        # Explicit claim takes precedence
        assert resolved == explicit_claim, (
            f"Expected explicit claim {explicit_claim!r}, got {resolved!r}"
        )
    else:
        # Category-based mapping
        expected = ClaimReport.CATEGORY_TO_CLAIM.get(category)
        assert resolved == expected, (
            f"Expected category mapping {expected!r} for category {category!r}, got {resolved!r}"
        )


@given(
    explicit_claim=_claim_name_strategy,
    category=_known_category_strategy,
)
@settings(max_examples=100)
def test_claim_explicit_takes_precedence(
    explicit_claim: str,
    category: str,
) -> None:
    """When a prompt has both an explicit claim and a known category,
    the explicit claim always wins."""
    report = ClaimReport()
    prompt = PromptRow(
        id="test-prompt",
        category=category,
        prompt="test",
        claim=explicit_claim,
    )
    resolved = report._resolve_claim(prompt)
    assert resolved == explicit_claim, (
        f"Explicit claim {explicit_claim!r} should override category {category!r}, "
        f"got {resolved!r}"
    )


# ---------------------------------------------------------------------------
# Property 14: Claim summary aggregation
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 14: Claim summary aggregation
# **Validates: Requirements 9.3**

_quality_record_strategy = st.builds(
    QualityRecord,
    grounded_files_count=st.integers(min_value=0, max_value=20),
    grounded_symbols_count=st.integers(min_value=0, max_value=20),
    ungrounded_references_count=st.integers(min_value=0, max_value=20),
    is_negative_correct=st.none(),
    quality_score=st.floats(min_value=0.0, max_value=1.0, allow_nan=False, allow_infinity=False),
)

@given(
    claim=_claim_name_strategy,
    entries=st.lists(
        st.tuples(_run_result_strategy, st.one_of(st.none(), _quality_record_strategy)),
        min_size=0,
        max_size=10,
    ),
)
@settings(max_examples=50)
def test_claim_summary_aggregation(
    claim: str,
    entries: list[tuple[RunResult, QualityRecord | None]],
) -> None:
    """For any set of run results mapped to a claim, the claim summary should
    count unique MCP prompts with scored answers, avg_quality_score equal
    to the mean per-prompt MCP quality, and verdict consistent with the data.
    """
    report = ClaimReport()
    summary = report._build_summary(claim, entries)

    scored_mcp: dict[str, list[float]] = {}
    for result, quality in entries:
        if result.mode == "mcp" and quality is not None:
            scored_mcp.setdefault(result.prompt_id, []).append(quality.quality_score)

    expected_prompts_tested = len(scored_mcp)
    assert summary.prompts_tested == expected_prompts_tested, (
        f"Expected prompts_tested={expected_prompts_tested}, got {summary.prompts_tested}"
    )

    # avg_quality_score == mean of per-prompt MCP quality (or 0.0 if none)
    per_prompt_scores = [
        sum(scores) / len(scores)
        for scores in scored_mcp.values()
        if scores
    ]
    if per_prompt_scores:
        expected_avg_quality = sum(per_prompt_scores) / len(per_prompt_scores)
        assert abs(summary.avg_quality_score - expected_avg_quality) < 1e-9, (
            f"avg_quality_score mismatch: expected {expected_avg_quality}, "
            f"got {summary.avg_quality_score}"
        )
    else:
        assert summary.avg_quality_score == 0.0, (
            f"Expected avg_quality_score=0.0 when no quality records, "
            f"got {summary.avg_quality_score}"
        )

    # Verdict consistency
    n = expected_prompts_tested
    avg_q = summary.avg_quality_score
    if n == 0:
        assert summary.verdict == "insufficient_data", (
            f"Expected 'insufficient_data' when prompts_tested=0, got {summary.verdict!r}"
        )
    elif n >= 3 and avg_q >= 0.7:
        assert summary.verdict == "validated", (
            f"Expected 'validated' when prompts_tested={n} and avg_quality={avg_q:.2f}, "
            f"got {summary.verdict!r}"
        )
    elif n >= 1:
        # Could be "validated" or "partially_supported" depending on quality
        if avg_q >= 0.7 and n >= 3:
            assert summary.verdict == "validated"
        else:
            assert summary.verdict in ("validated", "partially_supported"), (
                f"Expected 'validated' or 'partially_supported' when prompts_tested={n}, "
                f"got {summary.verdict!r}"
            )
    else:
        assert summary.verdict == "insufficient_data"


# ---------------------------------------------------------------------------
# Property 12: Failure resilience
# ---------------------------------------------------------------------------
# Feature: realworld-benchmark-framework, Property 12: Failure resilience
# **Validates: Requirements 8.5**

import tempfile

from bench.harness.framework.benchmark_runner import BenchmarkRunner
from bench.harness.framework.models import (
    ChatResponse as _ChatResponse,
    Message as _Msg,
    ModelMetadata as _ModelMetadata,
    TokenUsage as _TU,
    ToolDef as _ToolDef,
    ToolResult as _ToolResult,
)

# Strategy: for each (prompt, mode, run) slot, should the run raise an exception?
_fail_flag = st.booleans()


class _FailableBackend:
    """Mock backend that raises RuntimeError when told to fail."""

    def __init__(self, should_fail: bool) -> None:
        self._should_fail = should_fail

    def chat(self, messages, tools):  # noqa: ANN001
        if self._should_fail:
            raise RuntimeError("Simulated backend failure")
        return _ChatResponse(
            message=_Msg(role="assistant", content="answer", tool_calls=None),
            usage=_TU(prompt_tokens=1, completion_tokens=1, total_tokens=2),
        )

    def metadata(self) -> _ModelMetadata:
        return _ModelMetadata(name="mock", provider="mock", parameter_count=None, context_window=4096)


class _NullExecutor:
    """Minimal executor that does nothing."""

    def get_tool_definitions(self) -> list[_ToolDef]:
        return []

    def execute(self, tool_name: str, arguments: dict) -> _ToolResult:
        return _ToolResult(content="ok", byte_size=2, latency_ms=0.0)

    def total_response_bytes(self) -> int:
        return 0

    def startup(self, repo_path: str) -> None:
        pass

    def shutdown(self) -> None:
        pass


@given(
    prompt_ids=st.lists(
        st.text(
            alphabet=st.characters(whitelist_categories=("L", "N")),
            min_size=1,
            max_size=10,
        ),
        min_size=1,
        max_size=5,
        unique=True,
    ),
    runs_per_prompt=st.integers(min_value=1, max_value=3),
    mode=st.sampled_from(["mcp", "baseline", "both"]),
    fail_flags=st.lists(_fail_flag, min_size=1, max_size=30),
)
@settings(max_examples=100)
def test_failure_resilience(
    prompt_ids: list[str],
    runs_per_prompt: int,
    mode: str,
    fail_flags: list[bool],
) -> None:
    """For any sequence of prompts where some runs fail or timeout:
    - All non-failed runs should have complete results (status != 'error' or
      status in completed/max_iterations/timeout).
    - Failed runs should have error details recorded (error field non-None).
    - The total number of result records should equal the total number of
      attempted runs.
    """
    modes = ["mcp", "baseline"] if mode == "both" else [mode]
    total_expected = len(prompt_ids) * len(modes) * runs_per_prompt

    # Cycle through fail_flags to decide per-run failure
    flag_iter = iter(fail_flags * ((total_expected // max(len(fail_flags), 1)) + 2))

    with tempfile.TemporaryDirectory() as tmp_dir:
        # Write a minimal JSONL prompt file
        import json as _json
        prompts_path = str(Path(tmp_dir) / "prompts.jsonl")
        with open(prompts_path, "w", encoding="utf-8") as f:
            for pid in prompt_ids:
                f.write(_json.dumps({"id": pid, "category": "general", "prompt": "test"}) + "\n")

        out_dir = str(Path(tmp_dir) / "out")

        runner = BenchmarkRunner(
            repo_path=tmp_dir,
            prompt_set_path=prompts_path,
            model_name="mock",
            output_dir=out_dir,
            runs_per_prompt=runs_per_prompt,
            mode=mode,
            max_iterations=2,
            timeout_seconds=30.0,
        )

        # Build a backend that fails based on the flag sequence
        fail_sequence = [next(flag_iter) for _ in range(total_expected)]
        call_index = [0]

        class _SequencedBackend:
            def chat(self, messages, tools):  # noqa: ANN001
                idx = call_index[0]
                call_index[0] += 1
                should_fail = fail_sequence[idx] if idx < len(fail_sequence) else False
                if should_fail:
                    raise RuntimeError(f"Simulated failure at run {idx}")
                return _ChatResponse(
                    message=_Msg(role="assistant", content="answer", tool_calls=None),
                    usage=_TU(prompt_tokens=1, completion_tokens=1, total_tokens=2),
                )

            def metadata(self) -> _ModelMetadata:
                return _ModelMetadata(name="mock", provider="mock", parameter_count=None, context_window=4096)

        backend = _SequencedBackend()
        mcp_exec = _NullExecutor()
        baseline_exec = _NullExecutor()

        # BenchmarkRunner.run() must not raise even when individual runs fail
        runner.run(backend, mcp_exec, baseline_exec)

        # Verify results.jsonl has exactly total_expected records
        import json as _json2
        results_path = Path(out_dir) / "results.jsonl"
        assert results_path.exists(), "results.jsonl must be created"
        lines = [l for l in results_path.read_text(encoding="utf-8").splitlines() if l.strip()]
        assert len(lines) == total_expected, (
            f"Expected {total_expected} result records, got {len(lines)} "
            f"(prompts={len(prompt_ids)}, modes={modes}, runs={runs_per_prompt})"
        )

        # Verify each record: failed runs have error field, non-failed have status
        for i, line in enumerate(lines):
            obj = _json2.loads(line)
            assert "status" in obj, f"Record {i} missing 'status'"
            assert "error" in obj, f"Record {i} missing 'error'"
            if obj["status"] == "error":
                # Failed run must have error details
                assert obj["error"] is not None and obj["error"] != "", (
                    f"Record {i} has status='error' but no error details"
                )
            else:
                # Non-failed run must have a valid status
                assert obj["status"] in ("completed", "max_iterations", "timeout"), (
                    f"Record {i} has unexpected status {obj['status']!r}"
                )
