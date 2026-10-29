"""Quality scorer for benchmark answer evaluation.

Checks grounding of file paths and symbol names against the repository,
detects negation language for negative_control prompts, and computes
a quality_score in [0.0, 1.0].

Score version 2 corrects two measurement defects of the original
precision-only score:

1. A file named without a directory prefix (``dir.rs``) used to be invisible,
   so a correct answer naming real files could score 0.0. Bare names are now
   resolved by basename when exactly one repository file has that name. An
   ambiguous bare name is counted but never credited as grounded.
2. A single true reference used to score 1.0, which rewarded answers that cited
   almost nothing. ``quality_score`` now applies a citation floor: precision is
   scaled by ``min(1, grounded_citations / MIN_GROUNDED_CITATIONS)``. The
   unmodified precision remains available as ``grounding_precision``.
"""

from __future__ import annotations

import re
from pathlib import Path

from bench.harness.framework.models import QualityRecord

# ---------------------------------------------------------------------------
# Regex patterns
# ---------------------------------------------------------------------------

SCORE_VERSION = "grounding-v2"

# A perfect score requires at least this many grounded citations, so a one-file
# answer is not automatically treated as complete.
MIN_GROUNDED_CITATIONS = 4

_EXTENSIONS = (
    "py|rs|go|ts|js|java|c|cpp|h|hpp|rb|cs|swift|kt|lua|sh|md|toml|yaml|yml|json|txt"
)

# File path pattern: word chars + slashes + dots with a common extension.
# Matches things like src/foo.rs, lib/bar.py, path/to/file.ext
_FILE_PATH_RE = re.compile(r"\b(?:[\w.-]+/)+[\w.-]+\.(?:" + _EXTENSIONS + r")\b")

# Bare file name: same extension set, but not preceded by a path separator,
# word character or dot, so the tail of a longer path is not double counted.
_BARE_FILE_RE = re.compile(r"(?<![\w./-])([\w-]+\.(?:" + _EXTENSIONS + r")\b)")

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
        by_basename, source_files = _repository_index(repo)

        # --- Extract file references and resolve them to repository files ---
        references = _extract_file_references(answer)
        grounded_files: set[str] = set()
        ambiguous = 0
        for reference in references:
            resolved, is_ambiguous = _resolve_file(reference, repo, by_basename)
            if resolved is not None:
                grounded_files.add(resolved)
            ambiguous += is_ambiguous
        unresolved_files = len(references) - len(grounded_files) - ambiguous

        # --- Extract symbols and check presence in source files ---
        symbols = _extract_symbols(answer)
        grounded_symbols = _count_grounded_symbols(symbols, source_files)

        # --- Ungrounded references ---
        total_extracted = len(references) + len(symbols)
        total_grounded = len(grounded_files) + grounded_symbols
        ungrounded = unresolved_files + ambiguous + (len(symbols) - grounded_symbols)

        # --- Negative control check ---
        is_negative_correct: bool | None = None
        if category == "negative_control":
            answer_lower = answer.lower()
            is_negative_correct = any(phrase in answer_lower for phrase in _NEGATION_PHRASES)

        # --- Quality score: precision scaled by a citation floor ---
        precision = min(1.0, total_grounded / total_extracted) if total_extracted else 0.0
        citation_factor = min(1.0, total_grounded / MIN_GROUNDED_CITATIONS)
        quality_score = precision * citation_factor

        return QualityRecord(
            grounded_files_count=len(grounded_files),
            grounded_symbols_count=grounded_symbols,
            ungrounded_references_count=ungrounded,
            is_negative_correct=is_negative_correct,
            quality_score=quality_score,
            grounding_precision=precision,
            citation_count=total_grounded,
            ambiguous_references_count=ambiguous,
            score_version=SCORE_VERSION,
        )


def _repository_index(repo: Path) -> tuple[dict[str, list[str]], list[Path]]:
    """Return per-basename file locations and the source files to scan.

    One traversal serves both basename resolution and symbol lookup, so the
    scorer reads the repository once per answer.
    """
    files = [p for p in repo.rglob("*") if p.is_file()]
    by_basename: dict[str, list[str]] = {}
    for path in files:
        try:
            by_basename.setdefault(path.name, []).append(str(path.relative_to(repo)))
        except ValueError:
            continue
    return by_basename, [p for p in files if p.suffix in _SOURCE_EXTENSIONS]


def _extract_file_references(answer: str) -> list[str]:
    """Extract distinct file references, preserving first-seen order."""
    return list(dict.fromkeys(_FILE_PATH_RE.findall(answer) + _BARE_FILE_RE.findall(answer)))


def _extract_symbols(answer: str) -> list[str]:
    """Extract distinct CamelCase and snake_case candidates."""
    camel = _CAMEL_CASE_RE.findall(answer)
    snake = _SNAKE_CASE_RE.findall(answer)
    return list(dict.fromkeys(camel + snake))


def _resolve_file(reference: str, repo: Path, by_basename: dict[str, list[str]]) -> tuple[str | None, int]:
    """Resolve one reference to a repository-relative path.

    Returns (resolved_path_or_None, ambiguous_flag). A bare name matching more
    than one file is ambiguous: it is reported and never credited.
    """
    if "/" in reference:
        return (reference, 0) if (repo / reference).is_file() else (None, 0)
    if (repo / reference).is_file():
        return reference, 0
    matches = by_basename.get(reference, [])
    if len(matches) == 1:
        return matches[0], 0
    return None, 1 if matches else 0


def _count_grounded_symbols(symbols: list[str], source_files: list[Path]) -> int:
    """Count how many symbols appear in any of the given source files."""
    if not symbols:
        return 0
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
