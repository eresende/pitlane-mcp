"""Load and validate JSONL prompt files into PromptRow objects."""

from __future__ import annotations

import json
from pathlib import Path

from bench.harness.framework.models import PromptRow

VALID_CATEGORIES = frozenset({
    "symbol_grounding",
    "find_usages",
    "find_tests",
    "architecture",
    "negative_control",
    "token_efficiency_probe",
    "search_quality_probe",
    "graph_navigation_probe",
    "semantic_search_probe",
})

REQUIRED_FIELDS = ("id", "category", "prompt")


class PromptValidationError(Exception):
    """Raised when a JSONL prompt row is missing a required field."""

    def __init__(self, line_number: int, field_name: str, message: str | None = None) -> None:
        self.line_number = line_number
        self.field_name = field_name
        msg = message or f"Line {line_number}: missing required field '{field_name}'"
        super().__init__(msg)


def load_prompts(path: str) -> list[PromptRow]:
    """Load a JSONL prompt file and return validated PromptRow objects.

    Blank lines are skipped. Each non-blank line must be valid JSON with
    at least the required fields: id, category, prompt.

    Raises:
        PromptValidationError: If a required field is missing, with the
            line number and field name attached.
        FileNotFoundError: If the file does not exist.
        json.JSONDecodeError: If a line contains invalid JSON.
    """
    file_path = Path(path)
    rows: list[PromptRow] = []

    with file_path.open("r", encoding="utf-8") as fh:
        for line_number, raw_line in enumerate(fh, start=1):
            line = raw_line.strip()
            if not line:
                continue

            data = json.loads(line)

            for field in REQUIRED_FIELDS:
                if field not in data:
                    raise PromptValidationError(line_number, field)

            rows.append(
                PromptRow(
                    id=data["id"],
                    category=data["category"],
                    prompt=data["prompt"],
                    prompt_suffix=data.get("prompt_suffix"),
                    claim=data.get("claim"),
                )
            )

    return rows
