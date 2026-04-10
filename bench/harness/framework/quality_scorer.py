"""Quality scorer for benchmark answer evaluation.

Checks grounding of file paths and symbol names against the repository,
detects negation language for negative_control prompts, and computes
a quality_score in [0.0, 1.0].
"""

from __future__ import annotations

import re
from pathlib import Path

from bench.harness.framework.models import QualityRecord

# ---------------------------------------------------------------------------
# Regex patterns
# ---------------------------------------------------------------------------

# File path pattern: word chars + slashes + dots with a common extension.
# Matches things like src/foo.rs, lib/bar.py, path/to/file.ext
_FILE_PATH_RE = re.compile(
    r"\b(?:[\w.-]+/)+[\w.-]+\.(?:py|rs|go|ts|js|java|c|cpp|h|hpp|rb|cs|swift|kt|lua|sh|md|toml|yaml|yml|json|txt)\b"
)

# CamelCase symbol: starts with uppercase, has at least one more uppercase letter
_CAMEL_CASE_RE = re.compile(r"\b[A-Z][a-z]+(?:[A-Z][a-z]*)+\b")

# snake_case symbol: lowercase words joined by underscores (at least one underscore)
_SNAKE_CASE_RE = re.compile(r"\b[a-z][a-z0-9]*(?:_[a-z][a-z0-9]*)+\b")

# Negation phrases for negative_control category
_NEGATION_PHRASES = [
    "does not exist",
    "doesn't exist",
    "not found",
    "no such",
    "cannot find",
    "can't find",
    "not present",
    "does not have",
    "doesn't have",
    "no file",
    "no symbol",
    "not exist",
]

# Source file extensions to search for symbols
_SOURCE_EXTENSIONS = {
    ".py", ".rs", ".go", ".ts", ".js", ".java", ".c", ".cpp", ".h", ".hpp",
    ".rb", ".cs", ".swift", ".kt", ".lua", ".sh",
}


class QualityScorer:
    """Evaluates answer quality by checking grounding against a repository."""

    def score(self, answer: str, repo_path: str, category: str) -> QualityRecord:
        """Score an answer against the repository.

        Args:
            answer: The model's answer text.
            repo_path: Absolute or relative path to the repository root.
            category: The prompt category (e.g. "negative_control").

        Returns:
            A QualityRecord with grounding counts and quality_score.
        """
        repo = Path(repo_path)

        # --- Extract file paths and check existence ---
        file_paths = _FILE_PATH_RE.findall(answer)
        grounded_files = sum(1 for fp in file_paths if (repo / fp).exists())

        # --- Extract symbols and check presence in source files ---
        camel = _CAMEL_CASE_RE.findall(answer)
        snake = _SNAKE_CASE_RE.findall(answer)
        symbols = list(dict.fromkeys(camel + snake))  # deduplicate, preserve order

        grounded_symbols = 0
        if symbols:
            grounded_symbols = _count_grounded_symbols(symbols, repo)

        # --- Ungrounded references ---
        total_extracted = len(file_paths) + len(symbols)
        total_grounded = grounded_files + grounded_symbols
        ungrounded = max(0, total_extracted - total_grounded)

        # --- Negative control check ---
        is_negative_correct: bool | None = None
        if category == "negative_control":
            answer_lower = answer.lower()
            is_negative_correct = any(phrase in answer_lower for phrase in _NEGATION_PHRASES)

        # --- Quality score ---
        if grounded_files == 0 and grounded_symbols == 0:
            quality_score = 0.0
        else:
            if total_extracted == 0:
                quality_score = 0.0
            else:
                quality_score = min(1.0, total_grounded / total_extracted)

        return QualityRecord(
            grounded_files_count=grounded_files,
            grounded_symbols_count=grounded_symbols,
            ungrounded_references_count=ungrounded,
            is_negative_correct=is_negative_correct,
            quality_score=quality_score,
        )


def _count_grounded_symbols(symbols: list[str], repo: Path) -> int:
    """Count how many symbols appear in any source file under repo."""
    if not symbols:
        return 0

    # Build a combined pattern for all symbols (word-boundary match)
    # Search source files for each symbol
    source_files = [
        f for f in repo.rglob("*")
        if f.is_file() and f.suffix in _SOURCE_EXTENSIONS
    ]

    found: set[str] = set()
    for src_file in source_files:
        if len(found) == len(symbols):
            break
        try:
            text = src_file.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        for sym in symbols:
            if sym not in found and re.search(r"\b" + re.escape(sym) + r"\b", text):
                found.add(sym)

    return len(found)
