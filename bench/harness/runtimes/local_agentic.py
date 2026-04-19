"""Local runtime adapter for the current agentic benchmark path."""

from __future__ import annotations

from bench.harness.framework.benchmark_runner import BenchmarkRunner
from bench.harness.framework.executors import BaselineExecutor
from bench.harness.framework.mcp_executor import MCPExecutor
from bench.harness.runtimes.base import RuntimeRequest


class LocalAgenticRuntime:
    """Current local benchmark runtime using the in-process agentic loop."""

    name = "local"

    def __init__(self, backend_factory) -> None:  # noqa: ANN001
        self._backend_factory = backend_factory

    def run(self, request: RuntimeRequest) -> None:
        backend = self._backend_factory(request)
        runner = BenchmarkRunner(
            repo_path=request.repo_path,
            prompt_set_path=request.prompt_set_path,
            model_name=request.model_name,
            output_dir=request.output_dir,
            runs_per_prompt=request.runs_per_prompt,
            mode=request.mode,
            max_iterations=request.max_iterations,
            timeout_seconds=request.timeout_seconds,
            temperature=request.temperature,
            context_window=request.context_window,
            runtime_type=request.runtime_type,
            suite_id=request.suite_id,
            suite_manifest_path=request.suite_manifest_path,
            scorer_version=request.scorer_version,
            resume=request.resume,
            force=request.force,
            prompt_ids=request.prompt_ids,
        )
        runner.run(backend, MCPExecutor(), BaselineExecutor())

