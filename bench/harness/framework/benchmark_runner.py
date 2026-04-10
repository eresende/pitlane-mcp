"""BenchmarkRunner — orchestrates a full benchmark run.

Loads prompts, runs AgenticLoop for each prompt × mode × run index,
collects metrics, writes outputs, and generates a ClaimReport.
"""

from __future__ import annotations

import datetime
import subprocess
import sys
from pathlib import Path
from typing import TYPE_CHECKING

from bench.harness.framework.agentic_loop import AgenticLoop
from bench.harness.framework.claim_report import ClaimReport
from bench.harness.framework.models import (
    BenchmarkConfig,
    QualityRecord,
    RunResult,
)
from bench.harness.framework.output_writer import OutputWriter
from bench.harness.framework.prompt_loader import load_prompts
from bench.harness.framework.quality_scorer import QualityScorer

if TYPE_CHECKING:
    from bench.harness.framework.backends import ModelBackend
    from bench.harness.framework.executors import ToolExecutor


# ---------------------------------------------------------------------------
# Hardware / version detection helpers
# ---------------------------------------------------------------------------

def _run_cmd(*args: str) -> str | None:
    """Run a subprocess command and return stripped stdout, or None on failure."""
    try:
        result = subprocess.run(
            list(args),
            capture_output=True,
            text=True,
            timeout=10,
        )
        if result.returncode == 0:
            return result.stdout.strip() or None
        return None
    except Exception:  # noqa: BLE001
        return None


def _detect_gpu() -> tuple[str | None, float | None]:
    """Return (gpu_name, vram_gb) via nvidia-smi, or (None, None)."""
    name = _run_cmd("nvidia-smi", "--query-gpu=name", "--format=csv,noheader")
    if name:
        name = name.splitlines()[0].strip()
    vram_gb: float | None = None
    raw_vram = _run_cmd(
        "nvidia-smi", "--query-gpu=memory.total", "--format=csv,noheader,nounits"
    )
    if raw_vram:
        try:
            vram_gb = float(raw_vram.splitlines()[0].strip()) / 1024.0
        except ValueError:
            pass
    if name:
        return name, vram_gb

    # Fallback: check /proc/driver/nvidia/gpus
    nvidia_dir = Path("/proc/driver/nvidia/gpus")
    if nvidia_dir.is_dir():
        for gpu_dir in nvidia_dir.iterdir():
            info_file = gpu_dir / "information"
            if info_file.exists():
                try:
                    text = info_file.read_text(encoding="utf-8", errors="ignore")
                    for line in text.splitlines():
                        if line.lower().startswith("model:"):
                            return line.split(":", 1)[1].strip(), None
                except OSError:
                    pass
    return None, None


def _detect_cpu() -> str | None:
    """Return CPU model string from /proc/cpuinfo, or None."""
    try:
        text = Path("/proc/cpuinfo").read_text(encoding="utf-8", errors="ignore")
        for line in text.splitlines():
            if line.lower().startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return None


def _detect_ram_gb() -> float | None:
    """Return total RAM in GB from /proc/meminfo, or None."""
    try:
        text = Path("/proc/meminfo").read_text(encoding="utf-8", errors="ignore")
        for line in text.splitlines():
            if line.lower().startswith("memtotal:"):
                parts = line.split()
                if len(parts) >= 2:
                    kb = float(parts[1])
                    return round(kb / (1024 * 1024), 1)
    except OSError:
        pass
    return None


def _detect_git_commit(repo_path: str) -> str | None:
    """Return the HEAD commit hash for repo_path, or None."""
    return _run_cmd("git", "-C", repo_path, "rev-parse", "HEAD")


def _detect_pitlane_version() -> str | None:
    """Return pitlane-mcp version string, or None."""
    out = _run_cmd("pitlane-mcp", "--version")
    return out


def _detect_ollama_version() -> str | None:
    """Return ollama version string, or None."""
    out = _run_cmd("ollama", "--version")
    return out


def _uses_semantic_mode(result: RunResult) -> bool:
    """Return True if any tool call in the result used mode='semantic'."""
    for tc in result.tool_calls:
        args = tc.arguments or {}
        if args.get("mode") == "semantic":
            return True
    return False


# ---------------------------------------------------------------------------
# BenchmarkRunner
# ---------------------------------------------------------------------------

class BenchmarkRunner:
    """Orchestrates a full benchmark run across prompts, modes, and run indices."""

    def __init__(
        self,
        repo_path: str,
        prompt_set_path: str,
        model_name: str,
        output_dir: str,
        *,
        runs_per_prompt: int = 3,
        mode: str = "both",
        max_iterations: int = 25,
        timeout_seconds: float = 300.0,
        temperature: float = 0.0,
        context_window: int = 8192,
    ) -> None:
        self.repo_path = repo_path
        self.prompt_set_path = prompt_set_path
        self.model_name = model_name
        self.output_dir = output_dir
        self.runs_per_prompt = runs_per_prompt
        self.mode = mode
        self.max_iterations = max_iterations
        self.timeout_seconds = timeout_seconds
        self.temperature = temperature
        self.context_window = context_window

    # ------------------------------------------------------------------
    # Public entry point
    # ------------------------------------------------------------------

    def run(
        self,
        backend: "ModelBackend",
        mcp_executor: "ToolExecutor",
        baseline_executor: "ToolExecutor",
    ) -> None:
        """Execute the full benchmark and write all outputs.

        Args:
            backend: LLM backend (OllamaBackend or OpenRouterBackend).
            mcp_executor: Executor for MCP mode (MCPExecutor).
            baseline_executor: Executor for baseline mode (BaselineExecutor).
        """
        writer = OutputWriter(self.output_dir)

        # Detect hardware / version info
        gpu_name, gpu_vram_gb = _detect_gpu()
        cpu_model = _detect_cpu()
        ram_gb = _detect_ram_gb()
        repo_commit = _detect_git_commit(self.repo_path)
        pitlane_version = _detect_pitlane_version()
        ollama_version = _detect_ollama_version()

        # Determine provider from backend metadata
        try:
            meta = backend.metadata()
            model_provider = meta.provider
        except Exception:  # noqa: BLE001
            model_provider = "unknown"

        config = BenchmarkConfig(
            model_name=self.model_name,
            model_provider=model_provider,
            backend_type=getattr(backend, "_base_url", None) and "ollama" or "openrouter",
            repo_path=self.repo_path,
            repo_commit=repo_commit,
            repo_clean=None,
            pitlane_version=pitlane_version,
            ollama_version=ollama_version,
            prompt_set_path=self.prompt_set_path,
            runs_per_prompt=self.runs_per_prompt,
            max_iterations=self.max_iterations,
            timeout_seconds=self.timeout_seconds,
            temperature=self.temperature,
            context_window=self.context_window,
            gpu_name=gpu_name,
            gpu_vram_gb=gpu_vram_gb,
            cpu_model=cpu_model,
            ram_gb=ram_gb,
            timestamp=datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        )
        writer.write_config(config)

        # Load prompts
        prompts = load_prompts(self.prompt_set_path)

        # Determine which modes to run
        if self.mode == "both":
            modes = ["mcp", "baseline"]
        elif self.mode in ("mcp", "baseline"):
            modes = [self.mode]
        else:
            modes = ["mcp", "baseline"]

        # Startup executors
        if "mcp" in modes:
            try:
                mcp_executor.startup(self.repo_path)
            except Exception as exc:  # noqa: BLE001
                print(f"[WARN] MCP executor startup failed: {exc}", file=sys.stderr)

        if "baseline" in modes:
            try:
                baseline_executor.startup(self.repo_path)
            except Exception as exc:  # noqa: BLE001
                print(f"[WARN] Baseline executor startup failed: {exc}", file=sys.stderr)

        loop = AgenticLoop()
        scorer = QualityScorer()

        all_results: list[RunResult] = []
        all_qualities: list[QualityRecord | None] = []

        # Per-prompt summary data: prompt_id → {mode → [quality_score]}
        summary: dict[str, dict[str, list[float]]] = {}

        for prompt_row in prompts:
            summary[prompt_row.id] = {"mcp": [], "baseline": []}

            for run_mode in modes:
                executor = mcp_executor if run_mode == "mcp" else baseline_executor

                for run_idx in range(self.runs_per_prompt):
                    result: RunResult
                    quality: QualityRecord | None = None

                    try:
                        result = loop.run(
                            prompt=prompt_row.prompt,
                            backend=backend,
                            executor=executor,
                            max_iterations=self.max_iterations,
                            timeout_seconds=self.timeout_seconds,
                            prompt_id=prompt_row.id,
                            mode=run_mode,
                            run_index=run_idx,
                        )
                    except Exception as exc:  # noqa: BLE001
                        # Record failure but continue
                        from bench.harness.framework.models import (
                            Message,
                            TokenUsage,
                        )
                        result = RunResult(
                            prompt_id=prompt_row.id,
                            mode=run_mode,
                            run_index=run_idx,
                            status="error",
                            final_answer="",
                            conversation=[],
                            tool_calls=[],
                            token_usage=TokenUsage(0, 0, 0),
                            total_context_bytes=0,
                            wall_clock_seconds=0.0,
                            error=str(exc),
                        )

                    # Score non-error runs
                    if result.status != "error" and result.final_answer:
                        try:
                            quality = scorer.score(
                                result.final_answer,
                                self.repo_path,
                                prompt_row.category,
                            )
                        except Exception as exc:  # noqa: BLE001
                            quality = None

                    writer.write_run(result, quality)
                    all_results.append(result)
                    all_qualities.append(quality)

                    if quality is not None:
                        summary[prompt_row.id][run_mode].append(quality.quality_score)

        # Shutdown executors
        if "mcp" in modes:
            try:
                mcp_executor.shutdown()
            except Exception:  # noqa: BLE001
                pass

        if "baseline" in modes:
            try:
                baseline_executor.shutdown()
            except Exception:  # noqa: BLE001
                pass

        # Write CSV summary
        writer.write_csv_summary(all_results, all_qualities)

        # Generate and write ClaimReport
        report = ClaimReport()
        report_md = report.generate(all_results, all_qualities, prompts, config)
        writer.write_claim_report(report_md)

        # Print per-prompt comparison summary to stdout
        self._print_summary(summary, modes)

    # ------------------------------------------------------------------
    # Private helpers
    # ------------------------------------------------------------------

    def _print_summary(
        self,
        summary: dict[str, dict[str, list[float]]],
        modes: list[str],
    ) -> None:
        """Print a per-prompt comparison table to stdout."""
        print("\n" + "=" * 70)
        print("BENCHMARK SUMMARY")
        print("=" * 70)
        header_parts = ["prompt_id".ljust(30)]
        for m in modes:
            header_parts.append(f"avg_quality({m})".rjust(18))
        print("  ".join(header_parts))
        print("-" * 70)

        for prompt_id, mode_scores in summary.items():
            row_parts = [prompt_id[:30].ljust(30)]
            for m in modes:
                scores = mode_scores.get(m, [])
                if scores:
                    avg = sum(scores) / len(scores)
                    row_parts.append(f"{avg:.3f}".rjust(18))
                else:
                    row_parts.append("N/A".rjust(18))
            print("  ".join(row_parts))

        print("=" * 70 + "\n")
