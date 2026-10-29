"""Read-only experiment tools, with explicit errors and isolated MCP sessions."""
from __future__ import annotations

from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time

from jsonschema import Draft202012Validator

from bench.harness.framework.executors import BaselineExecutor
from bench.harness.framework.mcp_executor import MCPExecutor
from bench.harness.framework.models import ToolResult


class DeadlineExceeded(BaseException):
    """Bypass legacy catch-all handlers; the experiment records a hard timeout."""


@contextmanager
def deadline(seconds):
    def expired(*_):
        raise DeadlineExceeded()
    previous = signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], stderr=subprocess.DEVNULL)


def repo_identity(repo):
    """Hash actual worktree bytes (including untracked instruction files), plus HEAD/status."""
    digest = hashlib.sha256()
    names = git(repo, "ls-files", "-z", "--cached", "--others", "--exclude-standard").split(b"\0")
    for name in sorted(set(names) - {b""}):
        path = Path(repo) / os.fsdecode(name)
        digest.update(name + b"\0")
        if path.is_symlink():
            digest.update(os.readlink(path).encode())
        elif path.is_file():
            digest.update(path.read_bytes())
        else:
            digest.update(b"<absent>")
    return {"commit": git(repo, "rev-parse", "HEAD").decode().strip(),
            "status_sha256": hashlib.sha256(git(repo, "status", "--porcelain", "-z")).hexdigest(),
            "content_sha256": digest.hexdigest()}


def make_snapshot(repo, destination):
    """Export tracked HEAD only. No inherited agent config or .git access."""
    import io
    import tarfile
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=False)
    archive = git(repo, "archive", "HEAD")
    excluded = []
    links = []
    blocked = {"AGENTS.md", "CLAUDE.md", "GEMINI.md", "SKILL.md", ".env"}
    config_dirs = {".agents", ".codex", ".claude", ".gemini", ".kiro", ".opencode"}
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        for member in tar.getmembers():
            relative = Path(member.name)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError("Unsafe archive path")
            if relative.name in blocked or relative.name.startswith(".env.") or config_dirs.intersection(relative.parts):
                excluded.append(member.name)
                continue
            target = destination / relative
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(tar.extractfile(member).read())
                target.chmod(0o444)
            elif member.issym():
                links.append((target, member.linkname))
            else:
                raise ValueError("Snapshots reject special files")
    for target, link in links:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.symlink_to(link)
    for target, _ in links:
        if not target.resolve().is_relative_to(destination.resolve()) or not target.exists():
            raise ValueError("Snapshots reject escaping or broken symlinks")
    return {"excluded_tracked_instruction_files": excluded, "sha256": tree_digest(destination)}


def tree_digest(root):
    digest = hashlib.sha256()
    for path in sorted(Path(root).rglob("*")):
        if path.is_symlink():
            digest.update(str(path.relative_to(root)).encode() + b"\0link:" + os.readlink(path).encode())
        elif path.is_file():
            digest.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes())
    return digest.hexdigest()


class IsolatedMCP(MCPExecutor):
    def __init__(self, home, tier="default", guidance="full"):
        super().__init__()
        self.home = str(home)
        self.tier = tier
        self.guidance = guidance
        self.ready = None

    def _build_env(self):
        # The model never runs a host CLI agent and cannot read any host configuration.
        # MCP receives only explicit embedding settings and its own cache/config home.
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": self.home,
               "XDG_CACHE_HOME": str(Path(self.home) / ".cache"),
               "PITLANE_MCP_TOOL_TIER": self.tier,
               "PITLANE_MCP_TOOL_GUIDANCE": self.guidance}
        env.update({k: v for k, v in os.environ.items() if k.startswith("PITLANE_EMBED_")})
        return env

    def _ensure_project_ready(self, repo_path):
        response = self._call_tool("ensure_project_ready", {"path": repo_path, "watch": False})
        if response.get("result", {}).get("isError"):
            raise RuntimeError("index_preparation_failed")
        self.ready = json.loads(self._extract_content(response))


class ExperimentTools:
    """Same native capabilities in every arm, plus server tools in B/C.

    Calls never have their project/path arguments repaired by the harness.
    Local logs store arguments/sizes/hashes/errors, without tool response bodies.
    Keep these artifacts private and outside version control.
    """
    def __init__(self, root, mcp=None):
        self.root = Path(root).resolve()
        self.native = BaselineExecutor()
        self.native.startup(str(self.root))
        self.mcp = mcp
        self.native_names = {t.name for t in self.native.get_tool_definitions()}
        definitions = self.native.get_tool_definitions() + (mcp.get_tool_definitions() if mcp else [])
        self.definitions = {t.name: t for t in definitions}
        self.events = []
        self._bytes = 0

    def get_tool_definitions(self):
        return list(self.definitions.values())

    def total_response_bytes(self):
        return self._bytes

    def execute(self, name, arguments):
        start = time.perf_counter()
        error = None
        key = hashlib.sha256(json.dumps([name, arguments], sort_keys=True).encode()).hexdigest()
        previous = [e for e in self.events if e["call_sha256"] == key]
        try:
            if name not in self.definitions:
                error = "unknown_tool"
                raise ValueError()
            if not isinstance(arguments, dict):
                error = "invalid_arguments"
                raise ValueError()
            errors = list(Draft202012Validator(self.definitions[name].parameters).iter_errors(arguments))
            if errors:
                error = "invalid_arguments"
                # Schema location only: no credentials or argument values in error strings.
                raise ValueError("schema:" + ",".join(str(e.validator) for e in errors))
            if name in self.native_names:
                path = (self.root / arguments.get("path", "")).resolve()
                if not path.is_relative_to(self.root):
                    error = "path_outside_snapshot"
                    raise ValueError()
                if "glob" in arguments and (Path(arguments["glob"]).is_absolute() or ".." in Path(arguments["glob"]).parts):
                    error = "path_outside_snapshot"
                    raise ValueError()
                result = self.native.execute(name, arguments)
                content = result.content
                if content.startswith(("Error:", "Unknown tool:")):
                    error = "native_tool_error"
            else:
                # No cross-project MCP access; all project paths must match the snapshot.
                for field in ("project", "path"):
                    if field in arguments and Path(arguments[field]).resolve() != self.root:
                        error = "path_outside_snapshot"
                        raise ValueError()
                for field in ("file_path", "document"):
                    if arguments.get(field) and not (self.root / arguments[field]).resolve().is_relative_to(self.root):
                        error = "path_outside_snapshot"
                        raise ValueError()
                if name in {"ensure_project_ready", "doctor"} and (arguments.get("force") or arguments.get("repair") or arguments.get("watch") or arguments.get("exclude") or arguments.get("max_files")):
                    error = "read_only_policy"
                    raise ValueError()
                args = dict(arguments)
                if name == "ensure_project_ready":
                    args["watch"] = False
                raw = self.mcp._call_tool(name, args)
                content = self.mcp._extract_content(raw)
                if raw.get("result", {}).get("isError"):
                    error = "mcp_tool_error"
        except Exception as exc:
            error = error or "tool_transport_error"
            content = "Error: " + error
            if error == "invalid_arguments" and str(exc).startswith("schema:"):
                content += " (" + str(exc) + ")"
        size = len(content.encode())
        self._bytes += size
        self.events.append({"tool_name": name, "kind": "native" if name in self.native_names else "mcp" if name in self.definitions else "unknown",
                            "arguments": arguments,
                            "call_sha256": key, "result_bytes": size, "error": error,
                            "retry_after_failure": bool(previous and previous[-1]["error"]),
                            "redundant_successful_call": bool(previous and not previous[-1]["error"]),
                            "latency_ms": (time.perf_counter() - start) * 1000})
        return ToolResult(content, size, (time.perf_counter() - start) * 1000)
