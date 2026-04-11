"""Tool executor interfaces and baseline implementation.

Defines the ToolExecutor Protocol and BaselineExecutor which provides
three simple file-system tools: read_file, grep_search, list_directory.
"""

from __future__ import annotations

import os
import re
import time
from pathlib import Path
from typing import Protocol

from bench.harness.framework.models import ToolDef, ToolResult


class ToolExecutor(Protocol):
    """Protocol for tool executors used by the agentic loop."""

    def get_tool_definitions(self) -> list[ToolDef]:
        """Return the list of tools available to the model."""
        ...

    def execute(self, tool_name: str, arguments: dict) -> ToolResult:
        """Execute a tool call and return the result."""
        ...

    def total_response_bytes(self) -> int:
        """Return cumulative bytes returned across all tool calls."""
        ...

    def startup(self, repo_path: str) -> None:
        """Initialize the executor for a given repository."""
        ...

    def shutdown(self) -> None:
        """Clean up resources."""
        ...


class BaselineExecutor:
    """Baseline tool executor using raw filesystem operations.

    Provides three tools:
    - read_file: read full file contents from disk
    - grep_search: regex search across files with line numbers
    - list_directory: list files with optional glob filtering
    """

    def __init__(self) -> None:
        self._repo_path: Path | None = None
        self._total_bytes: int = 0

    def get_tool_definitions(self) -> list[ToolDef]:
        """Return definitions for the three baseline tools."""
        return [
            ToolDef(
                name="read_file",
                description="Read the full contents of a file from disk.",
                parameters={
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path relative to the repository root.",
                        },
                    },
                    "required": ["path"],
                },
            ),
            ToolDef(
                name="grep_search",
                description=(
                    "Search for a regex pattern across files in the repository. "
                    "Returns matching lines with file paths and line numbers."
                ),
                parameters={
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Python regex pattern to search for.",
                        },
                        "path": {
                            "type": "string",
                            "description": (
                                "Directory path relative to repo root to search in. "
                                "Defaults to the repository root."
                            ),
                        },
                    },
                    "required": ["pattern"],
                },
            ),
            ToolDef(
                name="list_directory",
                description=(
                    "List files in a directory with optional glob filtering."
                ),
                parameters={
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": (
                                "Directory path relative to repo root. "
                                "Defaults to the repository root."
                            ),
                        },
                        "glob": {
                            "type": "string",
                            "description": (
                                "Glob pattern to filter files. Defaults to '*'."
                            ),
                        },
                    },
                    "required": [],
                },
            ),
        ]

    def execute(self, tool_name: str, arguments: dict) -> ToolResult:
        """Execute a baseline tool call."""
        if self._repo_path is None:
            raise RuntimeError("BaselineExecutor not started. Call startup() first.")

        start = time.perf_counter()
        try:
            if tool_name == "read_file":
                content = self._read_file(arguments)
            elif tool_name == "grep_search":
                content = self._grep_search(arguments)
            elif tool_name == "list_directory":
                content = self._list_directory(arguments)
            else:
                content = f"Unknown tool: {tool_name}"
        except Exception as exc:
            content = f"Error: {exc}"

        elapsed_ms = (time.perf_counter() - start) * 1000
        byte_size = len(content.encode("utf-8"))
        self._total_bytes += byte_size
        return ToolResult(content=content, byte_size=byte_size, latency_ms=elapsed_ms)

    def total_response_bytes(self) -> int:
        """Return cumulative bytes returned across all tool calls."""
        return self._total_bytes

    def startup(self, repo_path: str) -> None:
        """Store the repository path for tool execution."""
        self._repo_path = Path(repo_path)

    def shutdown(self) -> None:
        """No-op for baseline executor."""

    # ------------------------------------------------------------------
    # Private tool implementations
    # ------------------------------------------------------------------

    def _read_file(self, arguments: dict) -> str:
        """Read a file relative to the repo root."""
        rel_path = arguments["path"]
        full_path = self._repo_path / rel_path  # type: ignore[operator]
        content = full_path.read_text(encoding="utf-8")
        _MAX_CHARS = 40_000  # ~10k tokens, fits in 8k context with room for conversation
        if len(content) > _MAX_CHARS:
            return content[:_MAX_CHARS] + f"\n[truncated: file is {len(content)} chars]"
        return content

    def _grep_search(self, arguments: dict) -> str:
        """Regex search across files, returning matching lines."""
        pattern = arguments["pattern"]
        search_path = arguments.get("path", "")
        base = self._repo_path / search_path if search_path else self._repo_path  # type: ignore[operator]

        compiled = re.compile(pattern)
        matches: list[str] = []
        _MAX_FILE_BYTES = 512 * 1024  # skip files larger than 512 KB
        _MAX_MATCHES = 200  # cap results to keep context window manageable

        for root, _dirs, files in os.walk(base):
            for fname in sorted(files):
                fpath = Path(root) / fname
                try:
                    if fpath.stat().st_size > _MAX_FILE_BYTES:
                        continue
                    text = fpath.read_text(encoding="utf-8", errors="replace")
                except (OSError, UnicodeDecodeError):
                    continue
                for line_no, line in enumerate(text.splitlines(), start=1):
                    if compiled.search(line):
                        rel = fpath.relative_to(self._repo_path)  # type: ignore[arg-type]
                        matches.append(f"{rel}:{line_no}:{line}")
                        if len(matches) >= _MAX_MATCHES:
                            return "\n".join(matches) + f"\n[truncated: more than {_MAX_MATCHES} matches]"

        return "\n".join(matches) if matches else "No matches found."

    def _list_directory(self, arguments: dict) -> str:
        """List directory contents with optional glob."""
        rel_path = arguments.get("path", "")
        glob_pattern = arguments.get("glob", "*")
        base = self._repo_path / rel_path if rel_path else self._repo_path  # type: ignore[operator]

        entries = sorted(base.glob(glob_pattern))
        lines = [str(e.relative_to(self._repo_path)) for e in entries]  # type: ignore[arg-type]
        return "\n".join(lines) if lines else "No entries found."
