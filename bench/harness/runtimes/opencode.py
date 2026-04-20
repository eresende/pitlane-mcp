"""Shared OpenCode runtime implementation."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from bench.harness.framework.models import (
    BenchmarkConfig,
    Message,
    RunResult,
    TokenUsage,
    ToolCall,
    ToolCallRecord,
)
from bench.harness.framework.output_writer import OutputWriter
from bench.harness.manifest import build_run_manifest
from bench.harness.resume import instance_is_complete, load_instance_artifacts
from bench.harness.runtimes.base import RuntimeRequest


@dataclass
class Target:
    label: str
    value: str

    @property
    def is_attach(self) -> bool:
        return self.value.startswith("http://") or self.value.startswith("https://")

    @property
    def is_config(self) -> bool:
        return self.value.startswith("CONFIG:")

    @property
    def config_path(self) -> str | None:
        if self.is_config:
            return self.value.split("CONFIG:", 1)[1]
        return None

    def resolved_config_path(self) -> str | None:
        """Return an absolute config path for CONFIG: targets."""
        config_path = self.config_path
        if config_path is None:
            return None
        return str(Path(config_path).expanduser().resolve())


def add_opencode_arguments(parser: argparse.ArgumentParser) -> argparse.ArgumentParser:
    """Register OpenCode-specific CLI flags on an existing parser."""
    parser.add_argument(
        "--target",
        action="append",
        default=[],
        help="OpenCode target in the form label=value. Value can be http://host:port or CONFIG:/path/to/config.json.",
    )
    parser.add_argument("--agent", default=None, help="OpenCode agent to use, e.g. build.")
    parser.add_argument("--title-prefix", default="bench", help="Session title prefix.")
    parser.add_argument("--prompt-suffix", default="", help="Suffix appended to every prompt.")
    parser.add_argument(
        "--extra-arg",
        action="append",
        default=[],
        dest="runtime_extra_args",
        help="Extra argument passed through to `opencode run`.",
    )
    parser.add_argument("--dry-run", action="store_true", help="Print commands without executing them.")
    return parser


def parse_targets(raw_targets: list[str]) -> list[Target]:
    targets: list[Target] = []
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


def _ensure_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)


def _run_cmd(*args: str) -> str | None:
    try:
        result = subprocess.run(
            list(args),
            capture_output=True,
            text=True,
            timeout=10,
        )
    except Exception:  # noqa: BLE001
        return None
    if result.returncode != 0:
        return None
    return result.stdout.strip() or None


def _sha256_file(path: str | Path) -> str:
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        while chunk := handle.read(8192):
            digest.update(chunk)
    return digest.hexdigest()


def _detect_git_commit(repo_path: str | Path) -> str | None:
    return _run_cmd("git", "-C", str(repo_path), "rev-parse", "HEAD")


def _detect_git_clean(repo_path: str | Path) -> bool | None:
    status = _run_cmd("git", "-C", str(repo_path), "status", "--porcelain")
    if status is None:
        return None
    return status == ""


def _detect_pitlane_version() -> str | None:
    return _run_cmd("pitlane-mcp", "--version")


def _detect_ollama_version() -> str | None:
    return _run_cmd("ollama", "--version")


def build_opencode_command(
    *,
    target: Target,
    full_prompt: str,
    agent: str | None,
    model: str | None,
    title: str,
    extra_args: list[str],
) -> tuple[list[str], dict[str, str]]:
    cmd = ["opencode", "run", "--format", "json", "--title", title]
    env = os.environ.copy()

    if target.is_attach:
        cmd += ["--attach", target.value]
    elif target.is_config:
        env["OPENCODE_CONFIG"] = str(target.resolved_config_path())
    else:
        raise ValueError(f"Unsupported target value: {target.value}")

    if agent:
        cmd += ["--agent", agent]
    if model:
        cmd += ["--model", model]

    cmd += extra_args
    cmd += [full_prompt]
    return cmd, env


def _try_parse_json_lines(text: str) -> list[Any]:
    events: list[Any] = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return events


def _walk(obj: Any):
    if isinstance(obj, dict):
        for key, value in obj.items():
            yield key, value
            yield from _walk(value)
    elif isinstance(obj, list):
        for item in obj:
            yield from _walk(item)


def _find_first_key(obj: Any, candidate_keys: list[str]) -> Any | None:
    keys = set(candidate_keys)
    if isinstance(obj, dict):
        for key, value in obj.items():
            if key in keys:
                return value
            found = _find_first_key(value, candidate_keys)
            if found is not None:
                return found
    elif isinstance(obj, list):
        for item in obj:
            found = _find_first_key(item, candidate_keys)
            if found is not None:
                return found
    return None


def _find_all_strings(obj: Any) -> list[str]:
    out: list[str] = []
    if isinstance(obj, dict):
        for value in obj.values():
            out.extend(_find_all_strings(value))
    elif isinstance(obj, list):
        for item in obj:
            out.extend(_find_all_strings(item))
    elif isinstance(obj, str):
        out.append(obj)
    return out


def extract_session_id(events: list[Any]) -> str | None:
    candidates = ["session_id", "sessionId", "id"]
    for event in events:
        value = _find_first_key(event, candidates)
        if isinstance(value, str) and len(value) >= 6:
            return value
    return None


def extract_text_answer(events: list[Any]) -> str:
    likely_keys = {
        "text",
        "content",
        "message",
        "output",
        "answer",
        "final",
        "summary",
        "delta",
        "response",
    }
    chunks: list[str] = []

    for event in events:
        if isinstance(event, dict):
            for key, value in _walk(event):
                if key in likely_keys and isinstance(value, str):
                    chunks.append(value)

    seen: set[str] = set()
    ordered: list[str] = []
    for chunk in chunks:
        chunk = chunk.strip()
        if not chunk or chunk in seen:
            continue
        seen.add(chunk)
        ordered.append(chunk)
    return "\n".join(ordered).strip()


def extract_token_usage(events: list[Any]) -> dict[str, int | None]:
    text = "\n".join(json.dumps(event, ensure_ascii=False) for event in events)
    candidates: dict[str, int | None] = {
        "input_tokens": None,
        "output_tokens": None,
        "total_tokens": None,
    }
    direct_map = {
        "input_tokens": ["input_tokens", "prompt_tokens", "tokens_in"],
        "output_tokens": ["output_tokens", "completion_tokens", "tokens_out"],
        "total_tokens": ["total_tokens", "tokens", "token_usage_total"],
    }
    for target_key, keys in direct_map.items():
        for event in events:
            value = _find_first_key(event, keys)
            if isinstance(value, int):
                candidates[target_key] = value
                break

    regexes = {
        "input_tokens": [r'"input_tokens"\s*:\s*(\d+)', r'"prompt_tokens"\s*:\s*(\d+)'],
        "output_tokens": [r'"output_tokens"\s*:\s*(\d+)', r'"completion_tokens"\s*:\s*(\d+)'],
        "total_tokens": [r'"total_tokens"\s*:\s*(\d+)', r'"tokens"\s*:\s*(\d+)'],
    }
    for key, patterns in regexes.items():
        if candidates[key] is None:
            for pattern in patterns:
                match = re.search(pattern, text)
                if match:
                    candidates[key] = int(match.group(1))
                    break
    if candidates["total_tokens"] is None:
        if candidates["input_tokens"] is not None and candidates["output_tokens"] is not None:
            candidates["total_tokens"] = candidates["input_tokens"] + candidates["output_tokens"]
    return candidates


def extract_tool_calls(events: list[Any]) -> tuple[int | None, list[str]]:
    names: list[str] = []
    for event in events:
        for value in _find_all_strings(event):
            if any(token in value.lower() for token in ["tool", "mcp_", "pitlane", "grep", "glob", "read", "bash"]):
                names.append(value)

    count = 0
    for event in events:
        if isinstance(event, dict):
            for key, value in _walk(event):
                if key in {"tool", "tool_name", "name"} and isinstance(value, str):
                    if any(x in value.lower() for x in ["mcp", "pitlane", "grep", "glob", "read", "bash", "edit", "webfetch"]):
                        count += 1
    return (count if count > 0 else None), names[:200]


def _normalize_tool_name(name: str) -> str:
    """Normalize OpenCode-emitted tool names to canonical harness tool ids."""
    for prefix in ("pitlane-mcp_", "pitlane_", "mcp_"):
        if name.startswith(prefix):
            return name[len(prefix):]
    return name


def _tool_latency_ms(part: dict[str, Any]) -> float:
    timing = ((part.get("state") or {}).get("time") or {})
    start = timing.get("start")
    end = timing.get("end")
    if isinstance(start, (int, float)) and isinstance(end, (int, float)) and end >= start:
        return float(end - start)
    return 0.0


def normalize_opencode_events(events: list[Any]) -> tuple[list[Message], list[ToolCallRecord], str, TokenUsage]:
    """Convert OpenCode NDJSON events into canonical conversation artifacts."""
    ordered_message_ids: list[str] = []
    steps: dict[str, dict[str, Any]] = {}

    for event in events:
        if not isinstance(event, dict):
            continue
        event_type = event.get("type")
        part = event.get("part")
        if not isinstance(part, dict):
            continue
        message_id = part.get("messageID")
        if not isinstance(message_id, str):
            continue

        step = steps.setdefault(
            message_id,
            {
                "texts": [],
                "tools": [],
                "reason": None,
                "tokens": {},
            },
        )

        if event_type == "step_start" and message_id not in ordered_message_ids:
            ordered_message_ids.append(message_id)
        elif event_type == "text":
            text = part.get("text")
            if isinstance(text, str) and text:
                step["texts"].append(text)
        elif event_type == "tool_use":
            step["tools"].append(part)
        elif event_type == "step_finish":
            step["reason"] = part.get("reason")
            tokens = part.get("tokens")
            if isinstance(tokens, dict):
                step["tokens"] = tokens

    conversation: list[Message] = []
    tool_records: list[ToolCallRecord] = []
    final_answer = ""
    prompt_tokens = 0
    completion_tokens = 0
    total_tokens = 0

    for iteration, message_id in enumerate(ordered_message_ids, start=1):
        step = steps[message_id]
        text = "\n".join(chunk.strip() for chunk in step["texts"] if isinstance(chunk, str) and chunk.strip()).strip()
        tool_calls: list[ToolCall] = []

        for tool_part in step["tools"]:
            raw_name = tool_part.get("tool")
            if not isinstance(raw_name, str):
                continue
            state = tool_part.get("state") or {}
            arguments = state.get("input")
            if not isinstance(arguments, dict):
                arguments = {}
            call_id = tool_part.get("callID")
            if not isinstance(call_id, str):
                call_id = f"{message_id}:{len(tool_calls)}"

            tool_calls.append(
                ToolCall(
                    id=call_id,
                    name=_normalize_tool_name(raw_name),
                    arguments=arguments,
                )
            )

            output = state.get("output")
            output_text = output if isinstance(output, str) else json.dumps(output, ensure_ascii=False) if output is not None else ""

            tool_records.append(
                ToolCallRecord(
                    iteration=iteration,
                    tool_name=_normalize_tool_name(raw_name),
                    arguments=arguments,
                    result_bytes=len(output_text.encode("utf-8")),
                    latency_ms=_tool_latency_ms(tool_part),
                )
            )

        assistant_msg = Message(
            role="assistant",
            content=text,
            tool_calls=tool_calls if tool_calls else None,
        )
        conversation.append(assistant_msg)

        for tool_part in step["tools"]:
            state = tool_part.get("state") or {}
            call_id = tool_part.get("callID")
            if not isinstance(call_id, str):
                continue
            output = state.get("output")
            output_text = output if isinstance(output, str) else json.dumps(output, ensure_ascii=False) if output is not None else ""
            conversation.append(
                Message(
                    role="tool",
                    content=output_text,
                    tool_call_id=call_id,
                )
            )

        if not step["tools"] and text:
            final_answer = text

        tokens = step.get("tokens") or {}
        prompt_tokens += int(tokens.get("input") or 0)
        completion_tokens += int(tokens.get("output") or 0)
        total_tokens += int(tokens.get("total") or 0)

    usage = TokenUsage(
        prompt_tokens=prompt_tokens,
        completion_tokens=completion_tokens,
        total_tokens=total_tokens or (prompt_tokens + completion_tokens),
    )
    return conversation, tool_records, final_answer, usage


def inject_agents_md(repo: Path, agents_md: Path, dry_run: bool = False) -> None:
    """Copy AGENTS.md into the target repository root so the agent can find it."""
    dest = repo / "AGENTS.md"
    if dry_run:
        print(f"DRY RUN: would copy {agents_md} -> {dest}")
        return
    shutil.copy2(agents_md, dest)
    print(f"Injected {agents_md.name} -> {dest}", flush=True)


def run_once(
    *,
    repo: Path,
    out_dir: Path,
    target: Target,
    prompt_row: dict[str, Any],
    run_index: int,
    agent: str | None,
    model: str | None,
    title_prefix: str,
    prompt_suffix: str,
    extra_args: list[str],
    dry_run: bool = False,
) -> dict[str, Any]:
    prompt_id = str(prompt_row["id"])
    category = prompt_row.get("category")
    prompt_text = str(prompt_row["prompt"]).strip()
    full_prompt = prompt_text + ("\n\n" + prompt_suffix.strip() if prompt_suffix.strip() else "")
    title = f"{title_prefix}-{target.label}-{prompt_id}-r{run_index}"

    raw_dir = out_dir / "raw" / prompt_id / target.label / f"run_{run_index}"
    _ensure_dir(raw_dir)

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
            "target_value": target.value,
            "run_index": run_index,
            "title": title,
            "status_code": 0,
            "latency_seconds": 0.0,
            "session_id": None,
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "tool_call_count": 0,
            "answer_chars": 0,
            "answer_preview": "",
            "tool_name_samples": [],
            "raw_dir": str(raw_dir),
            "status": "dry_run",
        }

    start = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(repo),
            env=env,
            text=True,
            capture_output=True,
            timeout=600,  # 10 min timeout to handle Bedrock throttling
        )
    except subprocess.TimeoutExpired as e:
        end = time.perf_counter()
        stdout = (e.stdout or "") if isinstance(e.stdout, str) else (e.stdout or b"").decode("utf-8", errors="replace")
        stderr = (e.stderr or "") if isinstance(e.stderr, str) else (e.stderr or b"").decode("utf-8", errors="replace")
        (raw_dir / "stdout.ndjson").write_text(stdout, encoding="utf-8")
        (raw_dir / "stderr.txt").write_text(f"TIMEOUT after {request.timeout_seconds}s\n{stderr}", encoding="utf-8")
        proc = None
    else:
        end = time.perf_counter()
        stdout = proc.stdout or ""
        stderr = proc.stderr or ""
        (raw_dir / "stdout.ndjson").write_text(stdout, encoding="utf-8")
        (raw_dir / "stderr.txt").write_text(stderr, encoding="utf-8")

    events = _try_parse_json_lines(stdout)
    session_id = extract_session_id(events)
    conversation, tool_records, answer_text, normalized_usage = normalize_opencode_events(events)
    fallback_usage = extract_token_usage(events)
    usage = {
        "input_tokens": normalized_usage.prompt_tokens or fallback_usage["input_tokens"],
        "output_tokens": normalized_usage.completion_tokens or fallback_usage["output_tokens"],
        "total_tokens": normalized_usage.total_tokens or fallback_usage["total_tokens"],
    }
    tool_count = len(tool_records)
    tool_name_samples = [record.tool_name for record in tool_records[:30]]

    row = {
        "prompt_id": prompt_id,
        "category": category,
        "target": target.label,
        "target_value": target.value,
        "run_index": run_index,
        "title": title,
        "status_code": proc.returncode if proc else -1,
        "latency_seconds": round(end - start, 3),
        "session_id": session_id,
        "input_tokens": usage["input_tokens"],
        "output_tokens": usage["output_tokens"],
        "total_tokens": usage["total_tokens"],
        "tool_call_count": tool_count,
        "answer_chars": len(answer_text),
        "answer_preview": answer_text[:800],
        "tool_name_samples": tool_name_samples[:30],
        "conversation": [
            {
                "role": msg.role,
                "content": msg.content,
                "tool_calls": [
                    {"id": tc.id, "name": tc.name, "arguments": tc.arguments}
                    for tc in (msg.tool_calls or [])
                ] or None,
                "tool_call_id": msg.tool_call_id,
            }
            for msg in conversation
        ],
        "tool_calls": [
            {
                "iteration": record.iteration,
                "tool_name": record.tool_name,
                "arguments": record.arguments,
                "result_bytes": record.result_bytes,
                "latency_ms": record.latency_ms,
            }
            for record in tool_records
        ],
        "raw_dir": str(raw_dir),
    }
    return row


def summarize(results: list[RunResult]) -> str:
    lines = ["Summary by mode:"]
    by_mode: dict[str, list[RunResult]] = {}
    for result in results:
        by_mode.setdefault(result.mode, []).append(result)
    for mode, items in sorted(by_mode.items()):
        latencies = [result.wall_clock_seconds for result in items]
        totals = [result.token_usage.total_tokens for result in items]
        avg_latency = round(sum(latencies) / len(latencies), 2) if latencies else None
        avg_total = round(sum(totals) / len(totals), 1) if totals else None
        lines.append(f"- {mode}: runs={len(items)}, avg_latency={avg_latency}, avg_total_tokens={avg_total}")
    return "\n".join(lines)


def _build_config(request: RuntimeRequest, prompt_count: int) -> BenchmarkConfig:
    repo = Path(request.repo_path).resolve()
    harness_root = Path(__file__).resolve().parents[3]
    provider = request.model_name.split("/", 1)[0] if "/" in request.model_name else "unknown"
    return BenchmarkConfig(
        model_name=request.model_name,
        model_provider=provider,
        backend_type=request.backend_type,
        repo_path=str(repo),
        repo_commit=_detect_git_commit(repo),
        repo_clean=_detect_git_clean(repo),
        harness_commit=_detect_git_commit(harness_root),
        harness_clean=_detect_git_clean(harness_root),
        pitlane_version=_detect_pitlane_version(),
        ollama_version=_detect_ollama_version(),
        prompt_set_path=str(Path(request.prompt_set_path).resolve()),
        prompt_set_sha256=_sha256_file(request.prompt_set_path),
        prompt_count=prompt_count,
        runs_per_prompt=request.runs_per_prompt,
        max_iterations=request.max_iterations,
        timeout_seconds=request.timeout_seconds,
        temperature=request.temperature,
        context_window=request.context_window,
        gpu_name=None,
        gpu_vram_gb=None,
        cpu_model=None,
        ram_gb=None,
        timestamp=datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    )


def _row_to_run_result(row: dict[str, Any]) -> RunResult:
    prompt_tokens = row.get("input_tokens") or 0
    completion_tokens = row.get("output_tokens") or 0
    total_tokens = row.get("total_tokens")
    if total_tokens is None:
        total_tokens = prompt_tokens + completion_tokens
    answer_preview = row.get("answer_preview") or ""
    status = str(row.get("status") or ("completed" if row.get("status_code", 1) == 0 else "error"))
    error = None if status in {"completed", "dry_run"} else f"opencode_exit_code={row.get('status_code')}"
    conversation = []
    for item in row.get("conversation") or []:
        if not isinstance(item, dict):
            continue
        tool_calls = item.get("tool_calls")
        parsed_tool_calls = None
        if isinstance(tool_calls, list):
            parsed_tool_calls = []
            for tc in tool_calls:
                if not isinstance(tc, dict):
                    continue
                parsed_tool_calls.append(
                    ToolCall(
                        id=str(tc.get("id") or ""),
                        name=str(tc.get("name") or ""),
                        arguments=tc.get("arguments") if isinstance(tc.get("arguments"), dict) else {},
                    )
                )
            if not parsed_tool_calls:
                parsed_tool_calls = None
        conversation.append(
            Message(
                role=str(item.get("role") or "assistant"),
                content=str(item.get("content") or ""),
                tool_calls=parsed_tool_calls,
                tool_call_id=str(item.get("tool_call_id")) if item.get("tool_call_id") is not None else None,
            )
        )
    tool_records = []
    for item in row.get("tool_calls") or []:
        if not isinstance(item, dict):
            continue
        tool_records.append(
            ToolCallRecord(
                iteration=int(item.get("iteration") or 0),
                tool_name=str(item.get("tool_name") or ""),
                arguments=item.get("arguments") if isinstance(item.get("arguments"), dict) else {},
                result_bytes=int(item.get("result_bytes") or 0),
                latency_ms=float(item.get("latency_ms") or 0.0),
            )
        )
    return RunResult(
        prompt_id=str(row["prompt_id"]),
        mode=str(row["target"]),
        run_index=int(row["run_index"]) - 1,
        status=status,
        final_answer=answer_preview,
        conversation=conversation,
        tool_calls=tool_records,
        token_usage=TokenUsage(
            prompt_tokens=int(prompt_tokens),
            completion_tokens=int(completion_tokens),
            total_tokens=int(total_tokens),
        ),
        total_context_bytes=sum(record.result_bytes for record in tool_records),
        wall_clock_seconds=float(row.get("latency_seconds") or 0.0),
        error=error,
    )


def execute_opencode_request(request: RuntimeRequest, *, agents_md_path: Path | None = None) -> int:
    """Run the OpenCode benchmark flow for a normalized runtime request."""
    repo = Path(request.repo_path).resolve()
    prompts_path = Path(request.prompt_set_path).resolve()
    out_dir = Path(request.output_dir).resolve()
    _ensure_dir(out_dir)
    writer = OutputWriter(str(out_dir))

    if agents_md_path is not None and agents_md_path.exists():
        inject_agents_md(repo, agents_md_path, dry_run=request.dry_run)

    with prompts_path.open("r", encoding="utf-8") as handle:
        prompts = [json.loads(line) for line in handle if line.strip()]
    if request.prompt_ids:
        selected = set(request.prompt_ids)
        prompts = [prompt for prompt in prompts if str(prompt.get("id")) in selected]
    targets = parse_targets(request.target_specs)
    if not targets:
        raise ValueError("OpenCode runtime requires at least one --target label=value")

    config = _build_config(request, len(prompts))
    writer.write_config(config)
    writer.write_run_manifest(
        build_run_manifest(
            suite_id=request.suite_id,
            suite_manifest_path=request.suite_manifest_path,
            repo_path=request.repo_path,
            prompt_set_path=request.prompt_set_path,
            model_name=request.model_name,
            backend_type=request.backend_type,
            runtime_type=request.runtime_type,
            mode=request.mode,
            runs_per_prompt=request.runs_per_prompt,
            max_iterations=request.max_iterations,
            timeout_seconds=request.timeout_seconds,
            temperature=request.temperature,
            context_window=request.context_window,
            scorer_version=request.scorer_version,
            prompt_filter=request.prompt_ids,
            resume_enabled=request.resume,
            force_enabled=request.force,
        )
    )

    results: list[RunResult] = []
    total_jobs = len(prompts) * len(targets) * request.runs_per_prompt
    job_no = 0

    for prompt_row in prompts:
        prompt_id = str(prompt_row.get("id"))
        for target in targets:
            for run_index in range(1, request.runs_per_prompt + 1):
                job_no += 1
                print(f"[{job_no}/{total_jobs}] {prompt_id} :: {target.label} :: run {run_index}", flush=True)
                if request.resume and not request.force and instance_is_complete(
                    out_dir,
                    prompt_id,
                    target.label,
                    run_index - 1,
                ):
                    result, _ = load_instance_artifacts(
                        out_dir,
                        prompt_id,
                        target.label,
                        run_index - 1,
                    )
                    results.append(result)
                    continue
                row = run_once(
                    repo=repo,
                    out_dir=out_dir,
                    target=target,
                    prompt_row=prompt_row,
                    run_index=run_index,
                    agent=request.agent,
                    model=request.model_name,
                    title_prefix=request.title_prefix,
                    prompt_suffix=request.prompt_suffix,
                    extra_args=request.runtime_extra_args,
                    dry_run=request.dry_run,
                )
                result = _row_to_run_result(row)
                writer.write_run(result)
                results.append(result)

    writer.write_results_jsonl(results)
    summary = summarize(results)
    (out_dir / "summary.txt").write_text(summary + "\n", encoding="utf-8")
    print()
    print(summary)
    print(f"\nWrote outputs to: {out_dir}")
    return 0


class OpenCodeRuntime:
    """Runtime adapter for OpenCode-backed benchmark execution."""

    name = "opencode"

    def run(self, request: RuntimeRequest) -> None:
        execute_opencode_request(
            request,
            agents_md_path=Path(__file__).resolve().parents[1] / "AGENTS.md",
        )
