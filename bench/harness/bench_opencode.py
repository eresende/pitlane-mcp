#!/usr/bin/env python3
"""Compatibility wrapper for the shared OpenCode benchmark runtime."""

from __future__ import annotations

import argparse
from pathlib import Path

from bench.harness.runtimes.base import RuntimeRequest
from bench.harness.runtimes.opencode import add_opencode_arguments, execute_opencode_request


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Benchmark OpenCode across multiple targets.")
    parser.add_argument("--repo", required=True, help="Path to repository to benchmark in.")
    parser.add_argument("--prompts", required=True, help="JSONL file with benchmark prompts.")
    parser.add_argument("--model", default="openai/gpt-5.4", help="Model override, e.g. openai/gpt-5.4.")
    parser.add_argument("--runs", type=int, default=1, help="Number of repeats per prompt/target.")
    parser.add_argument("--out", default="benchmark_out", help="Output directory.")
    add_opencode_arguments(parser)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    request = RuntimeRequest(
        repo_path=str(Path(args.repo).resolve()),
        prompt_set_path=str(Path(args.prompts).resolve()),
        model_name=args.model,
        output_dir=str(Path(args.out).resolve()),
        runs_per_prompt=args.runs,
        mode="both",
        max_iterations=1,
        timeout_seconds=0.0,
        temperature=0.0,
        context_window=0,
        runtime_type="opencode",
        suite_id=f"adhoc-{Path(args.prompts).stem}",
        suite_manifest_path=None,
        scorer_version="manual",
        resume=False,
        force=False,
        prompt_ids=[],
        backend_type="openai",
        target_specs=args.target,
        agent=args.agent,
        title_prefix=args.title_prefix,
        prompt_suffix=args.prompt_suffix,
        runtime_extra_args=args.runtime_extra_args,
        dry_run=args.dry_run,
    )
    return execute_opencode_request(
        request,
        agents_md_path=Path(__file__).parent / "AGENTS.md",
    )


if __name__ == "__main__":
    raise SystemExit(main())
