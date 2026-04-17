"""Unit tests for the benchmark framework.

Tests:
  1. CLI argument parsing
  2. Tool definition lists
  3. Claim category enumeration
  4. Hardware detection

Requirements: 4.2, 5.1, 9.1, 12.2
"""

from __future__ import annotations

import sys
from pathlib import Path
from unittest.mock import patch

import pytest

from bench.harness.bench_runner import _build_parser
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
        """--backend accepts 'ollama' and 'openrouter'."""
        parser = _build_parser()
        base = ["--repo", "/r", "--prompts", "p.jsonl", "--model", "m", "--out", "o"]
        for choice in ("ollama", "openrouter"):
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
        assert args.max_iterations == 25
        assert args.timeout == 300.0
        assert args.temperature == 0.0
        assert args.context_window == 8192
        assert args.resume is False
        assert args.force is False
        assert args.prompt_ids == []

    def test_optional_args_override(self):
        """Optional args can be overridden from the command line."""
        parser = _build_parser()
        args = parser.parse_args([
            "--repo", "/r", "--prompts", "p.jsonl", "--model", "m", "--out", "o",
            "--runs", "5",
            "--mode", "mcp",
            "--backend", "openrouter",
            "--max-iterations", "10",
            "--timeout", "60.0",
            "--temperature", "0.7",
            "--context-window", "4096",
        ])
        assert args.runs == 5
        assert args.mode == "mcp"
        assert args.backend == "openrouter"
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
        """MCPExecutor.PITLANE_TOOL_NAMES has exactly 14 tools."""
        assert len(PITLANE_TOOL_NAMES) == 14

    def test_mcp_executor_pitlane_tool_names_content(self):
        """PITLANE_TOOL_NAMES contains the expected pitlane-mcp tools."""
        expected = {
            "ensure_project_ready",
            "search_symbols",
            "get_symbol",
            "find_usages",
            "find_callers",
            "find_callees",
            "search_content",
            "search_files",
            "trace_execution_path",
            "get_file_outline",
            "get_project_outline",
            "get_lines",
            "get_index_stats",
            "get_usage_stats",
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
