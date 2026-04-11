#!/usr/bin/env python3
"""CLI entry point for the real-world benchmark framework.

Usage:
    python bench_runner.py \\
        --repo /path/to/repo \\
        --prompts prompts.ripgrep.jsonl \\
        --model qwen3:8b \\
        --out results/ripgrep-qwen3 \\
        --runs 3 \\
        --mode both \\
        --backend ollama
"""

from __future__ import annotations

import argparse
import logging
import shutil
import sys

# Configure logging to stderr with timestamps
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s  %(levelname)-7s  %(message)s",
    datefmt="%H:%M:%S",
    stream=sys.stderr,
)


def _check_pitlane_on_path() -> None:
    """Raise SystemExit if pitlane-mcp is not found on PATH."""
    if shutil.which("pitlane-mcp") is None:
        print(
            "ERROR: pitlane-mcp not found on PATH.\n"
            "Install it from https://github.com/eresende/pitlane-mcp",
            file=sys.stderr,
        )
        sys.exit(1)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="bench_runner",
        description="Run the pitlane-mcp real-world benchmark.",
    )
    parser.add_argument("--repo", required=True, help="Path to the target repository.")
    parser.add_argument("--prompts", required=True, help="Path to the JSONL prompt set.")
    parser.add_argument("--model", required=True, help="Model name (e.g. qwen3:8b).")
    parser.add_argument("--out", required=True, help="Output directory for results.")
    parser.add_argument(
        "--runs", type=int, default=3, help="Number of runs per prompt (default: 3)."
    )
    parser.add_argument(
        "--mode",
        choices=["both", "mcp", "baseline"],
        default="both",
        help="Which mode(s) to run (default: both).",
    )
    parser.add_argument(
        "--backend",
        choices=["ollama", "openrouter"],
        default="ollama",
        help="LLM backend to use (default: ollama).",
    )
    parser.add_argument(
        "--max-iterations",
        type=int,
        default=25,
        dest="max_iterations",
        help="Max agentic loop iterations per run (default: 25).",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=300.0,
        help="Wall-clock timeout in seconds per run (default: 300).",
    )
    parser.add_argument(
        "--temperature",
        type=float,
        default=0.0,
        help="Sampling temperature (default: 0.0).",
    )
    parser.add_argument(
        "--context-window",
        type=int,
        default=8192,
        dest="context_window",
        help="Context window size in tokens (default: 8192).",
    )
    return parser


def main(argv: list[str] | None = None) -> None:
    parser = _build_parser()
    args = parser.parse_args(argv)

    # Validate pitlane-mcp on PATH for modes that need it
    if args.mode in ("mcp", "both"):
        _check_pitlane_on_path()

    # Build backend
    if args.backend == "ollama":
        from bench.harness.framework.backends import OllamaBackend
        backend = OllamaBackend(
            model=args.model,
            temperature=args.temperature,
            num_ctx=args.context_window,
        )
    else:
        from bench.harness.framework.backends import OpenRouterBackend
        backend = OpenRouterBackend(
            model=args.model,
            temperature=args.temperature,
        )

    # Build executors
    from bench.harness.framework.mcp_executor import MCPExecutor
    from bench.harness.framework.executors import BaselineExecutor

    mcp_executor = MCPExecutor()
    baseline_executor = BaselineExecutor()

    # Build and run BenchmarkRunner
    from bench.harness.framework.benchmark_runner import BenchmarkRunner

    runner = BenchmarkRunner(
        repo_path=args.repo,
        prompt_set_path=args.prompts,
        model_name=args.model,
        output_dir=args.out,
        runs_per_prompt=args.runs,
        mode=args.mode,
        max_iterations=args.max_iterations,
        timeout_seconds=args.timeout,
        temperature=args.temperature,
        context_window=args.context_window,
    )

    runner.run(backend, mcp_executor, baseline_executor)


if __name__ == "__main__":
    main()
