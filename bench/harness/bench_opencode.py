#!/usr/bin/env python3
"""
Benchmark OpenCode across two configs (for example, with-MCP vs no-MCP).

Features
- Runs the same prompt set against two OpenCode backends or configs
- Captures raw JSON event streams from `opencode run --format json`
- Measures wall-clock latency
- Tries to extract token usage and session IDs from the event stream
- Writes:
  - results.jsonl   : one row per run
  - results.csv     : flattened summary
  - raw/<id>/...    : stdout/stderr per run
  - scores_template.csv : optional manual scoring sheet

Typical usage
-------------
1) Start two attached servers:

   export OPENAI_API_KEY=your_openai_api_key_here\n   OPENCODE_CONFIG=opencode.with-mcp.json opencode serve --port 4096
   OPENCODE_CONFIG=opencode.no-mcp.json   opencode serve --port 4097

2) Run benchmark:

   python bench_opencode.py \
     --repo /path/to/guava \
     --prompts prompts.jsonl \
     --target mcp=http://localhost:4096 \
     --target no_mcp=http://localhost:4097 \
     --agent build \
     --model openai/gpt-5.4 \
     --prompt-suffix "Ground your answer in the repository. Name exact files and symbols you used, and say clearly when something is not found."

Or, if you want each run to start its own local process instead of attaching:

   python bench_opencode.py \
     --repo /path/to/guava \
     --prompts prompts.jsonl \
     --target mcp=CONFIG:/path/to/opencode.with-mcp.json \
     --target no_mcp=CONFIG:/path/to/opencode.no-mcp.json

Notes
-----
- Attached servers usually reduce MCP cold-start noise.
- The script is defensive about JSON event shapes because CLI event schemas can evolve.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple


@dataclass
class Target:
    label: str
    value: str  # URL or CONFIG:/path/to/config.json

    @property
    def is_attach(self) -> bool:
        return self.value.startswith("http://") or self.value.startswith("https://")

    @property
    def is_config(self) -> bool:
        return self.value.startswith("CONFIG:")

    @property
    def config_path(self) -> Optional[str]:
        if self.is_config:
            return self.value.split("CONFIG:", 1)[1]
        return None


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Benchmark OpenCode across multiple targets.")
    p.add_argument("--repo", required=True, help="Path to repository to benchmark in.")
    p.add_argument("--prompts", required=True, help="JSONL file with benchmark prompts.")
    p.add_argument(
        "--target",
        action="append",
        required=True,
        help=(
            "Benchmark target in the form label=value. "
            "Value can be http://host:port for --attach, or CONFIG:/path/to/opencode.json."
        ),
    )
    p.add_argument("--agent", default=None, help="OpenCode agent to use, e.g. build.")
    p.add_argument("--model", default="openai/gpt-5.4", help="Model override, e.g. openai/gpt-5.4.")
    p.add_argument("--title-prefix", default="bench", help="Session title prefix.")
    p.add_argument("--prompt-suffix", default="", help="Suffix appended to every prompt.")
    p.add_argument("--runs", type=int, default=1, help="Number of repeats per prompt/target.")
    p.add_argument("--out", default="benchmark_out", help="Output directory.")
    p.add_argument("--extra-arg", action="append", default=[], help="Extra arg passed to `opencode run`.")
    p.add_argument("--dry-run", action="store_true", help="Print commands without executing.")
    return p.parse_args()


def load_prompts(path: Path) -> List[Dict[str, Any]]:
    rows: List[Dict[str, Any]] = []
    with path.open("r", encoding="utf-8") as f:
        for i, line in enumerate(f, start=1):
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            if "id" not in obj or "prompt" not in obj:
                raise ValueError(f"{path}:{i}: each JSONL row needs at least id and prompt")
            rows.append(obj)
    return rows


def parse_targets(raw_targets: Iterable[str]) -> List[Target]:
    targets: List[Target] = []
    for item in raw_targets:
        if "=" not in item:
            raise ValueError(f"Invalid --target {item!r}; expected label=value")
        label, value = item.split("=", 1)
        label = label.strip()
        value = value.strip()
        if not label or not value:
            raise ValueError(f"Invalid --target {item!r}; empty label or value")
        targets.append(Target(label=label, value=value))
    return targets


def ensure_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)


def build_opencode_command(
    target: Target,
    full_prompt: str,
    agent: Optional[str],
    model: Optional[str],
    title: str,
    extra_args: List[str],
) -> Tuple[List[str], Dict[str, str]]:
    cmd = ["opencode", "run", "--format", "json", "--title", title]
    env = os.environ.copy()

    if target.is_attach:
        cmd += ["--attach", target.value]
    elif target.is_config:
        env["OPENCODE_CONFIG"] = str(target.config_path)
    else:
        raise ValueError(f"Unsupported target value: {target.value}")

    if agent:
        cmd += ["--agent", agent]
    if model:
        cmd += ["--model", model]

    cmd += extra_args
    cmd += [full_prompt]
    return cmd, env


def try_parse_json_lines(text: str) -> List[Any]:
    events: List[Any] = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            # Keep going; some versions/log modes may emit non-JSON lines.
            pass
    return events


def walk(obj: Any):
    if isinstance(obj, dict):
        for k, v in obj.items():
            yield k, v
            yield from walk(v)
    elif isinstance(obj, list):
        for item in obj:
            yield from walk(item)


def find_first_key(obj: Any, candidate_keys: Iterable[str]) -> Optional[Any]:
    keys = set(candidate_keys)
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k in keys:
                return v
            found = find_first_key(v, keys)
            if found is not None:
                return found
    elif isinstance(obj, list):
        for item in obj:
            found = find_first_key(item, keys)
            if found is not None:
                return found
    return None


def find_all_strings(obj: Any) -> List[str]:
    out: List[str] = []
    if isinstance(obj, dict):
        for v in obj.values():
            out.extend(find_all_strings(v))
    elif isinstance(obj, list):
        for item in obj:
            out.extend(find_all_strings(item))
    elif isinstance(obj, str):
        out.append(obj)
    return out


def extract_session_id(events: List[Any]) -> Optional[str]:
    candidates = ["session_id", "sessionId", "id"]
    for ev in events:
        val = find_first_key(ev, candidates)
        if isinstance(val, str) and len(val) >= 6:
            return val
    return None


def extract_text_answer(events: List[Any]) -> str:
    # Heuristic: collect all strings under likely text-bearing keys.
    likely_keys = {
        "text", "content", "message", "output", "answer", "final",
        "summary", "delta", "response"
    }
    chunks: List[str] = []

    for ev in events:
        if isinstance(ev, dict):
            for k, v in walk(ev):
                if k in likely_keys and isinstance(v, str):
                    chunks.append(v)

    # Deduplicate while keeping order.
    seen = set()
    ordered: List[str] = []
    for c in chunks:
        c = c.strip()
        if not c or c in seen:
            continue
        seen.add(c)
        ordered.append(c)

    return "\n".join(ordered).strip()


def extract_token_usage(events: List[Any]) -> Dict[str, Optional[int]]:
    # Defensive heuristics because event shapes may vary by version/provider.
    text = "\n".join(json.dumps(ev, ensure_ascii=False) for ev in events)

    candidates = {
        "input_tokens": None,
        "output_tokens": None,
        "total_tokens": None,
    }

    # First, direct key search.
    direct_map = {
        "input_tokens": ["input_tokens", "prompt_tokens", "tokens_in"],
        "output_tokens": ["output_tokens", "completion_tokens", "tokens_out"],
        "total_tokens": ["total_tokens", "tokens", "token_usage_total"],
    }
    for target_key, keys in direct_map.items():
        for ev in events:
            val = find_first_key(ev, keys)
            if isinstance(val, int):
                candidates[target_key] = val
                break

    # Fallback regexes against the serialized stream.
    regexes = {
        "input_tokens": [r'"input_tokens"\s*:\s*(\d+)', r'"prompt_tokens"\s*:\s*(\d+)'],
        "output_tokens": [r'"output_tokens"\s*:\s*(\d+)', r'"completion_tokens"\s*:\s*(\d+)'],
        "total_tokens": [r'"total_tokens"\s*:\s*(\d+)', r'"tokens"\s*:\s*(\d+)'],
    }
    for key, patterns in regexes.items():
        if candidates[key] is None:
            for pat in patterns:
                m = re.search(pat, text)
                if m:
                    candidates[key] = int(m.group(1))
                    break

    # Derive total if possible.
    if candidates["total_tokens"] is None:
        if candidates["input_tokens"] is not None and candidates["output_tokens"] is not None:
            candidates["total_tokens"] = candidates["input_tokens"] + candidates["output_tokens"]

    return candidates


def extract_tool_calls(events: List[Any]) -> Tuple[Optional[int], List[str]]:
    names: List[str] = []
    for ev in events:
        strings = find_all_strings(ev)
        for s in strings:
            if any(tok in s.lower() for tok in ["tool", "mcp_", "pitlane", "grep", "glob", "read", "bash"]):
                names.append(s)

    # Also count explicit tool-ish keys if present.
    count = 0
    for ev in events:
        if isinstance(ev, dict):
            for k, v in walk(ev):
                if k in {"tool", "tool_name", "name"} and isinstance(v, str):
                    if any(x in v.lower() for x in ["mcp", "pitlane", "grep", "glob", "read", "bash", "edit", "webfetch"]):
                        count += 1

    count_val: Optional[int] = count if count > 0 else None
    return count_val, names[:200]


def run_once(
    repo: Path,
    out_dir: Path,
    target: Target,
    prompt_row: Dict[str, Any],
    run_index: int,
    agent: Optional[str],
    model: Optional[str],
    title_prefix: str,
    prompt_suffix: str,
    extra_args: List[str],
    dry_run: bool = False,
) -> Dict[str, Any]:
    prompt_id = str(prompt_row["id"])
    category = prompt_row.get("category")
    prompt_text = str(prompt_row["prompt"]).strip()
    full_prompt = prompt_text + ("\n\n" + prompt_suffix.strip() if prompt_suffix.strip() else "")
    title = f"{title_prefix}-{target.label}-{prompt_id}-r{run_index}"

    raw_dir = out_dir / "raw" / prompt_id / target.label / f"run_{run_index}"
    ensure_dir(raw_dir)

    cmd, env = build_opencode_command(
        target=target,
        full_prompt=full_prompt,
        agent=agent,
        model=model,
        title=title,
        extra_args=extra_args,
    )

    meta = {
        "cmd": cmd,
        "cwd": str(repo),
        "target": target.label,
        "target_value": target.value,
        "title": title,
    }
    (raw_dir / "meta.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")

    if dry_run:
        print("DRY RUN:", shlex.join(cmd))
        return {
            "prompt_id": prompt_id,
            "category": category,
            "target": target.label,
            "run_index": run_index,
            "title": title,
            "status": "dry_run",
        }

    start = time.perf_counter()
    proc = subprocess.run(
        cmd,
        cwd=str(repo),
        env=env,
        text=True,
        capture_output=True,
    )
    end = time.perf_counter()

    stdout = proc.stdout or ""
    stderr = proc.stderr or ""
    (raw_dir / "stdout.ndjson").write_text(stdout, encoding="utf-8")
    (raw_dir / "stderr.txt").write_text(stderr, encoding="utf-8")

    events = try_parse_json_lines(stdout)
    session_id = extract_session_id(events)
    usage = extract_token_usage(events)
    tool_count, tool_name_samples = extract_tool_calls(events)
    answer_text = extract_text_answer(events)

    row = {
        "prompt_id": prompt_id,
        "category": category,
        "target": target.label,
        "target_value": target.value,
        "run_index": run_index,
        "title": title,
        "status_code": proc.returncode,
        "latency_seconds": round(end - start, 3),
        "session_id": session_id,
        "input_tokens": usage["input_tokens"],
        "output_tokens": usage["output_tokens"],
        "total_tokens": usage["total_tokens"],
        "tool_call_count": tool_count,
        "answer_chars": len(answer_text),
        "answer_preview": answer_text[:800],
        "tool_name_samples": tool_name_samples[:30],
        "raw_dir": str(raw_dir),
    }

    return row


def write_results(out_dir: Path, rows: List[Dict[str, Any]]) -> None:
    ensure_dir(out_dir)
    jsonl_path = out_dir / "results.jsonl"
    with jsonl_path.open("w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

    csv_path = out_dir / "results.csv"
    fieldnames = [
        "prompt_id", "category", "target", "run_index", "status_code", "latency_seconds",
        "input_tokens", "output_tokens", "total_tokens", "tool_call_count",
        "answer_chars", "session_id", "raw_dir", "answer_preview",
    ]
    with csv_path.open("w", encoding="utf-8", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fieldnames)
        w.writeheader()
        for row in rows:
            w.writerow({k: row.get(k) for k in fieldnames})

    scores_path = out_dir / "scores_template.csv"
    score_fields = [
        "prompt_id", "category", "target", "run_index",
        "file_accuracy_1to5", "chain_accuracy_1to5", "test_relevance_1to5",
        "hallucination_resistance_1to5", "usefulness_1to5", "notes"
    ]
    with scores_path.open("w", encoding="utf-8", newline="") as f:
        w = csv.DictWriter(f, fieldnames=score_fields)
        w.writeheader()
        for row in rows:
            w.writerow({
                "prompt_id": row["prompt_id"],
                "category": row.get("category"),
                "target": row["target"],
                "run_index": row["run_index"],
            })


def summarize(rows: List[Dict[str, Any]]) -> str:
    lines = []
    by_target: Dict[str, List[Dict[str, Any]]] = {}
    for row in rows:
        by_target.setdefault(row["target"], []).append(row)

    lines.append("Summary by target:")
    for target, items in sorted(by_target.items()):
        latencies = [r["latency_seconds"] for r in items if isinstance(r.get("latency_seconds"), (int, float))]
        totals = [r["total_tokens"] for r in items if isinstance(r.get("total_tokens"), int)]
        avg_latency = round(sum(latencies) / len(latencies), 2) if latencies else None
        avg_total = round(sum(totals) / len(totals), 1) if totals else None
        lines.append(f"- {target}: runs={len(items)}, avg_latency={avg_latency}, avg_total_tokens={avg_total}")
    return "\n".join(lines)


def inject_agents_md(repo: Path, agents_md: Path, dry_run: bool = False) -> None:
    """Copy AGENTS.md into the target repository root so the agent can find it."""
    dest = repo / "AGENTS.md"
    if dry_run:
        print(f"DRY RUN: would copy {agents_md} -> {dest}")
        return
    shutil.copy2(agents_md, dest)
    print(f"Injected {agents_md.name} -> {dest}", flush=True)


def main() -> int:
    args = parse_args()
    repo = Path(args.repo).resolve()
    prompts_path = Path(args.prompts).resolve()
    out_dir = Path(args.out).resolve()
    ensure_dir(out_dir)

    # Inject AGENTS.md from the same directory as this script into the target repo.
    agents_md = Path(__file__).parent / "AGENTS.md"
    if agents_md.exists():
        inject_agents_md(repo, agents_md, dry_run=args.dry_run)
    else:
        print(f"Warning: AGENTS.md not found at {agents_md}, skipping injection.", flush=True)

    prompts = load_prompts(prompts_path)
    targets = parse_targets(args.target)

    rows: List[Dict[str, Any]] = []
    total_jobs = len(prompts) * len(targets) * args.runs
    job_no = 0

    for prompt_row in prompts:
        for target in targets:
            for run_index in range(1, args.runs + 1):
                job_no += 1
                print(f"[{job_no}/{total_jobs}] {prompt_row['id']} :: {target.label} :: run {run_index}", flush=True)
                row = run_once(
                    repo=repo,
                    out_dir=out_dir,
                    target=target,
                    prompt_row=prompt_row,
                    run_index=run_index,
                    agent=args.agent,
                    model=args.model,
                    title_prefix=args.title_prefix,
                    prompt_suffix=args.prompt_suffix,
                    extra_args=args.extra_arg,
                    dry_run=args.dry_run,
                )
                rows.append(row)
                write_results(out_dir, rows)

    summary = summarize(rows)
    (out_dir / "summary.txt").write_text(summary + "\n", encoding="utf-8")
    print()
    print(summary)
    print(f"\nWrote outputs to: {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
