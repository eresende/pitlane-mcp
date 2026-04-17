"""Suite loading and immutable run-manifest construction."""

from __future__ import annotations

import datetime
import hashlib
import json
import subprocess
import uuid
from pathlib import Path

from bench.harness.schemas import RunManifest, SuiteManifest


def _run_cmd(*args: str) -> str | None:
    try:
        result = subprocess.run(
            list(args),
            capture_output=True,
            text=True,
            timeout=10,
        )
    except Exception:  # noqa: BLE001
        return None
    if result.returncode != 0:
        return None
    return result.stdout.strip() or None


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(8192):
            digest.update(chunk)
    return digest.hexdigest()


def _detect_git_commit(repo_path: Path) -> str | None:
    return _run_cmd("git", "-C", str(repo_path), "rev-parse", "HEAD")


def _detect_git_clean(repo_path: Path) -> bool | None:
    status = _run_cmd("git", "-C", str(repo_path), "status", "--porcelain")
    if status is None:
        return None
    return status == ""


def _detect_pitlane_version() -> str | None:
    return _run_cmd("pitlane-mcp", "--version")


def _detect_ollama_version() -> str | None:
    return _run_cmd("ollama", "--version")


def _resolve_path(raw_path: str, relative_to: Path | None = None) -> Path:
    path = Path(raw_path)
    if path.is_absolute():
        return path.resolve()
    if relative_to is not None:
        return (relative_to / path).resolve()
    return path.resolve()


def load_suite_manifest(path: str) -> tuple[SuiteManifest, Path]:
    manifest_path = Path(path).resolve()
    payload = json.loads(manifest_path.read_text(encoding="utf-8"))
    return SuiteManifest.from_dict(payload), manifest_path


def build_run_manifest(
    *,
    suite_id: str,
    repo_path: str,
    prompt_set_path: str,
    model_name: str,
    backend_type: str,
    runtime_type: str,
    mode: str,
    runs_per_prompt: int,
    max_iterations: int,
    timeout_seconds: float,
    temperature: float,
    context_window: int,
    scorer_version: str = "v1",
    prompt_filter: list[str] | None = None,
    resume_enabled: bool = False,
    force_enabled: bool = False,
    suite_manifest_path: str | None = None,
) -> RunManifest:
    repo = Path(repo_path).resolve()
    prompt_set = Path(prompt_set_path).resolve()
    harness_root = Path(__file__).resolve().parents[2]
    suite_manifest = Path(suite_manifest_path).resolve() if suite_manifest_path else None
    created_at = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    return RunManifest(
        schema_version="1",
        run_id=uuid.uuid4().hex,
        suite_id=suite_id,
        suite_manifest_path=str(suite_manifest) if suite_manifest else None,
        suite_manifest_sha256=_sha256_file(suite_manifest) if suite_manifest else None,
        repo_path=str(repo),
        repo_commit=_detect_git_commit(repo),
        repo_clean=_detect_git_clean(repo),
        prompt_set_path=str(prompt_set),
        prompt_set_sha256=_sha256_file(prompt_set),
        model_name=model_name,
        backend_type=backend_type,
        runtime_type=runtime_type,
        mode=mode,
        runs_per_prompt=runs_per_prompt,
        max_iterations=max_iterations,
        timeout_seconds=timeout_seconds,
        temperature=temperature,
        context_window=context_window,
        harness_commit=_detect_git_commit(harness_root),
        harness_clean=_detect_git_clean(harness_root),
        pitlane_version=_detect_pitlane_version(),
        ollama_version=_detect_ollama_version(),
        scorer_version=scorer_version,
        prompt_filter=list(prompt_filter or []),
        resume_enabled=resume_enabled,
        force_enabled=force_enabled,
        created_at=created_at,
    )


def resolve_suite_paths(
    suite_path: str,
    *,
    repo_override: str | None = None,
    prompts_override: str | None = None,
) -> tuple[SuiteManifest, Path, Path, Path]:
    suite, suite_manifest_path = load_suite_manifest(suite_path)
    suite_root = suite_manifest_path.parent
    repo_path = _resolve_path(repo_override or suite.repo.path, suite_root)
    prompts_path = _resolve_path(prompts_override or suite.prompts.path, suite_root)
    return suite, suite_manifest_path, repo_path, prompts_path

