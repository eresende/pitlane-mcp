#!/usr/bin/env python3
"""Grade persisted benchmark run artifacts and regenerate derived outputs."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from bench.harness.framework.claim_report import ClaimReport
from bench.harness.framework.models import BenchmarkConfig, PromptRow, QualityRecord, RunResult
from bench.harness.framework.output_writer import OutputWriter
from bench.harness.framework.prompt_loader import load_prompts
from bench.harness.resume import load_all_instance_artifacts
from bench.harness.scorers.default import DefaultAnswerScorer


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="bench.harness.grade",
        description="Grade persisted benchmark run artifacts and regenerate summaries.",
    )
    parser.add_argument(
        "--run",
        required=True,
        help="Output directory containing run_manifest.json and raw artifacts.",
    )
    parser.add_argument(
        "--scorer",
        default="default",
        choices=["default", "v1"],
        help="Scorer implementation to use (default: default).",
    )
    return parser


def _load_config(run_dir: Path) -> BenchmarkConfig:
    payload = json.loads((run_dir / "config.json").read_text(encoding="utf-8"))
    return BenchmarkConfig.from_dict(payload)


def _load_prompts(config: BenchmarkConfig) -> list[PromptRow]:
    return load_prompts(config.prompt_set_path)


def _build_prompt_lookup(prompts: list[PromptRow]) -> dict[str, PromptRow]:
    return {prompt.id: prompt for prompt in prompts}


def _load_scorer(name: str) -> DefaultAnswerScorer:
    if name not in {"default", "v1"}:
        raise ValueError(f"Unsupported scorer: {name}")
    return DefaultAnswerScorer()


def grade_run(run_dir: str | Path, *, scorer_name: str = "default") -> tuple[list[RunResult], list[QualityRecord | None]]:
    run_path = Path(run_dir).resolve()
    config = _load_config(run_path)
    prompts = _load_prompts(config)
    prompt_lookup = _build_prompt_lookup(prompts)
    results, _ = load_all_instance_artifacts(run_path)
    scorer = _load_scorer(scorer_name)

    qualities: list[QualityRecord | None] = []
    writer = OutputWriter(str(run_path))

    for result in results:
        prompt = prompt_lookup.get(result.prompt_id)
        category = prompt.category if prompt is not None else ""
        quality = scorer.score(result, config.repo_path, category)
        writer.write_quality(result.prompt_id, result.mode, result.run_index, quality)
        qualities.append(quality)

    writer.write_results_jsonl(results)
    writer.write_csv_summary(results, qualities)
    report = ClaimReport()
    writer.write_claim_report(report.generate(results, qualities, prompts, config))
    return results, qualities


def main(argv: list[str] | None = None) -> None:
    args = _build_parser().parse_args(argv)
    results, qualities = grade_run(args.run, scorer_name=args.scorer)

    scored = sum(1 for quality in qualities if quality is not None)
    print(f"Graded {len(results)} runs ({scored} scored) in {Path(args.run).resolve()}")


if __name__ == "__main__":
    main()
