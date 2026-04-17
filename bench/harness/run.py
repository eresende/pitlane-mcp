#!/usr/bin/env python3
"""Canonical CLI entrypoint for benchmark runs."""

from __future__ import annotations

import argparse
import logging
import shutil
import sys
from pathlib import Path

from bench.harness.manifest import resolve_suite_paths


def _check_pitlane_on_path() -> None:
    if shutil.which("pitlane-mcp") is None:
        print(
            "ERROR: pitlane-mcp not found on PATH.\n"
            "Install it from https://github.com/eresende/pitlane-mcp",
            file=sys.stderr,
        )
        sys.exit(1)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="bench.harness.run",
        description="Run the pitlane benchmark harness.",
    )
    parser.add_argument("--suite", help="Path to a suite manifest JSON file.")
    parser.add_argument("--repo", help="Path to the target repository.")
    parser.add_argument("--prompts", help="Path to the JSONL prompt set.")
    parser.add_argument("--model", required=True, help="Model name (e.g. qwen3:8b).")
    parser.add_argument("--out", required=True, help="Output directory for results.")
    parser.add_argument(
        "--runs",
        type=int,
        default=None,
        help="Number of runs per prompt. Defaults to the suite manifest or 3.",
    )
    parser.add_argument(
        "--mode",
        choices=["both", "mcp", "baseline"],
        default=None,
        help="Which mode(s) to run. Defaults to the suite manifest or both.",
    )
    parser.add_argument(
        "--backend",
        choices=["ollama", "openrouter"],
        default="ollama",
        help="LLM backend to use (default: ollama).",
    )
    parser.add_argument(
        "--runtime",
        choices=["local"],
        default="local",
        help="Execution runtime (default: local).",
    )
    parser.add_argument(
        "--max-iterations",
        type=int,
        default=None,
        dest="max_iterations",
        help="Max agentic loop iterations per run. Defaults to the suite manifest or 25.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=None,
        help="Wall-clock timeout in seconds per run. Defaults to the suite manifest or 300.",
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
    parser.add_argument(
        "--resume",
        action="store_true",
        help="Skip instances with existing per-instance artifacts.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Rerun instances even when per-instance artifacts already exist.",
    )
    parser.add_argument(
        "--prompt-id",
        dest="prompt_ids",
        action="append",
        default=[],
        help="Run only the specified prompt id. Repeat to select multiple prompts.",
    )
    return parser


def _resolve_inputs(args: argparse.Namespace) -> tuple[str, str, str, str, str]:
    if args.suite:
        suite, suite_manifest_path, repo_path, prompts_path = resolve_suite_paths(
            args.suite,
            repo_override=args.repo,
            prompts_override=args.prompts,
        )
        suite_id = suite.suite_id
        runs = args.runs if args.runs is not None else suite.defaults.runs
        mode = args.mode or suite.defaults.mode
        max_iterations = (
            args.max_iterations
            if args.max_iterations is not None
            else suite.defaults.max_iterations
        )
        timeout = args.timeout if args.timeout is not None else suite.defaults.timeout_seconds
        args.runs = runs
        args.mode = mode
        args.max_iterations = max_iterations
        args.timeout = timeout
        args.repo = str(repo_path)
        args.prompts = str(prompts_path)
        args.suite_manifest_path = str(suite_manifest_path)
        args.suite_id = suite_id
        args.scorer_version = suite.scorer.version
        return str(repo_path), str(prompts_path), str(suite_manifest_path), suite_id, suite.scorer.version

    if not args.repo or not args.prompts:
        raise SystemExit("--repo and --prompts are required when --suite is not provided")

    args.runs = args.runs if args.runs is not None else 3
    args.mode = args.mode or "both"
    args.max_iterations = args.max_iterations if args.max_iterations is not None else 25
    args.timeout = args.timeout if args.timeout is not None else 300.0
    args.repo = str(Path(args.repo).resolve())
    args.prompts = str(Path(args.prompts).resolve())
    args.suite_manifest_path = None
    args.suite_id = f"adhoc-{Path(args.prompts).stem}"
    args.scorer_version = "v1"
    return args.repo, args.prompts, "", args.suite_id, args.scorer_version


def main(argv: list[str] | None = None) -> None:
    parser = _build_parser()
    args = parser.parse_args(argv)
    _resolve_inputs(args)

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s  %(levelname)-7s  %(message)s",
        datefmt="%H:%M:%S",
        stream=sys.stderr,
    )

    if args.mode in ("mcp", "both"):
        _check_pitlane_on_path()

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

    from bench.harness.framework.benchmark_runner import BenchmarkRunner
    from bench.harness.framework.executors import BaselineExecutor
    from bench.harness.framework.mcp_executor import MCPExecutor

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
        runtime_type=args.runtime,
        suite_id=args.suite_id,
        suite_manifest_path=args.suite_manifest_path,
        scorer_version=args.scorer_version,
        resume=args.resume,
        force=args.force,
        prompt_ids=args.prompt_ids,
    )
    runner.run(backend, MCPExecutor(), BaselineExecutor())


if __name__ == "__main__":
    main()
