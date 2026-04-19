"""Runtime adapter interfaces for benchmark execution."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Protocol


@dataclass
class RuntimeRequest:
    """Normalized execution request passed to runtime adapters."""

    repo_path: str
    prompt_set_path: str
    model_name: str
    output_dir: str
    runs_per_prompt: int
    mode: str
    max_iterations: int
    timeout_seconds: float
    temperature: float
    context_window: int
    runtime_type: str
    suite_id: str
    suite_manifest_path: str | None
    scorer_version: str
    resume: bool
    force: bool
    prompt_ids: list[str] = field(default_factory=list)
    backend_type: str = "ollama"
    target_specs: list[str] = field(default_factory=list)
    agent: str | None = None
    title_prefix: str = "bench"
    prompt_suffix: str = ""
    runtime_extra_args: list[str] = field(default_factory=list)
    dry_run: bool = False


class BenchmarkRuntime(Protocol):
    """Execution runtime contract for one benchmark job."""

    name: str

    def run(self, request: RuntimeRequest) -> None:
        """Execute the benchmark job and persist raw execution artifacts."""
