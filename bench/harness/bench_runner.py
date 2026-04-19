#!/usr/bin/env python3
"""Compatibility wrapper for the canonical benchmark runner."""

from __future__ import annotations

import argparse

from bench.harness.runtimes.opencode import add_opencode_arguments
from bench.harness.run import main as _run_main


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="bench_runner",
        description="Run the pitlane benchmark harness.",
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
        choices=["ollama", "openrouter", "lmstudio"],
        default="ollama",
        help="LLM backend to use (default: ollama).",
    )
    parser.add_argument(
        "--runtime",
        choices=["local", "opencode"],
        default="local",
        help="Execution runtime (default: local).",
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
    parser.add_argument(
        "--resume",
        action="store_true",
        help="Skip instances that already have per-instance artifacts.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Rerun instances even if artifacts already exist.",
    )
    parser.add_argument(
        "--prompt-id",
        dest="prompt_ids",
        action="append",
        default=[],
        help="Run only the specified prompt id. Repeat to select multiple prompts.",
    )
    parser.add_argument(
        "--skip-grade",
        action="store_true",
        help="Execute runs only and skip the grading phase.",
    )
    return add_opencode_arguments(parser)


def main(argv: list[str] | None = None) -> None:
    args = _build_parser().parse_args(argv)
    forwarded = [
        "--repo",
        args.repo,
        "--prompts",
        args.prompts,
        "--model",
        args.model,
        "--out",
        args.out,
        "--runs",
        str(args.runs),
        "--mode",
        args.mode,
        "--backend",
        args.backend,
        "--runtime",
        args.runtime,
        "--max-iterations",
        str(args.max_iterations),
        "--timeout",
        str(args.timeout),
        "--temperature",
        str(args.temperature),
        "--context-window",
        str(args.context_window),
    ]
    if args.resume:
        forwarded.append("--resume")
    if args.force:
        forwarded.append("--force")
    for prompt_id in args.prompt_ids:
        forwarded.extend(["--prompt-id", prompt_id])
    if args.skip_grade:
        forwarded.append("--skip-grade")
    for target in args.target:
        forwarded.extend(["--target", target])
    if args.agent:
        forwarded.extend(["--agent", args.agent])
    if args.title_prefix:
        forwarded.extend(["--title-prefix", args.title_prefix])
    if args.prompt_suffix:
        forwarded.extend(["--prompt-suffix", args.prompt_suffix])
    for extra_arg in args.runtime_extra_args:
        forwarded.extend(["--extra-arg", extra_arg])
    if args.dry_run:
        forwarded.append("--dry-run")
    _run_main(forwarded)


if __name__ == "__main__":
    main()
