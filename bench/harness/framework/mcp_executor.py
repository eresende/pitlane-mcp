"""MCP tool executor for with-MCP benchmark mode.

Spawns pitlane-mcp as a subprocess and communicates via MCP JSON-RPC over stdio.
Implements the ToolExecutor Protocol defined in executors.py.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import time
from typing import Any

from bench.harness.framework.models import ToolDef, ToolResult


# Known pitlane-mcp tool names (used as fallback if tools/list fails)
PITLANE_TOOL_NAMES = [
    "ensure_project_ready",
    "locate_code",
    "read_code_unit",
    "trace_path",
    "analyze_impact",
    "get_index_stats",
    "search_content",
]


class MCPExecutor:
    """Tool executor that drives pitlane-mcp over MCP JSON-RPC stdio.

    Lifecycle:
        executor = MCPExecutor()
        executor.startup(repo_path)   # spawns process, handshakes, indexes repo
        result = executor.execute("search_symbols", {"query": "..."})
        executor.shutdown()           # terminates subprocess
    """

    def __init__(self) -> None:
        self._process: subprocess.Popen | None = None
        self._tool_defs: list[ToolDef] = []
        self._total_bytes: int = 0
        self._request_id: int = 0
        self._stderr_lines: list[str] = []
        self._stderr_thread: threading.Thread | None = None
        self._repo_path: str | None = None
        self._embeddings_available: bool = False

    # ------------------------------------------------------------------
    # ToolExecutor Protocol
    # ------------------------------------------------------------------

    def get_tool_definitions(self) -> list[ToolDef]:
        """Return tool definitions discovered from pitlane-mcp."""
        return self._tool_defs

    def execute(self, tool_name: str, arguments: dict) -> ToolResult:
        """Execute a pitlane-mcp tool call via JSON-RPC tools/call."""
        self._check_process_alive()
        start = time.perf_counter()
        normalized_arguments = self._normalize_arguments(tool_name, arguments)
        try:
            response = self._call_tool(tool_name, normalized_arguments)
            content = self._extract_content(response)
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
        """Spawn pitlane-mcp, perform MCP handshake, and index the repo.

        Raises:
            FileNotFoundError: if pitlane-mcp is not on PATH.
            RuntimeError: if the MCP handshake fails.
        """
        self._repo_path = repo_path
        self._spawn_process()
        self._handshake()
        self._discover_tools()
        self._ensure_project_ready(repo_path)

    def shutdown(self) -> None:
        """Terminate the pitlane-mcp subprocess."""
        if self._process is not None:
            try:
                self._process.stdin.close()  # type: ignore[union-attr]
            except Exception:
                pass
            try:
                self._process.terminate()
                self._process.wait(timeout=5)
            except Exception:
                try:
                    self._process.kill()
                except Exception:
                    pass
            self._process = None

        if self._stderr_thread is not None:
            self._stderr_thread.join(timeout=2)
            self._stderr_thread = None

    # ------------------------------------------------------------------
    # Semantic search support
    # ------------------------------------------------------------------

    @property
    def embeddings_available(self) -> bool:
        """True if embeddings were confirmed available after startup."""
        return self._embeddings_available

    # ------------------------------------------------------------------
    # Private: process management
    # ------------------------------------------------------------------

    def _build_env(self) -> dict[str, str]:
        """Build subprocess environment, forwarding semantic search vars if set."""
        env = os.environ.copy()
        embed_url = os.environ.get("PITLANE_EMBED_URL")
        embed_model = os.environ.get("PITLANE_EMBED_MODEL")
        if embed_url:
            env["PITLANE_EMBED_URL"] = embed_url
        if embed_model:
            env["PITLANE_EMBED_MODEL"] = embed_model
        return env

    def _spawn_process(self) -> None:
        """Spawn the pitlane-mcp subprocess."""
        try:
            self._process = subprocess.Popen(
                ["pitlane-mcp"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=self._build_env(),
            )
        except FileNotFoundError:
            raise FileNotFoundError(
                "pitlane-mcp not found on PATH. "
                "Install it from https://github.com/eresende/pitlane-mcp"
            )

        # Drain stderr in background to prevent deadlocks
        self._stderr_thread = threading.Thread(
            target=self._drain_stderr, daemon=True
        )
        self._stderr_thread.start()

    def _drain_stderr(self) -> None:
        """Background thread: read stderr lines to avoid pipe buffer deadlock."""
        assert self._process is not None
        try:
            for line in self._process.stderr:  # type: ignore[union-attr]
                decoded = line.decode("utf-8", errors="replace").rstrip()
                self._stderr_lines.append(decoded)
        except Exception:
            pass

    def _check_process_alive(self) -> None:
        """Raise if the subprocess has exited unexpectedly."""
        if self._process is None:
            raise RuntimeError("MCPExecutor not started. Call startup() first.")
        ret = self._process.poll()
        if ret is not None:
            stderr_tail = "\n".join(self._stderr_lines[-20:])
            raise RuntimeError(
                f"pitlane-mcp process exited unexpectedly with code {ret}.\n"
                f"stderr:\n{stderr_tail}"
            )

    # ------------------------------------------------------------------
    # Private: JSON-RPC transport
    # ------------------------------------------------------------------

    def _next_id(self) -> int:
        self._request_id += 1
        return self._request_id

    def _send(self, message: dict[str, Any]) -> None:
        """Write a JSON-RPC message as a newline-delimited line to stdin."""
        assert self._process is not None
        line = json.dumps(message) + "\n"
        self._process.stdin.write(line.encode("utf-8"))  # type: ignore[union-attr]
        self._process.stdin.flush()  # type: ignore[union-attr]

    def _recv(self) -> dict[str, Any]:
        """Read one JSON-RPC response line from stdout."""
        assert self._process is not None
        raw = self._process.stdout.readline()  # type: ignore[union-attr]
        if not raw:
            ret = self._process.poll()
            stderr_tail = "\n".join(self._stderr_lines[-20:])
            raise RuntimeError(
                f"pitlane-mcp stdout closed (exit code: {ret}).\n"
                f"stderr:\n{stderr_tail}"
            )
        return json.loads(raw.decode("utf-8"))

    def _send_notification(self, method: str, params: dict | None = None) -> None:
        """Send a JSON-RPC notification (no id, no response expected)."""
        msg: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)

    def _request(self, method: str, params: dict) -> dict[str, Any]:
        """Send a JSON-RPC request and return the response."""
        req_id = self._next_id()
        self._send({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params})
        response = self._recv()
        if "error" in response:
            raise RuntimeError(
                f"JSON-RPC error for {method}: {response['error']}"
            )
        return response

    # ------------------------------------------------------------------
    # Private: MCP protocol
    # ------------------------------------------------------------------

    def _handshake(self) -> None:
        """Perform the MCP initialize / notifications/initialized handshake."""
        self._request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "bench-runner", "version": "0.1.0"},
            },
        )
        self._send_notification("notifications/initialized")

    def _discover_tools(self) -> None:
        """Fetch tool definitions via tools/list and store as ToolDef objects."""
        response = self._request("tools/list", {})
        tools_raw: list[dict] = response.get("result", {}).get("tools", [])
        self._tool_defs = [
            ToolDef(
                name=t["name"],
                description=t.get("description", ""),
                parameters=t.get("inputSchema", {}),
            )
            for t in tools_raw
        ]

    def _call_tool(self, tool_name: str, arguments: dict) -> dict[str, Any]:
        """Send a tools/call request and return the raw response."""
        return self._request(
            "tools/call",
            {"name": tool_name, "arguments": arguments},
        )

    def _extract_content(self, response: dict[str, Any]) -> str:
        """Extract text content from a tools/call response."""
        result = response.get("result", {})
        content_list = result.get("content", [])
        parts: list[str] = []
        for item in content_list:
            if isinstance(item, dict) and item.get("type") == "text":
                parts.append(item.get("text", ""))
        return "\n".join(parts) if parts else json.dumps(result)

    def _normalize_arguments(self, tool_name: str, arguments: dict) -> dict[str, Any]:
        """Fill in known required arguments the harness already has."""
        normalized = dict(arguments)
        tool_def = next((tool for tool in self._tool_defs if tool.name == tool_name), None)
        schema = tool_def.parameters if tool_def is not None else {}
        properties = schema.get("properties", {})
        required = set(schema.get("required", []))
        if (
            self._repo_path
            and "project" in properties
            and "project" in required
            and not normalized.get("project")
        ):
            normalized["project"] = self._repo_path
        return normalized

    def _ensure_project_ready(self, repo_path: str) -> None:
        """Call ensure_project_ready and check for embedding availability."""
        response = self._call_tool("ensure_project_ready", {"path": repo_path})
        content = self._extract_content(response)

        # Check if semantic search env vars are configured
        embed_url = os.environ.get("PITLANE_EMBED_URL")
        embed_model = os.environ.get("PITLANE_EMBED_MODEL")
        if embed_url and embed_model:
            # Embeddings are configured; check if they're available
            # The ensure_project_ready response mentions embedding status
            lower = content.lower()
            self._embeddings_available = (
                "embedding" in lower and "disabled" not in lower
            ) or "ok" in lower
        else:
            self._embeddings_available = False
