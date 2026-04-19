"""Unit tests for the benchmark framework.

Tests:
  1. CLI argument parsing
  2. Tool definition lists
  3. Claim category enumeration
  4. Hardware detection

Requirements: 4.2, 5.1, 9.1, 12.2
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from unittest.mock import patch

import pytest

from bench.harness.bench_runner import _build_parser
from bench.harness.bench_opencode import parse_args as parse_opencode_args
from bench.harness.framework.agentic_loop import AgenticLoop
from bench.harness.framework.backends import LMStudioBackend
from bench.harness.grade import grade_run
from bench.harness.manifest import build_run_manifest, load_suite_manifest
from bench.harness.resume import instance_dir
from bench.harness.framework.benchmark_runner import BenchmarkRunner
from bench.harness.framework.claim_report import ClaimReport
from bench.harness.framework.executors import BaselineExecutor
from bench.harness.framework.mcp_executor import MCPExecutor, PITLANE_TOOL_NAMES
from bench.harness.framework.models import (
    ChatResponse,
    Message,
    ModelMetadata,
    TokenUsage,
    ToolDef,
)
from bench.harness.run import create_runtime, main as run_main
from bench.harness.runtimes.base import RuntimeRequest
from bench.harness.runtimes.local_agentic import LocalAgenticRuntime
from bench.harness.runtimes.opencode import (
    OpenCodeRuntime,
    Target,
    _row_to_run_result,
    build_opencode_command,
    execute_opencode_request,
    normalize_opencode_events,
)


# ---------------------------------------------------------------------------
# 1. CLI argument parsing
# ---------------------------------------------------------------------------


class TestCLIArgumentParsing:
    """Tests for _build_parser() in bench_runner.py."""

    def test_required_repo(self):
        """--repo is required; omitting it raises SystemExit."""
        parser = _build_parser()
        with pytest.raises(SystemExit):
            parser.parse_args(["--prompts", "p.jsonl", "--model", "m", "--out", "o"])

    def test_required_prompts(self):
        """--prompts is required; omitting it raises SystemExit."""
        parser = _build_parser()
        with pytest.raises(SystemExit):
            parser.parse_args(["--repo", "/r", "--model", "m", "--out", "o"])

    def test_required_model(self):
        """--model is required; omitting it raises SystemExit."""
        parser = _build_parser()
        with pytest.raises(SystemExit):
            parser.parse_args(["--repo", "/r", "--prompts", "p.jsonl", "--out", "o"])

    def test_required_out(self):
        """--out is required; omitting it raises SystemExit."""
        parser = _build_parser()
        with pytest.raises(SystemExit):
            parser.parse_args(["--repo", "/r", "--prompts", "p.jsonl", "--model", "m"])

    def test_all_required_args_accepted(self):
        """All four required args together should parse without error."""
        parser = _build_parser()
        args = parser.parse_args(
            ["--repo", "/r", "--prompts", "p.jsonl", "--model", "qwen3:8b", "--out", "out/"]
        )
        assert args.repo == "/r"
        assert args.prompts == "p.jsonl"
        assert args.model == "qwen3:8b"
        assert args.out == "out/"

    def test_mode_choices_valid(self):
        """--mode accepts 'both', 'mcp', 'baseline'."""
        parser = _build_parser()
        base = ["--repo", "/r", "--prompts", "p.jsonl", "--model", "m", "--out", "o"]
        for choice in ("both", "mcp", "baseline"):
            args = parser.parse_args(base + ["--mode", choice])
            assert args.mode == choice

    def test_mode_invalid_choice(self):
        """--mode rejects unknown values."""
        parser = _build_parser()
        with pytest.raises(SystemExit):
            parser.parse_args(
                ["--repo", "/r", "--prompts", "p.jsonl", "--model", "m", "--out", "o",
                 "--mode", "invalid"]
            )

    def test_backend_choices_valid(self):
        """--backend accepts 'ollama', 'openrouter', and 'lmstudio'."""
        parser = _build_parser()
        base = ["--repo", "/r", "--prompts", "p.jsonl", "--model", "m", "--out", "o"]
        for choice in ("ollama", "openrouter", "lmstudio"):
            args = parser.parse_args(base + ["--backend", choice])
            assert args.backend == choice

    def test_backend_invalid_choice(self):
        """--backend rejects unknown values."""
        parser = _build_parser()
        with pytest.raises(SystemExit):
            parser.parse_args(
                ["--repo", "/r", "--prompts", "p.jsonl", "--model", "m", "--out", "o",
                 "--backend", "anthropic"]
            )

    def test_defaults(self):
        """Default values are applied when optional args are omitted."""
        parser = _build_parser()
        args = parser.parse_args(
            ["--repo", "/r", "--prompts", "p.jsonl", "--model", "m", "--out", "o"]
        )
        assert args.runs == 3
        assert args.mode == "both"
        assert args.backend == "ollama"
        assert args.runtime == "local"
        assert args.max_iterations == 25
        assert args.timeout == 300.0
        assert args.temperature == 0.0
        assert args.context_window == 8192
        assert args.resume is False
        assert args.force is False
        assert args.prompt_ids == []
        assert args.skip_grade is False

    def test_optional_args_override(self):
        """Optional args can be overridden from the command line."""
        parser = _build_parser()
        args = parser.parse_args([
            "--repo", "/r", "--prompts", "p.jsonl", "--model", "m", "--out", "o",
            "--runs", "5",
            "--mode", "mcp",
            "--backend", "lmstudio",
            "--runtime", "opencode",
            "--max-iterations", "10",
            "--timeout", "60.0",
            "--temperature", "0.7",
            "--context-window", "4096",
        ])
        assert args.runs == 5
        assert args.mode == "mcp"
        assert args.backend == "lmstudio"
        assert args.runtime == "opencode"
        assert args.max_iterations == 10
        assert args.timeout == 60.0
        assert args.temperature == 0.7
        assert args.context_window == 4096

    def test_resume_force_and_prompt_filters(self):
        """Resume flags and prompt filters parse cleanly."""
        parser = _build_parser()
        args = parser.parse_args([
            "--repo", "/r",
            "--prompts", "p.jsonl",
            "--model", "m",
            "--out", "o",
            "--resume",
            "--force",
            "--prompt-id", "prompt-1",
            "--prompt-id", "prompt-2",
        ])
        assert args.resume is True
        assert args.force is True
        assert args.prompt_ids == ["prompt-1", "prompt-2"]
        assert args.skip_grade is False

    def test_skip_grade_flag(self):
        """The compatibility wrapper should accept --skip-grade."""
        parser = _build_parser()
        args = parser.parse_args([
            "--repo", "/r",
            "--prompts", "p.jsonl",
            "--model", "m",
            "--out", "o",
            "--skip-grade",
        ])
        assert args.skip_grade is True

    def test_opencode_runtime_args_parse(self):
        """The compatibility wrapper should accept OpenCode runtime options."""
        parser = _build_parser()
        args = parser.parse_args([
            "--repo", "/r",
            "--prompts", "p.jsonl",
            "--model", "m",
            "--out", "o",
            "--runtime", "opencode",
            "--target", "mcp=http://localhost:4096",
            "--agent", "build",
            "--title-prefix", "bench",
            "--prompt-suffix", "Ground it",
            "--extra-arg=--some-flag",
            "--dry-run",
        ])
        assert args.runtime == "opencode"
        assert args.target == ["mcp=http://localhost:4096"]
        assert args.agent == "build"
        assert args.title_prefix == "bench"
        assert args.prompt_suffix == "Ground it"
        assert args.runtime_extra_args == ["--some-flag"]
        assert args.dry_run is True


class TestPatchSetOneHelpers:
    """Tests for suite loading, manifest construction, and per-instance paths."""

    def test_load_suite_manifest_ripgrep(self):
        """The ripgrep suite manifest should load and expose pinned local inputs."""
        suite, suite_path = load_suite_manifest("bench/harness/suites/ripgrep-core-v1.json")
        assert suite.suite_id == "ripgrep-core-v1"
        assert suite.defaults.runs == 3
        assert suite.defaults.max_iterations == 25
        assert suite.defaults.timeout_seconds == 300
        assert suite_path.name == "ripgrep-core-v1.json"

    def test_build_run_manifest_hashes_prompt_set(self, tmp_path: Path):
        """Run manifests should capture immutable prompt-set identity and flags."""
        prompts = tmp_path / "prompts.jsonl"
        prompts.write_text('{"id":"p1","category":"general","prompt":"hi"}\n', encoding="utf-8")

        manifest = build_run_manifest(
            suite_id="adhoc-prompts",
            repo_path=str(tmp_path),
            prompt_set_path=str(prompts),
            model_name="mock-model",
            backend_type="ollama",
            runtime_type="local",
            mode="mcp",
            runs_per_prompt=2,
            max_iterations=5,
            timeout_seconds=30.0,
            temperature=0.1,
            context_window=4096,
            prompt_filter=["p1"],
            resume_enabled=True,
            force_enabled=False,
        )

        assert manifest.schema_version == "1"
        assert manifest.suite_id == "adhoc-prompts"
        assert manifest.prompt_set_path == str(prompts.resolve())
        assert len(manifest.prompt_set_sha256) == 64
        assert manifest.prompt_filter == ["p1"]
        assert manifest.resume_enabled is True
        assert manifest.force_enabled is False
        assert manifest.runtime_type == "local"

    def test_instance_dir_uses_stable_safe_slug(self, tmp_path: Path):
        """Instance directories should be deterministic and path-safe."""
        artifact_dir = instance_dir(tmp_path, "feature/path prompt", "mcp", 2)
        expected = tmp_path / "raw" / artifact_dir.parts[-3] / "mcp" / "run_2"
        assert artifact_dir == expected
        assert "/" not in artifact_dir.parts[-3]
        assert artifact_dir.parts[-2] == "mcp"


class TestRuntimeSelection:
    """Tests for runtime adapter resolution."""

    def test_create_runtime_local(self):
        """The local runtime should resolve to the local agentic adapter."""
        runtime = create_runtime("local", "ollama")
        assert isinstance(runtime, LocalAgenticRuntime)

    def test_create_runtime_opencode(self):
        """The opencode runtime should resolve to the reserved adapter."""
        runtime = create_runtime("opencode", "ollama")
        assert isinstance(runtime, OpenCodeRuntime)

    def test_create_runtime_rejects_unknown_runtime(self):
        """Unknown runtime names should raise a ValueError."""
        with pytest.raises(ValueError):
            create_runtime("unknown", "ollama")


class TestOpenCodeRuntimeHelpers:
    """Tests for OpenCode command construction and parsing helpers."""

    def test_build_opencode_command_resolves_config_target_to_absolute_path(self, tmp_path: Path):
        """CONFIG targets should export an absolute OPENCODE_CONFIG path."""
        config = tmp_path / "sample.opencode.json"
        config.write_text("{}", encoding="utf-8")

        previous_cwd = Path.cwd()
        try:
            os.chdir(tmp_path)
            target = Target(label="with-mcp", value=f"CONFIG:{config.name}")
            cmd, env = build_opencode_command(
                target=target,
                full_prompt="hello",
                agent=None,
                model="openai/gpt-5.4-mini",
                title="bench-test",
                extra_args=[],
            )
        finally:
            os.chdir(previous_cwd)

        assert cmd[:4] == ["opencode", "run", "--format", "json"]
        assert env["OPENCODE_CONFIG"] == str(config.resolve())


class _RecordingRuntime:
    def __init__(self) -> None:
        self.requests = []

    def run(self, request):  # noqa: ANN001
        self.requests.append(request)


class TestCanonicalRunMain:
    """Tests for the canonical run.py entrypoint orchestration."""

    def test_main_grades_non_dry_run_opencode(self, tmp_path: Path):
        """Canonical run.py should grade persisted OpenCode artifacts on non-dry runs."""
        prompts = tmp_path / "prompts.jsonl"
        prompts.write_text(
            '{"id":"prompt-1","category":"architecture","prompt":"Explain the flow"}\n',
            encoding="utf-8",
        )
        runtime = _RecordingRuntime()

        with patch("bench.harness.run.create_runtime", return_value=runtime) as create_runtime_mock:
            with patch("bench.harness.run.grade_run") as grade_run_mock:
                run_main([
                    "--repo", str(tmp_path),
                    "--prompts", str(prompts),
                    "--model", "openai/gpt-5.4-mini",
                    "--out", str(tmp_path / "out"),
                    "--runtime", "opencode",
                    "--target", "with-mcp=http://127.0.0.1:4096",
                    "--runs", "1",
                ])

        create_runtime_mock.assert_called_once_with("opencode", "ollama")
        grade_run_mock.assert_called_once_with(str(tmp_path / "out"))
        assert len(runtime.requests) == 1
        assert runtime.requests[0].runtime_type == "opencode"
        assert runtime.requests[0].dry_run is False

    def test_main_skips_grade_for_dry_run_opencode(self, tmp_path: Path):
        """Canonical run.py should not grade dry-run OpenCode requests."""
        prompts = tmp_path / "prompts.jsonl"
        prompts.write_text(
            '{"id":"prompt-1","category":"architecture","prompt":"Explain the flow"}\n',
            encoding="utf-8",
        )
        runtime = _RecordingRuntime()

        with patch("bench.harness.run.create_runtime", return_value=runtime):
            with patch("bench.harness.run.grade_run") as grade_run_mock:
                run_main([
                    "--repo", str(tmp_path),
                    "--prompts", str(prompts),
                    "--model", "openai/gpt-5.4-mini",
                    "--out", str(tmp_path / "out"),
                    "--runtime", "opencode",
                    "--target", "with-mcp=http://127.0.0.1:4096",
                    "--runs", "1",
                    "--dry-run",
                ])

        grade_run_mock.assert_not_called()
        assert len(runtime.requests) == 1
        assert runtime.requests[0].runtime_type == "opencode"
        assert runtime.requests[0].dry_run is True


class TestLMStudioBackend:
    """Tests for the LM Studio OpenAI-compatible backend parser."""

    def test_parse_response_preserves_tool_calls_and_usage(self):
        """LM Studio responses should parse like other OpenAI-compatible providers."""
        response = {
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "done",
                        "tool_calls": [
                            {
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "search_symbols",
                                    "arguments": "{\"query\":\"ignore\",\"project\":\"/tmp/repo\"}",
                                },
                            }
                        ],
                    }
                }
            ],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 7,
                "total_tokens": 18,
            },
        }

        parsed = LMStudioBackend.parse_response(response)

        assert parsed.message.role == "assistant"
        assert parsed.message.content == "done"
        assert parsed.message.tool_calls is not None
        assert len(parsed.message.tool_calls) == 1
        assert parsed.message.tool_calls[0].id == "call_1"
        assert parsed.message.tool_calls[0].name == "search_symbols"
        assert parsed.message.tool_calls[0].arguments == {
            "query": "ignore",
            "project": "/tmp/repo",
        }
        assert parsed.usage.prompt_tokens == 11
        assert parsed.usage.completion_tokens == 7
        assert parsed.usage.total_tokens == 18
        assert parsed.finish_reason is None
        assert parsed.reasoning_content is None

    def test_parse_response_falls_back_to_reasoning_content(self):
        """LM Studio reasoning-only turns should not collapse to an empty answer."""
        response = {
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "reasoning_content": "Replying with exactly OK.",
                        "tool_calls": [],
                    },
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 14,
                "completion_tokens": 32,
                "total_tokens": 46,
            },
        }

        parsed = LMStudioBackend.parse_response(response)

        assert parsed.message.content == "Replying with exactly OK."
        assert parsed.reasoning_content == "Replying with exactly OK."
        assert parsed.finish_reason == "stop"

    def test_parse_response_normalizes_block_content(self):
        """OpenAI-compatible block content should be joined into plain text."""
        response = {
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": [
                            {"type": "text", "text": "hello "},
                            {"type": "output_text", "text": "world"},
                        ],
                        "tool_calls": [],
                    },
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 3,
                "completion_tokens": 2,
                "total_tokens": 5,
            },
        }

        parsed = LMStudioBackend.parse_response(response)

        assert parsed.message.content == "hello world"
        assert parsed.finish_reason == "stop"

    def test_parse_response_extracts_inline_tool_call_markup(self):
        """LM Studio inline tool-call markup should be normalized into tool_calls."""
        response = {
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": (
                            "Let me inspect that.\n\n"
                            "<tool_call>\n"
                            "<function=get_project_outline>\n"
                            "<parameter=project>/tmp/repo</parameter>\n"
                            "<parameter=depth>2</parameter>\n"
                            "</function>\n"
                            "</tool_call>"
                        ),
                        "tool_calls": [],
                    },
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 12,
                "total_tokens": 22,
            },
        }

        parsed = LMStudioBackend.parse_response(response)

        assert parsed.message.content == "Let me inspect that."
        assert parsed.message.tool_calls is not None
        assert len(parsed.message.tool_calls) == 1
        assert parsed.message.tool_calls[0].name == "get_project_outline"
        assert parsed.message.tool_calls[0].arguments == {
            "project": "/tmp/repo",
            "depth": 2,
        }

    def test_chat_applies_cooldown_between_calls(self):
        """LM Studio chat calls should honor the configured inter-request cooldown."""
        response = {
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }
        with patch.dict("os.environ", {"LMSTUDIO_COOLDOWN_SECONDS": "2.0"}):
            with patch.object(LMStudioBackend, "_validate_model", return_value=None):
                backend = LMStudioBackend("google/gemma-3-4b")
        with patch.object(LMStudioBackend, "_post", side_effect=[response, response]):
            with patch("bench.harness.framework.backends.time.monotonic", side_effect=[100.0, 100.5, 102.5]):
                with patch("bench.harness.framework.backends.time.sleep") as sleep_mock:
                    first = backend.chat([Message(role="user", content="hi")], [])
                    second = backend.chat([Message(role="user", content="again")], [])

        assert first.message.content == "ok"
        assert second.message.content == "ok"
        sleep_mock.assert_called_once_with(1.5)


class _SequenceBackend:
    def __init__(self, responses: list[ChatResponse]) -> None:
        self._responses = responses
        self.calls = 0

    def chat(self, messages, tools):  # noqa: ANN001
        response = self._responses[self.calls]
        self.calls += 1
        return response

    def metadata(self) -> ModelMetadata:
        return ModelMetadata(
            name="mock-model",
            provider="mock",
            parameter_count=None,
            context_window=4096,
        )


class TestAgenticLoopLMStudioCompatibility:
    """Regression coverage for LM Studio/OpenAI-compatible response handling."""

    def test_reasoning_content_completes_when_visible_content_is_empty(self, tmp_path: Path):
        """The loop should use reasoning_content when LM Studio omits visible content."""
        backend = _SequenceBackend([
            ChatResponse(
                message=Message(role="assistant", content=""),
                usage=TokenUsage(prompt_tokens=1, completion_tokens=1, total_tokens=2),
                finish_reason="stop",
                reasoning_content="The answer is in reasoning content.",
            )
        ])

        result = AgenticLoop().run(
            prompt="Explain the flow",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=2,
            timeout_seconds=10.0,
            mode="baseline",
            repo_path=str(tmp_path),
        )

        assert result.status == "completed"
        assert result.final_answer == "The answer is in reasoning content."

    def test_empty_length_response_does_not_count_as_completed(self, tmp_path: Path):
        """An empty length-truncated reply should not be accepted as a final answer."""
        backend = _SequenceBackend([
            ChatResponse(
                message=Message(role="assistant", content=""),
                usage=TokenUsage(prompt_tokens=1, completion_tokens=1, total_tokens=2),
                finish_reason="length",
                reasoning_content="Partial reasoning only",
            )
        ])

        result = AgenticLoop().run(
            prompt="Explain the flow",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=2,
            timeout_seconds=10.0,
            mode="baseline",
            repo_path=str(tmp_path),
        )

        assert result.status == "max_iterations"
        assert result.final_answer == ""


class TestOpenCodeRuntime:
    """Tests for the shared OpenCode runtime implementation."""

    def test_bench_opencode_parser(self, monkeypatch: pytest.MonkeyPatch):
        """The compatibility wrapper parser should accept the shared target syntax."""
        monkeypatch.setattr(
            sys,
            "argv",
            [
                "bench_opencode.py",
                "--repo", "/tmp/repo",
                "--prompts", "/tmp/prompts.jsonl",
                "--target", "mcp=http://localhost:4096",
            ],
        )
        args = parse_opencode_args()
        assert args.repo == "/tmp/repo"
        assert args.prompts == "/tmp/prompts.jsonl"
        assert args.target == ["mcp=http://localhost:4096"]

    def test_execute_opencode_request_dry_run(self, tmp_path: Path):
        """The shared OpenCode runtime should emit the canonical pre-grade artifact set in dry-run mode."""
        prompts = tmp_path / "prompts.jsonl"
        prompts.write_text(
            '{"id":"prompt-1","prompt":"Explain the flow","category":"architecture"}\n',
            encoding="utf-8",
        )
        out_dir = tmp_path / "out"
        request = RuntimeRequest(
            repo_path=str(tmp_path),
            prompt_set_path=str(prompts),
            model_name="openai/gpt-5.4-mini",
            output_dir=str(out_dir),
            runs_per_prompt=1,
            mode="both",
            max_iterations=1,
            timeout_seconds=0.0,
            temperature=0.0,
            context_window=0,
            runtime_type="opencode",
            suite_id="adhoc-prompts",
            suite_manifest_path=None,
            scorer_version="manual",
            resume=False,
            force=False,
            target_specs=["mcp=http://localhost:4096"],
            agent="build",
            title_prefix="bench",
            prompt_suffix="Ground it",
            runtime_extra_args=[],
            dry_run=True,
        )

        result = execute_opencode_request(request, agents_md_path=None)

        assert result == 0
        assert (out_dir / "config.json").exists()
        assert (out_dir / "run_manifest.json").exists()
        assert (out_dir / "results.jsonl").exists()
        assert not (out_dir / "results.csv").exists()
        assert not (out_dir / "claim_report.md").exists()
        assert (out_dir / "summary.txt").exists()
        canonical_dir = instance_dir(out_dir, "prompt-1", "mcp", 0)
        assert (canonical_dir / "result.json").exists()
        assert (canonical_dir / "conversation.json").exists()
        assert (canonical_dir / "tool_calls.json").exists()
        payload = json.loads((out_dir / "results.jsonl").read_text(encoding="utf-8").strip())
        assert payload["mode"] == "mcp"
        assert payload["status"] == "dry_run"

    def test_normalize_opencode_events_preserves_tool_calls_and_final_answer(self):
        """OpenCode event normalization should retain the multi-step tool transcript."""
        events = [
            {
                "type": "step_start",
                "part": {"messageID": "msg-1"},
            },
            {
                "type": "text",
                "part": {"messageID": "msg-1", "text": "Let me inspect the repo."},
            },
            {
                "type": "tool_use",
                "part": {
                    "messageID": "msg-1",
                    "tool": "pitlane-mcp_trace_execution_path",
                    "callID": "call-1",
                    "state": {
                        "input": {"project": "/tmp/repo", "query": "main path"},
                        "output": "{\"count\":1}",
                        "time": {"start": 1000, "end": 1125},
                    },
                },
            },
            {
                "type": "step_finish",
                "part": {
                    "messageID": "msg-1",
                    "reason": "tool-calls",
                    "tokens": {"input": 10, "output": 20, "total": 30},
                },
            },
            {
                "type": "step_start",
                "part": {"messageID": "msg-2"},
            },
            {
                "type": "text",
                "part": {"messageID": "msg-2", "text": "The main path starts in crates/core/main.rs."},
            },
            {
                "type": "step_finish",
                "part": {
                    "messageID": "msg-2",
                    "reason": "stop",
                    "tokens": {"input": 3, "output": 7, "total": 10},
                },
            },
        ]

        conversation, tool_calls, final_answer, usage = normalize_opencode_events(events)

        assert final_answer == "The main path starts in crates/core/main.rs."
        assert usage.prompt_tokens == 13
        assert usage.completion_tokens == 27
        assert usage.total_tokens == 40
        assert len(tool_calls) == 1
        assert tool_calls[0].tool_name == "trace_execution_path"
        assert tool_calls[0].arguments == {"project": "/tmp/repo", "query": "main path"}
        assert tool_calls[0].result_bytes == len("{\"count\":1}".encode("utf-8"))
        assert tool_calls[0].latency_ms == 125.0
        assert len(conversation) == 3
        assert conversation[0].role == "assistant"
        assert conversation[0].tool_calls is not None
        assert conversation[0].tool_calls[0].name == "trace_execution_path"
        assert conversation[1].role == "tool"
        assert conversation[1].tool_call_id == "call-1"
        assert conversation[2].role == "assistant"
        assert conversation[2].content == final_answer

    def test_row_to_run_result_preserves_normalized_conversation_and_tools(self):
        """Canonical RunResult conversion should keep normalized OpenCode tool structure."""
        row = {
            "prompt_id": "prompt-1",
            "target": "with-mcp",
            "run_index": 1,
            "status": "completed",
            "answer_preview": "final answer",
            "input_tokens": 11,
            "output_tokens": 13,
            "total_tokens": 24,
            "latency_seconds": 1.5,
            "conversation": [
                {
                    "role": "assistant",
                    "content": "Let me inspect it.",
                    "tool_calls": [
                        {"id": "call-1", "name": "trace_execution_path", "arguments": {"project": "/tmp/repo"}},
                    ],
                    "tool_call_id": None,
                },
                {
                    "role": "tool",
                    "content": "{\"count\":1}",
                    "tool_calls": None,
                    "tool_call_id": "call-1",
                },
                {
                    "role": "assistant",
                    "content": "final answer",
                    "tool_calls": None,
                    "tool_call_id": None,
                },
            ],
            "tool_calls": [
                {
                    "iteration": 1,
                    "tool_name": "trace_execution_path",
                    "arguments": {"project": "/tmp/repo"},
                    "result_bytes": 11,
                    "latency_ms": 125.0,
                }
            ],
        }

        result = _row_to_run_result(row)

        assert result.final_answer == "final answer"
        assert len(result.conversation) == 3
        assert result.conversation[0].tool_calls is not None
        assert result.conversation[0].tool_calls[0].name == "trace_execution_path"
        assert len(result.tool_calls) == 1
        assert result.tool_calls[0].tool_name == "trace_execution_path"
        assert result.total_context_bytes == 11


class _NullExecutor:
    def startup(self, repo_path: str) -> None:
        self.repo_path = repo_path

    def shutdown(self) -> None:
        pass

    def get_tool_definitions(self) -> list[ToolDef]:
        return []

    def execute(self, tool_name: str, arguments: dict):  # noqa: ANN001
        raise AssertionError(f"Unexpected tool execution: {tool_name} {arguments}")

    def total_response_bytes(self) -> int:
        return 0


class _StaticBackend:
    def __init__(self, *, fail: bool = False) -> None:
        self.fail = fail
        self.calls = 0

    def chat(self, messages, tools):  # noqa: ANN001
        self.calls += 1
        if self.fail:
            raise AssertionError("backend should not have been called")
        return ChatResponse(
            message=Message(role="assistant", content="grounded answer"),
            usage=TokenUsage(prompt_tokens=1, completion_tokens=1, total_tokens=2),
        )

    def metadata(self) -> ModelMetadata:
        return ModelMetadata(
            name="mock-model",
            provider="mock",
            parameter_count=None,
            context_window=4096,
        )


class _CaptureBackend:
    def __init__(self) -> None:
        self.calls = 0
        self.last_messages = None

    def chat(self, messages, tools):  # noqa: ANN001
        self.calls += 1
        self.last_messages = messages
        return ChatResponse(
            message=Message(role="assistant", content="grounded answer"),
            usage=TokenUsage(prompt_tokens=1, completion_tokens=1, total_tokens=2),
        )

    def metadata(self) -> ModelMetadata:
        return ModelMetadata(
            name="mock-model",
            provider="mock",
            parameter_count=None,
            context_window=4096,
        )


class TestAgenticLoopPromptGuidance:
    """Coverage for MCP-specific system prompt steering."""

    def test_mcp_architecture_prompt_includes_orientation_guidance(self, tmp_path: Path):
        backend = _CaptureBackend()

        result = AgenticLoop().run(
            prompt="Explain the package layout",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=1,
            timeout_seconds=10.0,
            mode="mcp",
            prompt_id="arch_package_map",
            repo_path=str(tmp_path),
        )

        assert result.status == "completed"
        assert backend.last_messages is not None
        system_prompt = backend.last_messages[0].content
        assert "Architecture prompt guidance:" in system_prompt
        assert "get_index_stats once" in system_prompt
        assert "Use locate_code to find the central files after orientation." in system_prompt
        assert "Do not read many crate root files just to build a package map." in system_prompt
        assert "Do not switch to generic read or glob" in system_prompt

    def test_mcp_symbol_prompt_includes_subsystem_stop_guidance(self, tmp_path: Path):
        backend = _CaptureBackend()

        AgenticLoop().run(
            prompt="Find ignore handling",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=1,
            timeout_seconds=10.0,
            mode="mcp",
            prompt_id="symbol_ignore_logic",
            repo_path=str(tmp_path),
        )

        assert backend.last_messages is not None
        system_prompt = backend.last_messages[0].content
        assert "Implementation lookup guidance:" in system_prompt
        assert "Use one focused discovery call, then read the strongest two to four targets and answer." in system_prompt
        assert "stay inside that subsystem" in system_prompt
        assert "issue one sharper subsystem query" in system_prompt

    def test_mcp_enumeration_probe_includes_mechanism_guidance(self, tmp_path: Path):
        backend = _CaptureBackend()

        AgenticLoop().run(
            prompt="List all exclusion mechanisms",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=1,
            timeout_seconds=10.0,
            mode="mcp",
            prompt_id="smart_exclusions_probe",
            repo_path=str(tmp_path),
        )

        assert backend.last_messages is not None
        system_prompt = backend.last_messages[0].content
        assert "Enumeration guidance:" in system_prompt
        assert "identify the mechanism list first" in system_prompt
        assert "Do not retry the same concept with multiple broad searches" in system_prompt
        assert "Do not fan out into generic glob or raw file reads" in system_prompt

    def test_mcp_prompt_discourages_generic_file_tools(self, tmp_path: Path):
        backend = _CaptureBackend()

        AgenticLoop().run(
            prompt="Map the repo",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=1,
            timeout_seconds=10.0,
            mode="mcp",
            prompt_id="arch_package_map",
            repo_path=str(tmp_path),
        )

        assert backend.last_messages is not None
        system_prompt = backend.last_messages[0].content
        assert "Do not use generic read_file/read/read-style tools" in system_prompt
        assert "Do not use generic list_directory/glob/search-style tools" in system_prompt
        assert "Do not use bash/shell tools for code lookup" in system_prompt
        assert "Generic file tools are escape hatches" in system_prompt

    def test_mcp_cli_flow_prompt_discourages_generic_entrypoint_searches(self, tmp_path: Path):
        backend = _CaptureBackend()

        AgenticLoop().run(
            prompt="Trace CLI config flow",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=1,
            timeout_seconds=10.0,
            mode="mcp",
            prompt_id="symbol_cli_config_flow",
            repo_path=str(tmp_path),
        )

        assert backend.last_messages is not None
        system_prompt = backend.last_messages[0].content
        assert "CLI flow guidance:" in system_prompt
        assert "Do not search for generic terms like main function" in system_prompt
        assert "Prefer locate_code queries that name the concrete subsystem" in system_prompt
        assert "before using any generic read, glob, or shell tool" in system_prompt

    def test_mcp_regex_path_prompt_discourages_directory_exploration(self, tmp_path: Path):
        backend = _CaptureBackend()

        AgenticLoop().run(
            prompt="Trace regex search path",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=1,
            timeout_seconds=10.0,
            mode="mcp",
            prompt_id="symbol_regex_search_path",
            repo_path=str(tmp_path),
        )

        assert backend.last_messages is not None
        system_prompt = backend.last_messages[0].content
        assert "Execution-path guidance:" in system_prompt
        assert "Prefer trace_path plus focused read_code_unit calls" in system_prompt
        assert "Do not use generic read or glob" in system_prompt

    def test_mcp_ignore_prompt_discourages_shell_listing(self, tmp_path: Path):
        backend = _CaptureBackend()

        AgenticLoop().run(
            prompt="Find ignore handling",
            backend=backend,
            executor=_NullExecutor(),
            max_iterations=1,
            timeout_seconds=10.0,
            mode="mcp",
            prompt_id="symbol_ignore_logic",
            repo_path=str(tmp_path),
        )

        assert backend.last_messages is not None
        system_prompt = backend.last_messages[0].content
        assert "Ignore-subsystem guidance:" in system_prompt
        assert "Prefer read_code_unit line slices or symbol reads" in system_prompt
        assert "Do not use shell listing or globbing once the main ignore files are known." in system_prompt


class TestBenchmarkRunnerResume:
    """Patch Set 1 resume/force behavior."""

    def _write_prompts(self, tmp_path: Path) -> Path:
        prompts = tmp_path / "prompts.jsonl"
        prompts.write_text(
            '{"id":"prompt-1","category":"general","prompt":"Explain the flow"}\n',
            encoding="utf-8",
        )
        return prompts

    def test_resume_skips_completed_instance(self, tmp_path: Path):
        """A resumed run should reuse existing instance artifacts."""
        prompts = self._write_prompts(tmp_path)
        out_dir = tmp_path / "out"
        first_backend = _StaticBackend()
        runner = BenchmarkRunner(
            repo_path=str(tmp_path),
            prompt_set_path=str(prompts),
            model_name="mock-model",
            output_dir=str(out_dir),
            runs_per_prompt=1,
            mode="baseline",
            max_iterations=2,
            timeout_seconds=10.0,
        )
        runner.run(first_backend, _NullExecutor(), _NullExecutor())
        assert first_backend.calls == 1

        resumed_backend = _StaticBackend(fail=True)
        resumed_runner = BenchmarkRunner(
            repo_path=str(tmp_path),
            prompt_set_path=str(prompts),
            model_name="mock-model",
            output_dir=str(out_dir),
            runs_per_prompt=1,
            mode="baseline",
            max_iterations=2,
            timeout_seconds=10.0,
            resume=True,
        )
        resumed_runner.run(resumed_backend, _NullExecutor(), _NullExecutor())

        assert resumed_backend.calls == 0
        results_lines = (out_dir / "results.jsonl").read_text(encoding="utf-8").splitlines()
        assert len(results_lines) == 1
        assert (out_dir / "run_manifest.json").exists()

    def test_force_reruns_completed_instance(self, tmp_path: Path):
        """Force should ignore existing instance artifacts and rerun."""
        prompts = self._write_prompts(tmp_path)
        out_dir = tmp_path / "out"
        initial_runner = BenchmarkRunner(
            repo_path=str(tmp_path),
            prompt_set_path=str(prompts),
            model_name="mock-model",
            output_dir=str(out_dir),
            runs_per_prompt=1,
            mode="baseline",
            max_iterations=2,
            timeout_seconds=10.0,
        )
        initial_runner.run(_StaticBackend(), _NullExecutor(), _NullExecutor())

        forced_backend = _StaticBackend()
        forced_runner = BenchmarkRunner(
            repo_path=str(tmp_path),
            prompt_set_path=str(prompts),
            model_name="mock-model",
            output_dir=str(out_dir),
            runs_per_prompt=1,
            mode="baseline",
            max_iterations=2,
            timeout_seconds=10.0,
            resume=True,
            force=True,
        )
        forced_runner.run(forced_backend, _NullExecutor(), _NullExecutor())
        assert forced_backend.calls == 1

    def test_grade_run_regenerates_derived_outputs(self, tmp_path: Path):
        """Grading should regenerate quality artifacts and derived summaries from raw results."""
        prompts = self._write_prompts(tmp_path)
        out_dir = tmp_path / "out"
        runner = BenchmarkRunner(
            repo_path=str(tmp_path),
            prompt_set_path=str(prompts),
            model_name="mock-model",
            output_dir=str(out_dir),
            runs_per_prompt=1,
            mode="baseline",
            max_iterations=2,
            timeout_seconds=10.0,
        )
        runner.run(_StaticBackend(), _NullExecutor(), _NullExecutor())

        assert (out_dir / "results.jsonl").exists()
        assert not (out_dir / "results.csv").exists()
        assert not (out_dir / "claim_report.md").exists()

        grade_run(out_dir)

        instance = instance_dir(out_dir, "prompt-1", "baseline", 0)
        assert (instance / "quality.json").exists()
        assert (out_dir / "results.csv").exists()
        assert (out_dir / "claim_report.md").exists()
        assert (out_dir / "results.jsonl").exists()

    def test_grade_run_grades_opencode_runtime(self, tmp_path: Path):
        """The canonical grader should accept OpenCode outputs once they use the shared run contract."""
        prompts = tmp_path / "prompts.jsonl"
        prompts.write_text(
            '{"id":"prompt-1","prompt":"Explain the flow","category":"architecture"}\n',
            encoding="utf-8",
        )
        out_dir = tmp_path / "out"
        request = RuntimeRequest(
            repo_path=str(tmp_path),
            prompt_set_path=str(prompts),
            model_name="openai/gpt-5.4-mini",
            output_dir=str(out_dir),
            runs_per_prompt=1,
            mode="both",
            max_iterations=1,
            timeout_seconds=0.0,
            temperature=0.0,
            context_window=0,
            runtime_type="opencode",
            suite_id="adhoc-prompts",
            suite_manifest_path=None,
            scorer_version="manual",
            resume=False,
            force=False,
            target_specs=["mcp=http://localhost:4096"],
            agent="build",
            title_prefix="bench",
            prompt_suffix="Ground it",
            runtime_extra_args=[],
            dry_run=True,
        )
        execute_opencode_request(request, agents_md_path=None)

        results, qualities = grade_run(out_dir)

        assert len(results) == 1
        assert results[0].mode == "mcp"
        assert results[0].status == "dry_run"
        assert qualities == [None]
        assert (out_dir / "results.csv").exists()
        assert (out_dir / "claim_report.md").exists()
        assert (out_dir / "results.jsonl").exists()


# ---------------------------------------------------------------------------
# 2. Tool definition lists
# ---------------------------------------------------------------------------


class TestToolDefinitionLists:
    """Tests for tool definitions exposed by executors."""

    def test_baseline_executor_exposes_exactly_3_tools(self):
        """BaselineExecutor.get_tool_definitions() returns exactly 3 tools."""
        executor = BaselineExecutor()
        tools = executor.get_tool_definitions()
        assert len(tools) == 3

    def test_baseline_executor_tool_names(self):
        """BaselineExecutor exposes read_file, grep_search, list_directory."""
        executor = BaselineExecutor()
        names = {t.name for t in executor.get_tool_definitions()}
        assert names == {"read_file", "grep_search", "list_directory"}

    def test_baseline_executor_no_pitlane_tools(self):
        """BaselineExecutor must not expose any pitlane-mcp tool names."""
        executor = BaselineExecutor()
        names = {t.name for t in executor.get_tool_definitions()}
        pitlane_names = set(PITLANE_TOOL_NAMES)
        overlap = names & pitlane_names
        assert overlap == set(), f"Unexpected pitlane tools in baseline: {overlap}"

    def test_mcp_executor_pitlane_tool_names_count(self):
        """MCPExecutor.PITLANE_TOOL_NAMES has exactly 7 default tools."""
        assert len(PITLANE_TOOL_NAMES) == 7

    def test_mcp_executor_pitlane_tool_names_content(self):
        """PITLANE_TOOL_NAMES contains the expected pitlane-mcp tools."""
        expected = {
            "ensure_project_ready",
            "locate_code",
            "read_code_unit",
            "trace_path",
            "analyze_impact",
            "get_index_stats",
            "search_content",
        }
        assert set(PITLANE_TOOL_NAMES) == expected

    def test_baseline_tool_definitions_have_required_fields(self):
        """Each ToolDef from BaselineExecutor has name, description, parameters."""
        executor = BaselineExecutor()
        for tool in executor.get_tool_definitions():
            assert tool.name, "Tool name must be non-empty"
            assert tool.description, "Tool description must be non-empty"
            assert isinstance(tool.parameters, dict), "Tool parameters must be a dict"

    def test_mcp_executor_injects_required_project_argument(self):
        """MCPExecutor should backfill a required project arg from startup state."""
        executor = MCPExecutor()
        executor._repo_path = "/tmp/repo"
        executor._tool_defs = [
            ToolDef(
                name="trace_execution_path",
                description="",
                parameters={
                    "type": "object",
                    "properties": {"project": {"type": "string"}},
                    "required": ["project"],
                },
            )
        ]

        normalized = executor._normalize_arguments("trace_execution_path", {"query": "x"})

        assert normalized["project"] == "/tmp/repo"
        assert normalized["query"] == "x"

    def test_mcp_executor_wraps_tool_errors(self):
        """MCPExecutor should surface tool-call failures as tool output, not abort."""
        executor = MCPExecutor()
        executor._process = object()

        with patch.object(executor, "_check_process_alive"), patch.object(
            executor, "_call_tool", side_effect=RuntimeError("boom")
        ):
            result = executor.execute("search_symbols", {"query": "needle"})

        assert result.content == "Error: boom"
        assert result.byte_size == len(result.content.encode("utf-8"))


# ---------------------------------------------------------------------------
# 3. Claim category enumeration
# ---------------------------------------------------------------------------


class TestClaimCategoryEnumeration:
    """Tests for ClaimReport.CLAIM_CATEGORIES."""

    def test_claim_categories_count(self):
        """ClaimReport.CLAIM_CATEGORIES has exactly 8 entries."""
        assert len(ClaimReport.CLAIM_CATEGORIES) == 8

    def test_claim_categories_match_readme(self):
        """CLAIM_CATEGORIES contains the 8 README claims."""
        expected = {
            "token_efficiency",
            "indexing_speed",
            "bm25_search_quality",
            "graph_navigation",
            "semantic_search_quality",
            "incremental_reindexing",
            "smart_exclusions",
            "fully_local_operation",
        }
        assert set(ClaimReport.CLAIM_CATEGORIES) == expected

    def test_claim_categories_are_strings(self):
        """All claim category entries are non-empty strings."""
        for cat in ClaimReport.CLAIM_CATEGORIES:
            assert isinstance(cat, str) and cat, f"Invalid category: {cat!r}"

    def test_claim_categories_no_duplicates(self):
        """CLAIM_CATEGORIES has no duplicate entries."""
        assert len(ClaimReport.CLAIM_CATEGORIES) == len(set(ClaimReport.CLAIM_CATEGORIES))


# ---------------------------------------------------------------------------
# 4. Hardware detection
# ---------------------------------------------------------------------------


class TestHardwareDetection:
    """Tests for _detect_cpu() and _detect_ram_gb() in benchmark_runner.py."""

    def test_detect_cpu_returns_string_or_none(self):
        """_detect_cpu() returns a non-empty string or None (never raises)."""
        from bench.harness.framework.benchmark_runner import _detect_cpu
        result = _detect_cpu()
        assert result is None or (isinstance(result, str) and len(result) > 0)

    def test_detect_ram_gb_returns_float_or_none(self):
        """_detect_ram_gb() returns a positive float or None (never raises)."""
        from bench.harness.framework.benchmark_runner import _detect_ram_gb
        result = _detect_ram_gb()
        assert result is None or (isinstance(result, float) and result > 0)

    def test_detect_cpu_on_linux(self):
        """On Linux, _detect_cpu() returns a non-None string."""
        import platform
        from bench.harness.framework.benchmark_runner import _detect_cpu
        if platform.system() == "Linux":
            result = _detect_cpu()
            assert result is not None, "_detect_cpu() should return a value on Linux"
            assert len(result) > 0

    def test_detect_ram_gb_on_linux(self):
        """On Linux, _detect_ram_gb() returns a positive float."""
        import platform
        from bench.harness.framework.benchmark_runner import _detect_ram_gb
        if platform.system() == "Linux":
            result = _detect_ram_gb()
            assert result is not None, "_detect_ram_gb() should return a value on Linux"
            assert result > 0

    def test_detect_cpu_graceful_on_missing_proc(self):
        """_detect_cpu() returns None gracefully when /proc/cpuinfo is absent."""
        from bench.harness.framework.benchmark_runner import _detect_cpu
        from pathlib import Path
        with patch.object(Path, "read_text", side_effect=OSError("no such file")):
            result = _detect_cpu()
            assert result is None

    def test_detect_ram_gb_graceful_on_missing_proc(self):
        """_detect_ram_gb() returns None gracefully when /proc/meminfo is absent."""
        from bench.harness.framework.benchmark_runner import _detect_ram_gb
        from pathlib import Path
        with patch.object(Path, "read_text", side_effect=OSError("no such file")):
            result = _detect_ram_gb()
            assert result is None
