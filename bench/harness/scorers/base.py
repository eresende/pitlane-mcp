"""Scorer interfaces for benchmark grading."""

from __future__ import annotations

from typing import Protocol

from bench.harness.framework.models import QualityRecord, RunResult


class AnswerScorer(Protocol):
    """Minimal scorer interface used by the grading pipeline."""

    def score(self, result: RunResult, repo_path: str, category: str) -> QualityRecord | None:
        """Return a quality record for one run result, or None when unscored."""

