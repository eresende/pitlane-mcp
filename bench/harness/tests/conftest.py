"""Shared pytest fixtures for the benchmark framework test suite.

Requirements: 3.1, 5.1
"""

from __future__ import annotations

import tempfile
from pathlib import Path
from typing import Generator

import pytest

from bench.harness.framework.models import (
    ChatResponse,
    Message,
    ModelMetadata,
    PromptRow,
    ToolDef,
    ToolResult,
    TokenUsage,
)


# ---------------------------------------------------------------------------
# Mock ModelBackend
# ---------------------------------------------------------------------------


class MockModelBackend:
    """Configurable mock ModelBackend that returns a final text response.

    By default returns a single final text response with no tool calls.
    Set `responses` to a list of ChatResponse objects to control the sequence.
    """

    def __init__(self, responses: list[ChatResponse] | None = None) -> None:
        if responses is None:
            responses = [
                ChatResponse(
                    message=Message(role="assistant", content="mock answer", tool_calls=None),
                    usage=TokenUsage(prompt_tokens=10, completion_tokens=5, total_tokens=15),
                )
            ]
        self._responses = responses
        self._index = 0
        self.calls: list[tuple] = []  # records (messages, tools) per call

    def chat(self, messages: list, tools: list) -> ChatResponse:
        self.calls.append((messages, tools))
        if self._index < len(self._responses):
            resp = self._responses[self._index]
            self._index += 1
            return resp
        # Fallback: final text response
        return ChatResponse(
            message=Message(role="assistant", content="done", tool_calls=None),
            usage=TokenUsage(prompt_tokens=0, completion_tokens=1, total_tokens=1),
        )

    def metadata(self) -> ModelMetadata:
        return ModelMetadata(
            name="mock-model",
            provider="mock",
            parameter_count=None,
            context_window=4096,
        )


@pytest.fixture
def mock_backend() -> MockModelBackend:
    """Return a configurable mock ModelBackend that returns a final text response."""
    return MockModelBackend()


# ---------------------------------------------------------------------------
# Temporary repository fixture
# ---------------------------------------------------------------------------


@pytest.fixture
def temp_repo(tmp_path: Path) -> Path:
    """Create a temporary directory with sample Python and Rust files."""
    # Python file
    py_file = tmp_path / "main.py"
    py_file.write_text(
        "def hello_world():\n"
        "    \"\"\"Print a greeting.\"\"\"\n"
        "    print('Hello, world!')\n"
        "\n"
        "class MyClass:\n"
        "    def __init__(self, value: int) -> None:\n"
        "        self.value = value\n"
        "\n"
        "    def get_value(self) -> int:\n"
        "        return self.value\n",
        encoding="utf-8",
    )

    # Rust file
    rs_file = tmp_path / "lib.rs"
    rs_file.write_text(
        "pub fn add(a: i32, b: i32) -> i32 {\n"
        "    a + b\n"
        "}\n"
        "\n"
        "pub struct Counter {\n"
        "    count: u32,\n"
        "}\n"
        "\n"
        "impl Counter {\n"
        "    pub fn new() -> Self {\n"
        "        Counter { count: 0 }\n"
        "    }\n"
        "\n"
        "    pub fn increment(&mut self) {\n"
        "        self.count += 1;\n"
        "    }\n"
        "}\n",
        encoding="utf-8",
    )

    # A subdirectory with another file
    sub = tmp_path / "src"
    sub.mkdir()
    (sub / "utils.py").write_text(
        "import os\n\n"
        "def read_file(path: str) -> str:\n"
        "    with open(path) as f:\n"
        "        return f.read()\n",
        encoding="utf-8",
    )

    return tmp_path


# ---------------------------------------------------------------------------
# Sample PromptRow objects
# ---------------------------------------------------------------------------


@pytest.fixture
def sample_prompts() -> list[PromptRow]:
    """Return a list of sample PromptRow objects."""
    return [
        PromptRow(
            id="token_efficiency_001",
            category="token_efficiency_probe",
            prompt="How many tokens does it take to find the main entry point?",
            prompt_suffix=None,
            claim="token_efficiency",
        ),
        PromptRow(
            id="symbol_grounding_001",
            category="symbol_grounding",
            prompt="Where is the `hello_world` function defined?",
            prompt_suffix="Provide the file path and line number.",
            claim=None,
        ),
        PromptRow(
            id="negative_control_001",
            category="negative_control",
            prompt="Does this repository contain a function called `nonexistent_function_xyz`?",
            prompt_suffix=None,
            claim=None,
        ),
        PromptRow(
            id="architecture_001",
            category="architecture",
            prompt="Describe the high-level architecture of this codebase.",
            prompt_suffix=None,
            claim=None,
        ),
    ]


# ---------------------------------------------------------------------------
# Mock ToolExecutor
# ---------------------------------------------------------------------------


class MockToolExecutor:
    """Mock ToolExecutor that returns 'ok' for any tool call."""

    def __init__(self, result_content: str = "ok") -> None:
        self._result_content = result_content
        self._total_bytes = 0
        self.calls: list[tuple[str, dict]] = []  # records (tool_name, arguments)

    def get_tool_definitions(self) -> list[ToolDef]:
        return [
            ToolDef(
                name="mock_tool",
                description="A mock tool for testing.",
                parameters={
                    "type": "object",
                    "properties": {
                        "input": {"type": "string", "description": "Input value."}
                    },
                    "required": [],
                },
            )
        ]

    def execute(self, tool_name: str, arguments: dict) -> ToolResult:
        self.calls.append((tool_name, arguments))
        content = self._result_content
        byte_size = len(content.encode("utf-8"))
        self._total_bytes += byte_size
        return ToolResult(content=content, byte_size=byte_size, latency_ms=0.1)

    def total_response_bytes(self) -> int:
        return self._total_bytes

    def startup(self, repo_path: str) -> None:
        pass

    def shutdown(self) -> None:
        pass


@pytest.fixture
def mock_executor() -> MockToolExecutor:
    """Return a mock ToolExecutor that returns 'ok' for any call."""
    return MockToolExecutor()
