"""Per-instance artifact paths and resume helpers."""

from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path

from bench.harness.framework.models import (
    Message,
    QualityRecord,
    RunResult,
    ToolCall,
    ToolCallRecord,
    TokenUsage,
)


def _prompt_slug(prompt_id: str) -> str:
    normalized = re.sub(r"[^A-Za-z0-9._-]+", "-", prompt_id).strip("-")
    if not normalized:
        normalized = "prompt"
    digest = hashlib.sha1(prompt_id.encode("utf-8")).hexdigest()[:8]
    return f"{normalized[:48]}-{digest}"


def instance_dir(output_dir: str | Path, prompt_id: str, mode: str, run_index: int) -> Path:
    return (
        Path(output_dir)
        / "raw"
        / _prompt_slug(prompt_id)
        / mode
        / f"run_{run_index}"
    )


def instance_is_complete(output_dir: str | Path, prompt_id: str, mode: str, run_index: int) -> bool:
    return (instance_dir(output_dir, prompt_id, mode, run_index) / "result.json").exists()


def _message_from_dict(data: dict) -> Message:
    tool_calls_data = data.get("tool_calls")
    tool_calls = None
    if tool_calls_data is not None:
        tool_calls = [ToolCall(**item) for item in tool_calls_data]
    return Message(
        role=data["role"],
        content=data["content"],
        tool_calls=tool_calls,
        tool_call_id=data.get("tool_call_id"),
    )


def _run_result_from_dict(data: dict) -> RunResult:
    return RunResult(
        prompt_id=data["prompt_id"],
        mode=data["mode"],
        run_index=int(data["run_index"]),
        status=data["status"],
        final_answer=data["final_answer"],
        conversation=[_message_from_dict(msg) for msg in data.get("conversation", [])],
        tool_calls=[ToolCallRecord(**tool_call) for tool_call in data.get("tool_calls", [])],
        token_usage=TokenUsage(**data["token_usage"]),
        total_context_bytes=int(data["total_context_bytes"]),
        wall_clock_seconds=float(data["wall_clock_seconds"]),
        error=data.get("error"),
    )


def load_instance_artifacts(
    output_dir: str | Path,
    prompt_id: str,
    mode: str,
    run_index: int,
) -> tuple[RunResult, QualityRecord | None]:
    artifact_dir = instance_dir(output_dir, prompt_id, mode, run_index)
    result_payload = json.loads((artifact_dir / "result.json").read_text(encoding="utf-8"))
    quality_path = artifact_dir / "quality.json"
    quality_payload = None
    if quality_path.exists():
        quality_payload = json.loads(quality_path.read_text(encoding="utf-8"))
    quality = QualityRecord(**quality_payload) if quality_payload is not None else None
    return _run_result_from_dict(result_payload), quality


def load_all_instance_artifacts(output_dir: str | Path) -> tuple[list[RunResult], list[QualityRecord | None]]:
    """Load all per-instance artifacts in deterministic prompt/mode/run order."""
    root = Path(output_dir) / "raw"
    if not root.exists():
        return [], []

    collected: list[tuple[RunResult, QualityRecord | None]] = []
    for result_path in sorted(root.glob("*/**/run_*/result.json")):
        artifact_dir = result_path.parent
        result_payload = json.loads(result_path.read_text(encoding="utf-8"))
        quality_path = artifact_dir / "quality.json"
        quality_payload = None
        if quality_path.exists():
            quality_payload = json.loads(quality_path.read_text(encoding="utf-8"))
        result = _run_result_from_dict(result_payload)
        quality = QualityRecord(**quality_payload) if quality_payload is not None else None
        collected.append((result, quality))

    collected.sort(key=lambda item: (item[0].prompt_id, item[0].mode, item[0].run_index))
    return [result for result, _ in collected], [quality for _, quality in collected]
