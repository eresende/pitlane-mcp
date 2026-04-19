"""Default quality scorer used by the benchmark harness."""

from __future__ import annotations

from bench.harness.framework.models import QualityRecord, RunResult
from bench.harness.framework.quality_scorer import QualityScorer
from bench.harness.scorers.base import AnswerScorer


class DefaultAnswerScorer(AnswerScorer):
    """Adapter that scores completed answers with the existing quality scorer."""

    def __init__(self) -> None:
        self._scorer = QualityScorer()

    def score(self, result: RunResult, repo_path: str, category: str) -> QualityRecord | None:
        if result.status == "error" or not result.final_answer:
            return None
        return self._scorer.score(result.final_answer, repo_path, category)

