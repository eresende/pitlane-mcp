"""Replay a preserved post-tool checkpoint for one tools-disabled synthesis.

This diagnostic does not navigate, grade automatically, or run benchmark arms.
Requests and reasoning stay in a fresh ignored artifact directory, never source.
"""
from __future__ import annotations

import argparse
import copy
import json
import os
from pathlib import Path
import subprocess
import time

from bench.harness.experiments.navigation_skill import ROOT, digest, dump, vram_samples
from bench.harness.experiments.provider import TracedProvider, ProviderError, read_key, usage_summary
from bench.harness.experiments.tools import tree_digest


SYNTHESIS_INSTRUCTION = (
    "Answer the original repository question now using only the evidence already "
    "retrieved in this conversation. No additional tools are available. Keep the "
    "final answer to at most 300 words: identify the essential source files and "
    "symbols, explain how directory traversal connects to ignore matching, and "
    "give the short relevant execution flow. Prioritize that connection over an "
    "exhaustive implementation tour. State any missing evidence or uncertainty "
    "explicitly; do not invent details."
)


def replay_payload(recorded, instruction=SYNTHESIS_INSTRUCTION):
    """Preserve every recorded message and inference option; append one instruction."""
    payload = copy.deepcopy(recorded)
    payload.pop("tools", None)
    payload.pop("tool_choice", None)
    payload["messages"].append({"role": "user", "content": instruction})
    return payload


def checkpoint_payload(source, arm, checkpoint):
    """Request N+1 contains results returned by all tool calls in request N."""
    wire = Path(source) / "instances" / "symbol_ignore_logic" / arm / "run_1" / "wire"
    path = wire / f"request_{checkpoint + 1:02}.json"
    recorded = json.loads(path.read_text())
    previous = json.loads((wire / f"response_{checkpoint:02}.json").read_text())
    calls = previous["choices"][0]["message"].get("tool_calls") or []
    if not calls:
        raise ValueError("Checkpoint must follow recorded tool calls")
    tail = recorded["messages"][-len(calls):]
    if [m.get("tool_call_id") for m in tail] != [c["id"] for c in calls] or any(m["role"] != "tool" for m in tail):
        raise ValueError("Checkpoint tool results do not match recorded response")
    return path, recorded


def response_summary(raw, backend, elapsed):
    choice = (raw.get("choices") or [{}])[0]
    msg = choice.get("message") or {}
    content = msg.get("content") or ""
    calls = msg.get("tool_calls") or []
    error = None
    try:
        backend.parse(raw)
    except ProviderError as exc:
        error = str(exc)
    accepted = error is None and not calls and choice.get("finish_reason") == "stop"
    return {
        "finish_reason": choice.get("finish_reason"), "accepted_terminal_response": accepted,
        "termination_scope": "Explicit synthesis instruction with tools disabled; not autonomous navigation completion",
        "error_classification": error or ("unexpected_tool_call" if calls else None),
        "tool_calls_returned": len(calls), "tool_calls_executed": 0,
        "usage": usage_summary(backend.usages), "elapsed_seconds": elapsed,
        "assistant_content_bytes": len(content.encode()),
        "reasoning_content_bytes": len((msg.get("reasoning_content") or "").encode()),
        "final_answer": content if accepted else "",
        "correctness_score": None, "complete_enough_for_task": None,
        "review": "Manual repository-grounded review required; truncation is never accepted",
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=os.environ.get("STRATA_BASE_URL"))
    parser.add_argument("--env-file")
    parser.add_argument("--api-key-env", default="STRATA_API_KEY")
    parser.add_argument("--source-run", required=True)
    parser.add_argument("--arm", choices=list("ABC"), default="C")
    parser.add_argument("--checkpoint", type=int, default=16, help="Replay context AFTER this request's tools")
    parser.add_argument("--out", required=True)
    args = parser.parse_args(argv)
    if not args.base_url:
        parser.error("Set STRATA_BASE_URL or --base-url")
    source, out = Path(args.source_run).resolve(), Path(args.out).resolve()
    if out.exists() or out.is_relative_to(source) or source.is_relative_to(out):
        parser.error("Use a new output directory separate from the preserved source run")
    if out.is_relative_to(ROOT) and subprocess.run(["git", "-C", str(ROOT), "check-ignore", "--quiet", str(out)]).returncode:
        parser.error("Diagnostic artifacts must be ignored or outside the repository")
    out.mkdir(parents=True, mode=0o700)
    source_hash = tree_digest(source)
    path, recorded = checkpoint_payload(source, args.arm, args.checkpoint)
    config = json.loads((source / "run_manifest.json").read_text())["config"]
    payload = replay_payload(recorded)
    for field, expected in (("model", config["model"]), ("temperature", config["temperature"]), ("max_tokens", config["max_output_tokens"]), ("stream", False)):
        if payload.get(field) != expected:
            raise ValueError("Recorded request disagrees with source manifest: " + field)
    backend = TracedProvider(args.base_url, payload["model"], read_key(args.api_key_env, args.env_file),
                             trace_dir=out / "wire", timeout=config["timeout_seconds"])
    models = backend.request("/models")
    model = next((m for m in models.get("data", []) if m.get("id") == payload["model"]), None)
    if model is None:
        raise ProviderError("requested_model_not_available")
    if config.get("verify_reported_context", True) is not False and model.get("meta", {}).get("n_ctx") != config["context_window"]:
        raise ProviderError("context_window_not_verified")
    dump(out / "manifest.json", {
        "source_run": str(source), "source_tree_sha256_before": source_hash,
        "source_request": str(path.relative_to(source)), "source_request_sha256": digest(path.read_bytes()),
        "checkpoint_after_request": args.checkpoint, "configuration_context": args.arm,
        "inherited_skill_context": args.arm == "C", "source_config": config,
        "model_discovery": model, "provider_endpoint_sha256": digest(args.base_url.encode()),
        "synthesis_instruction": SYNTHESIS_INSTRUCTION,
        "settings_preserved": {k: v for k, v in recorded.items() if k not in {"messages", "tools", "tool_choice"}},
        "context_transformation": "Unchanged recorded messages through checkpoint tools; remove tool definitions and tool_choice; append concise instruction",
    })
    start = time.perf_counter()
    vram_before = vram_samples()
    try:
        raw = backend.request("/chat/completions", payload)
        row = response_summary(raw, backend, time.perf_counter() - start)
        dump(out / "observations.json", backend.observations)
        dump(out / "summary.json", row)
        dump(out / "environment.json", {"vram_before": vram_before, "vram_after": vram_samples(),
                                       "models_before": models, "models_after": backend.request("/models"),
                                       "engine_configuration_changed_by_diagnostic": False})
        print(json.dumps({k: v for k, v in row.items() if k not in {"final_answer", "review"}}, indent=2))
    finally:
        after = tree_digest(source)
        dump(out / "source_integrity.json", {"before": source_hash, "after": after, "unchanged": after == source_hash})
        if source_hash != after:
            raise ValueError("Preserved source run changed")


if __name__ == "__main__":
    main()
