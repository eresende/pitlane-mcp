"""OutputWriter — writes benchmark results to disk.

Produces:
  config.json          — full BenchmarkConfig serialized
  run_manifest.json    — immutable run identity
  results.jsonl        — derived aggregate of persisted RunResult artifacts
  results.csv          — derived flattened summary CSV
  claim_report.md      — derived Markdown claim report
  raw/<prompt_slug>/<mode>/run_<n>/
      result.json
      quality.json
      conversation.json
      tool_calls.json
"""

from __future__ import annotations

import csv
import dataclasses
import json
import os
from pathlib import Path
from typing import Any

from bench.harness.resume import instance_dir
from bench.harness.framework.models import BenchmarkConfig, Message, QualityRecord, RunResult, ToolCallRecord, TokenUsage
from bench.harness.schemas import RunManifest


# ---------------------------------------------------------------------------
# Serialization helpers
# ---------------------------------------------------------------------------

def _serialize_message(msg: Message) -> dict:
    d: dict[str, Any] = {"role": msg.role, "content": msg.content}
    if msg.tool_calls is not None:
        d["tool_calls"] = [
            {"id": tc.id, "name": tc.name, "arguments": tc.arguments}
            for tc in msg.tool_calls
        ]
    if msg.tool_call_id is not None:
        d["tool_call_id"] = msg.tool_call_id
    return d


def _serialize_tool_call_record(rec: ToolCallRecord) -> dict:
    return {
        "iteration": rec.iteration,
        "tool_name": rec.tool_name,
        "arguments": rec.arguments,
        "result_bytes": rec.result_bytes,
        "latency_ms": rec.latency_ms,
    }


def _serialize_token_usage(usage: TokenUsage) -> dict:
    return {
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
    }


def _run_result_to_dict(result: RunResult) -> dict:
    """Serialize a RunResult to a flat-ish dict (nested objects expanded)."""
    return {
        "prompt_id": result.prompt_id,
        "mode": result.mode,
        "run_index": result.run_index,
        "status": result.status,
        "final_answer": result.final_answer,
        "conversation": [_serialize_message(m) for m in result.conversation],
        "tool_calls": [_serialize_tool_call_record(tc) for tc in result.tool_calls],
        "token_usage": _serialize_token_usage(result.token_usage),
        "total_context_bytes": result.total_context_bytes,
        "wall_clock_seconds": result.wall_clock_seconds,
        "error": result.error,
    }


def _quality_record_to_dict(quality: QualityRecord) -> dict:
    return dataclasses.asdict(quality)


def _run_result_to_flat_dict(
    result: RunResult, quality: QualityRecord | None
) -> dict:
    """Flatten a RunResult + optional QualityRecord into a single CSV row dict."""
    row: dict[str, Any] = {
        "prompt_id": result.prompt_id,
        "mode": result.mode,
        "run_index": result.run_index,
        "status": result.status,
        "total_context_bytes": result.total_context_bytes,
        "wall_clock_seconds": result.wall_clock_seconds,
        "prompt_tokens": result.token_usage.prompt_tokens,
        "completion_tokens": result.token_usage.completion_tokens,
        "total_tokens": result.token_usage.total_tokens,
        "tool_call_count": len(result.tool_calls),
        "error": result.error or "",
    }
    if quality is not None:
        row["grounded_files_count"] = quality.grounded_files_count
        row["grounded_symbols_count"] = quality.grounded_symbols_count
        row["ungrounded_references_count"] = quality.ungrounded_references_count
        row["is_negative_correct"] = (
            "" if quality.is_negative_correct is None else str(quality.is_negative_correct)
        )
        row["quality_score"] = quality.quality_score
    else:
        row["grounded_files_count"] = ""
        row["grounded_symbols_count"] = ""
        row["ungrounded_references_count"] = ""
        row["is_negative_correct"] = ""
        row["quality_score"] = ""
    return row


_CSV_HEADERS = [
    "prompt_id",
    "mode",
    "run_index",
    "status",
    "total_context_bytes",
    "wall_clock_seconds",
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "tool_call_count",
    "error",
    "grounded_files_count",
    "grounded_symbols_count",
    "ungrounded_references_count",
    "is_negative_correct",
    "quality_score",
]


# ---------------------------------------------------------------------------
# OutputWriter
# ---------------------------------------------------------------------------

class OutputWriter:
    """Writes benchmark outputs to a directory."""

    def __init__(self, output_dir: str) -> None:
        self._out = Path(output_dir)
        self._out.mkdir(parents=True, exist_ok=True)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def write_config(self, config: BenchmarkConfig) -> None:
        """Write config.json with the full BenchmarkConfig."""
        config_path = self._out / "config.json"
        config_path.write_text(
            json.dumps(config.to_dict(), indent=2, ensure_ascii=False),
            encoding="utf-8",
        )

    def write_run_manifest(self, manifest: RunManifest) -> None:
        """Write run_manifest.json with the immutable benchmark identity."""
        manifest_path = self._out / "run_manifest.json"
        manifest_path.write_text(
            json.dumps(manifest.to_dict(), indent=2, ensure_ascii=False),
            encoding="utf-8",
        )

    def write_run(self, result: RunResult) -> None:
        """Write execution artifacts for one benchmark instance."""
        row = _run_result_to_dict(result)

        raw_dir = instance_dir(self._out, result.prompt_id, result.mode, result.run_index)
        raw_dir.mkdir(parents=True, exist_ok=True)

        result_path = raw_dir / "result.json"
        result_path.write_text(
            json.dumps(row, indent=2, ensure_ascii=False),
            encoding="utf-8",
        )

        conversation_path = raw_dir / "conversation.json"
        conversation_path.write_text(
            json.dumps(
                [_serialize_message(m) for m in result.conversation],
                indent=2,
                ensure_ascii=False,
            ),
            encoding="utf-8",
        )

        tool_calls_path = raw_dir / "tool_calls.json"
        tool_calls_path.write_text(
            json.dumps(
                [_serialize_tool_call_record(tc) for tc in result.tool_calls],
                indent=2,
                ensure_ascii=False,
            ),
            encoding="utf-8",
        )

    def write_quality(
        self,
        prompt_id: str,
        mode: str,
        run_index: int,
        quality: QualityRecord | None,
    ) -> None:
        """Write or clear the grading artifact for one benchmark instance."""
        raw_dir = instance_dir(self._out, prompt_id, mode, run_index)
        raw_dir.mkdir(parents=True, exist_ok=True)
        quality_path = raw_dir / "quality.json"
        if quality is None:
            if quality_path.exists():
                quality_path.unlink()
            return
        quality_path.write_text(
            json.dumps(_quality_record_to_dict(quality), indent=2, ensure_ascii=False),
            encoding="utf-8",
        )

    def write_results_jsonl(self, results: list[RunResult]) -> None:
        """Rewrite results.jsonl from the current authoritative run list."""
        jsonl_path = self._out / "results.jsonl"
        with jsonl_path.open("w", encoding="utf-8") as f:
            for result in results:
                f.write(json.dumps(_run_result_to_dict(result), ensure_ascii=False) + "\n")

    def write_csv_summary(
        self,
        results: list[RunResult],
        qualities: list[QualityRecord | None],
    ) -> None:
        """Write results.csv with one row per RunResult."""
        csv_path = self._out / "results.csv"
        rows = [
            _run_result_to_flat_dict(r, q)
            for r, q in zip(results, qualities)
        ]
        with csv_path.open("w", newline="", encoding="utf-8") as f:
            writer = csv.DictWriter(f, fieldnames=_CSV_HEADERS)
            writer.writeheader()
            writer.writerows(rows)

    def write_claim_report(self, report_md: str) -> None:
        """Write claim_report.md."""
        report_path = self._out / "claim_report.md"
        report_path.write_text(report_md, encoding="utf-8")
