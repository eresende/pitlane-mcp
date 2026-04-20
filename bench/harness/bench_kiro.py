#!/usr/bin/env python3
"""
Benchmark kiro-cli across two agents (with-mcp vs no-mcp).

Usage
-----
python bench_kiro.py \
    --repo /path/to/ripgrep \
    --prompts prompts/ripgrep.jsonl \
    --agent-mcp bench-with-mcp \
    --agent-no-mcp bench-no-mcp \
    --model glm-5 \
    --out out_kiro_ripgrep

Outputs
-------
  Legacy compatibility outputs maintained by this script only:
  <out>/results.jsonl
  <out>/results.csv
  <out>/scores_template.csv
  <out>/raw/<prompt_id>/<agent>/run_<n>/stdout.txt

This script is not part of the canonical bench.harness.run artifact contract.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import re
import subprocess
import time
from pathlib import Path
from typing import Any, Dict, List, Optional


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Benchmark kiro-cli with/without Pitlane MCP.")
    p.add_argument("--repo", required=True, help="Repo to run the agent inside.")
    p.add_argument("--prompts", required=True, help="JSONL file with benchmark prompts.")
    p.add_argument("--agent-mcp", default="bench-with-mcp", help="kiro-cli agent name with MCP.")
    p.add_argument("--agent-no-mcp", default="bench-no-mcp", help="kiro-cli agent name without MCP.")
    p.add_argument("--model", default="claude-haiku-4.5", help="Model to use for both agents.")
    p.add_argument("--timeout", type=int, default=300, help="Per-run timeout in seconds (default: 300).")
    p.add_argument("--runs", type=int, default=1, help="Repeats per prompt per agent.")
    p.add_argument("--out", default="benchmark_out_kiro", help="Output directory.")
    p.add_argument("--prompt-suffix", default="", help="Suffix appended to every prompt.")
    p.add_argument("--dry-run", action="store_true", help="Print commands without running.")
    return p.parse_args()


# ---------------------------------------------------------------------------
# Prompt loading
# ---------------------------------------------------------------------------

def load_prompts(path: Path) -> List[Dict[str, Any]]:
    rows: List[Dict[str, Any]] = []
    with path.open(encoding="utf-8") as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            if "id" not in obj or "prompt" not in obj:
                raise ValueError(f"{path}:{i}: needs 'id' and 'prompt'")
            rows.append(obj)
    return rows


# ---------------------------------------------------------------------------
# MCP toggle — temporarily disable pitlane-mcp for no-mcp runs
# ---------------------------------------------------------------------------

_MCP_JSON = Path.home() / ".kiro" / "settings" / "mcp.json"
_MCP_SERVER = "pitlane-mcp"


def _set_pitlane_disabled(disabled: bool) -> None:
    """Flip the disabled flag on pitlane-mcp in the global mcp.json."""
    if not _MCP_JSON.exists():
        return
    cfg = json.loads(_MCP_JSON.read_text(encoding="utf-8"))
    servers = cfg.get("mcpServers", {})
    if _MCP_SERVER in servers:
        servers[_MCP_SERVER]["disabled"] = disabled
        _MCP_JSON.write_text(json.dumps(cfg, indent=4), encoding="utf-8")


# ---------------------------------------------------------------------------
# Output parsing
# ---------------------------------------------------------------------------

# Patterns that indicate a tool invocation line in kiro-cli stdout.
# Anchored to actual tool call patterns to avoid matching file paths.
_TOOL_LINE_RE = re.compile(
    r"(using tool|Running tool|\bGrep\b|\bGlob\b"
    r"|✓ Successfully"
    r"|\breading\b|\bsearching\b|\bexecuting\b|\blisting\b|\bwriting\b)"
    r"|^(Reading|Searching|Executing|Running|Listing|Writing)\b",
    re.IGNORECASE,
)

_CREDITS_RE = re.compile(r"Credits:\s*([\d.]+)", re.IGNORECASE)
_TIME_RE = re.compile(r"Time:\s*(?:(\d+)m\s*)?(\d+)s", re.IGNORECASE)


def parse_output(stdout: str, stderr: str = "") -> Dict[str, Any]:
    """Extract tool calls, credits, latency hint, and answer text from kiro-cli output.

    Credits and time are emitted to stderr; tool lines and answer text come from stdout.
    """
    tool_lines: List[str] = []
    answer_lines: List[str] = []
    credits_used: Optional[float] = None
    reported_time: Optional[int] = None

    # Credits and time live in stderr
    for line in stderr.splitlines():
        m_credits = _CREDITS_RE.search(line)
        m_time = _TIME_RE.search(line)
        if m_credits:
            credits_used = float(m_credits.group(1))
        if m_time:
            minutes = int(m_time.group(1)) if m_time.group(1) else 0
            seconds = int(m_time.group(2))
            reported_time = minutes * 60 + seconds

    for line in stdout.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        # Strip ANSI escape codes before matching
        clean = re.sub(r"\x1b\[[0-9;]*m", "", stripped)
        # Tool invocation lines
        if _TOOL_LINE_RE.search(clean):
            tool_lines.append(clean)
        elif clean.startswith(">"):
            answer_lines.append(clean.lstrip("> ").strip())

    # Count distinct pitlane MCP calls specifically — match tool invocation lines,
    # not just any line containing the word "pitlane" (e.g. file paths).
    pitlane_calls = [l for l in tool_lines if re.search(
        r"(Running tool|using tool).*(index_project|search_symbols|get_symbol|get_file_outline"
        r"|find_usages|get_lines|get_index_stats|watch_project|wait_for_embeddings|get_project_outline)"
        r"|from mcp server.*pitlane",
        l, re.IGNORECASE,
    )]

    return {
        "tool_call_count": len(tool_lines),
        "pitlane_call_count": len(pitlane_calls),
        "tool_lines": tool_lines[:50],
        "pitlane_lines": pitlane_calls[:30],
        "credits_used": credits_used,
        "reported_time_s": reported_time,
        "answer_text": "\n".join(answer_lines),
    }


# ---------------------------------------------------------------------------
# Single run
# ---------------------------------------------------------------------------

def run_once(
    repo: Path,
    out_dir: Path,
    agent: str,
    prompt_row: Dict[str, Any],
    run_index: int,
    model: str,
    prompt_suffix: str,
    timeout: int = 300,
    dry_run: bool = False,
) -> Dict[str, Any]:
    prompt_id = str(prompt_row["id"])
    category = prompt_row.get("category", "")
    prompt_text = str(prompt_row["prompt"]).strip()
    # Per-prompt suffix takes priority over global suffix
    row_suffix = str(prompt_row.get("prompt_suffix", "")).strip()
    suffix = row_suffix or prompt_suffix.strip()
    full_prompt = prompt_text + ("\n\n" + suffix if suffix else "")

    raw_dir = out_dir / "raw" / prompt_id / agent / f"run_{run_index}"
    raw_dir.mkdir(parents=True, exist_ok=True)

    cmd = [
        "kiro-cli", "chat",
        "--no-interactive",
        "--agent", agent,
        "--model", model,
    ]

    # For the no-mcp agent, explicitly restrict tools to prevent MCP leakage
    if "no-mcp" in agent:
        cmd += ["--trust-tools", "read,grep,glob"]
    else:
        cmd += ["--trust-all-tools"]

    cmd += [full_prompt]

    (raw_dir / "cmd.txt").write_text(" ".join(cmd), encoding="utf-8")

    if dry_run:
        print("DRY RUN:", " ".join(cmd))
        return {
            "prompt_id": prompt_id, "category": category,
            "agent": agent, "run_index": run_index, "status": "dry_run",
        }

    start = time.perf_counter()
    # For no-mcp runs, temporarily disable pitlane in the global mcp.json
    # so kiro-cli cannot load it regardless of agent config.
    is_no_mcp = "no-mcp" in agent
    if is_no_mcp:
        _set_pitlane_disabled(True)
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(repo),
            text=True,
            capture_output=True,
            timeout=timeout,
        )
        timed_out = False
    except subprocess.TimeoutExpired as e:
        elapsed = round(time.perf_counter() - start, 3)
        stdout = (e.stdout or b"").decode("utf-8", errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
        stderr = (e.stderr or b"").decode("utf-8", errors="replace") if isinstance(e.stderr, bytes) else (e.stderr or "")
        (raw_dir / "stdout.txt").write_text(stdout, encoding="utf-8")
        (raw_dir / "stderr.txt").write_text(f"TIMEOUT after {timeout}s\n{stderr}", encoding="utf-8")
        print(f"  → TIMEOUT after {timeout}s")
        return {
            "prompt_id": prompt_id, "category": category,
            "agent": agent, "run_index": run_index,
            "status_code": -1, "latency_seconds": elapsed,
            "reported_time_s": None, "credits_used": None,
            "tool_call_count": None, "pitlane_call_count": None,
            "answer_chars": 0, "answer_preview": "TIMEOUT",
            "tool_lines": [], "pitlane_lines": [],
            "raw_dir": str(raw_dir),
        }
    finally:
        if is_no_mcp:
            _set_pitlane_disabled(False)
    timed_out = False  # noqa: F841 (kept for future use)
    elapsed = round(time.perf_counter() - start, 3)
    stdout = proc.stdout or ""
    stderr = proc.stderr or ""
    (raw_dir / "stdout.txt").write_text(stdout, encoding="utf-8")
    (raw_dir / "stderr.txt").write_text(stderr, encoding="utf-8")

    parsed = parse_output(stdout, stderr)

    return {
        "prompt_id": prompt_id,
        "category": category,
        "agent": agent,
        "run_index": run_index,
        "status_code": proc.returncode,
        "latency_seconds": elapsed,
        "reported_time_s": parsed["reported_time_s"],
        "credits_used": parsed["credits_used"],
        "tool_call_count": parsed["tool_call_count"],
        "pitlane_call_count": parsed["pitlane_call_count"],
        "answer_chars": len(parsed["answer_text"]),
        "answer_preview": parsed["answer_text"][:800],
        "tool_lines": parsed["tool_lines"],
        "pitlane_lines": parsed["pitlane_lines"],
        "raw_dir": str(raw_dir),
    }


# ---------------------------------------------------------------------------
# Output writing
# ---------------------------------------------------------------------------

FLAT_FIELDS = [
    "prompt_id", "category", "agent", "run_index",
    "status_code", "latency_seconds", "reported_time_s", "credits_used",
    "tool_call_count", "pitlane_call_count",
    "answer_chars", "answer_preview",
]

SCORE_FIELDS = ["prompt_id", "category", "agent", "run_index", "score_0_3", "notes"]


def write_results(out_dir: Path, rows: List[Dict[str, Any]]) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)

    jsonl_path = out_dir / "results.jsonl"
    with jsonl_path.open("w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row) + "\n")

    csv_path = out_dir / "results.csv"
    with csv_path.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=FLAT_FIELDS, extrasaction="ignore")
        w.writeheader()
        w.writerows(rows)

    scores_path = out_dir / "scores_template.csv"
    with scores_path.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=SCORE_FIELDS)
        w.writeheader()
        for row in rows:
            w.writerow({k: row.get(k, "") for k in SCORE_FIELDS})

    print(f"\nWrote {len(rows)} rows → {out_dir}")


def summarize(rows: List[Dict[str, Any]]) -> str:
    lines = ["", "=== Summary by agent ==="]
    by_agent: Dict[str, List[Dict[str, Any]]] = {}
    for row in rows:
        by_agent.setdefault(row["agent"], []).append(row)

    for agent, items in sorted(by_agent.items()):
        lats = [r["latency_seconds"] for r in items if isinstance(r.get("latency_seconds"), float)]
        tools = [r["tool_call_count"] for r in items if isinstance(r.get("tool_call_count"), int)]
        pitlane = [r["pitlane_call_count"] for r in items if isinstance(r.get("pitlane_call_count"), int)]
        credits = [r["credits_used"] for r in items if isinstance(r.get("credits_used"), float)]

        avg = lambda xs: round(sum(xs) / len(xs), 2) if xs else None
        lines.append(
            f"  {agent}: runs={len(items)}"
            f"  avg_latency={avg(lats)}s"
            f"  avg_tools={avg(tools)}"
            f"  avg_pitlane_calls={avg(pitlane)}"
            f"  avg_credits={avg(credits)}"
        )
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    args = parse_args()
    repo = Path(args.repo).expanduser().resolve()
    prompts_path = Path(args.prompts).expanduser()
    out_dir = Path(args.out).expanduser()

    if not repo.exists():
        print(f"ERROR: repo not found: {repo}")
        return 1
    if not prompts_path.exists():
        print(f"ERROR: prompts file not found: {prompts_path}")
        return 1

    prompts = load_prompts(prompts_path)
    agents = [args.agent_mcp, args.agent_no_mcp]

    total = len(prompts) * len(agents) * args.runs
    print(f"Repo:    {repo}")
    print(f"Prompts: {len(prompts)}  Agents: {agents}  Runs: {args.runs}  Total: {total}")
    print(f"Model:   {args.model}")
    print(f"Output:  {out_dir}")
    if args.dry_run:
        print("(dry run)")

    rows: List[Dict[str, Any]] = []
    n = 0
    for prompt_row in prompts:
        for agent in agents:
            for run_i in range(args.runs):
                n += 1
                pid = prompt_row["id"]
                print(f"\n[{n}/{total}] prompt={pid}  agent={agent}  run={run_i}")
                row = run_once(
                    repo=repo,
                    out_dir=out_dir,
                    agent=agent,
                    prompt_row=prompt_row,
                    run_index=run_i,
                    model=args.model,
                    prompt_suffix=args.prompt_suffix,
                    timeout=args.timeout,
                    dry_run=args.dry_run,
                )
                rows.append(row)
                if not args.dry_run:
                    print(
                        f"  → latency={row.get('latency_seconds')}s"
                        f"  tools={row.get('tool_call_count')}"
                        f"  pitlane={row.get('pitlane_call_count')}"
                        f"  credits={row.get('credits_used')}"
                    )

    if not args.dry_run:
        write_results(out_dir, rows)
        print(summarize(rows))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
