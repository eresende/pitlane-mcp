"""Integration tests requiring real Ollama + pitlane-mcp.

All tests are marked with @pytest.mark.integration and are skipped
automatically when pitlane-mcp is not on PATH.

Requirements: 4.1, 4.3, 12.3
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

# ---------------------------------------------------------------------------
# Skip marker: skip entire module if pitlane-mcp is not on PATH
# ---------------------------------------------------------------------------

pytestmark = pytest.mark.integration

_PITLANE_AVAILABLE = shutil.which("pitlane-mcp") is not None

skip_no_pitlane = pytest.mark.skipif(
    not _PITLANE_AVAILABLE,
    reason="pitlane-mcp not found on PATH",
)

# Path to the bats benchmark repo (relative to workspace root)
_BATS_REPO = Path(__file__).parents[3] / "bench" / "repos" / "bats"


# ---------------------------------------------------------------------------
# Helper: send a single JSON-RPC message and read one response line
# ---------------------------------------------------------------------------


def _jsonrpc_send(proc: subprocess.Popen, message: dict) -> dict:
    """Write a JSON-RPC message to proc.stdin and read one response line."""
    line = json.dumps(message) + "\n"
    proc.stdin.write(line.encode("utf-8"))
    proc.stdin.flush()
    raw = proc.stdout.readline()
    return json.loads(raw.decode("utf-8"))


# ---------------------------------------------------------------------------
# Test 1: MCP JSON-RPC handshake
# ---------------------------------------------------------------------------


@skip_no_pitlane
def test_mcp_jsonrpc_handshake():
    """Spawn pitlane-mcp, send initialize, verify a valid response is returned."""
    proc = subprocess.Popen(
        ["pitlane-mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        response = _jsonrpc_send(
            proc,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "0.1.0"},
                },
            },
        )

        # Response must be valid JSON-RPC 2.0
        assert response.get("jsonrpc") == "2.0", f"Unexpected response: {response}"
        assert response.get("id") == 1
        assert "result" in response, f"Expected 'result' in response: {response}"

        # Result should contain serverInfo or protocolVersion
        result = response["result"]
        assert isinstance(result, dict), f"result should be a dict, got: {result}"

    finally:
        try:
            proc.stdin.close()
        except Exception:
            pass
        proc.terminate()
        proc.wait(timeout=5)


# ---------------------------------------------------------------------------
# Test 2: Git commit hash detection
# ---------------------------------------------------------------------------


def test_detect_git_commit_workspace_root():
    """_detect_git_commit() on the workspace root returns a valid hex string."""
    from bench.harness.framework.benchmark_runner import _detect_git_commit

    # Use the workspace root (3 levels up from this file)
    workspace_root = str(Path(__file__).parents[3])
    commit = _detect_git_commit(workspace_root)

    # If git is available and this is a git repo, we get a hex string
    if commit is not None:
        assert isinstance(commit, str)
        assert len(commit) == 40, f"Expected 40-char hex hash, got: {commit!r}"
        assert all(c in "0123456789abcdef" for c in commit.lower()), (
            f"Commit hash contains non-hex chars: {commit!r}"
        )


def test_detect_git_commit_non_repo(tmp_path):
    """_detect_git_commit() on a non-git directory returns None gracefully."""
    from bench.harness.framework.benchmark_runner import _detect_git_commit

    result = _detect_git_commit(str(tmp_path))
    assert result is None


# ---------------------------------------------------------------------------
# Test 3: BaselineExecutor on bench/repos/bats
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    not _BATS_REPO.is_dir(),
    reason="bench/repos/bats not found",
)
def test_baseline_executor_on_bats_repo():
    """BaselineExecutor: startup on bats repo, read a file, verify content."""
    from bench.harness.framework.executors import BaselineExecutor

    executor = BaselineExecutor()
    executor.startup(str(_BATS_REPO))

    try:
        # README.md should exist in the bats repo
        result = executor.execute("read_file", {"path": "README.md"})
        assert result.content, "README.md should have non-empty content"
        assert result.byte_size > 0
        assert result.byte_size == len(result.content.encode("utf-8"))
        # bats README should mention "bats" somewhere
        assert "bats" in result.content.lower(), (
            "Expected 'bats' in README.md content"
        )
    finally:
        executor.shutdown()


@pytest.mark.skipif(
    not _BATS_REPO.is_dir(),
    reason="bench/repos/bats not found",
)
def test_baseline_executor_list_directory_on_bats():
    """BaselineExecutor: list_directory on bats repo root returns entries."""
    from bench.harness.framework.executors import BaselineExecutor

    executor = BaselineExecutor()
    executor.startup(str(_BATS_REPO))

    try:
        result = executor.execute("list_directory", {})
        assert result.content != "No entries found.", (
            "Expected entries in bats repo root"
        )
        # README.md should appear in the listing
        assert "README.md" in result.content
    finally:
        executor.shutdown()


# ---------------------------------------------------------------------------
# Test 4: MCPExecutor startup on bats repo (requires pitlane-mcp)
# ---------------------------------------------------------------------------


@skip_no_pitlane
@pytest.mark.skipif(
    not _BATS_REPO.is_dir(),
    reason="bench/repos/bats not found",
)
def test_mcp_executor_startup_and_tool_list():
    """MCPExecutor: startup on bats repo, verify tool definitions are populated."""
    from bench.harness.framework.mcp_executor import MCPExecutor, PITLANE_TOOL_NAMES

    executor = MCPExecutor()
    executor.startup(str(_BATS_REPO))

    try:
        tools = executor.get_tool_definitions()
        assert len(tools) > 0, "MCPExecutor should have tool definitions after startup"

        tool_names = {t.name for t in tools}
        # At minimum, core tools should be present
        for expected in ("search_symbols", "get_symbol", "search_content"):
            assert expected in tool_names, (
                f"Expected tool {expected!r} in MCPExecutor tool list"
            )
    finally:
        executor.shutdown()
