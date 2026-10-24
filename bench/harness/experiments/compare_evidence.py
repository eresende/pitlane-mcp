"""Four synthesis-only requests: accumulated history versus lossless source extraction.

The compact input contains recorded source/signatures, never new repository reads
or reviewer explanations. Snapshot reads validate provenance only, not model input.
"""
from __future__ import annotations

import argparse
import copy
from dataclasses import asdict
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time

from bench.harness.experiments.completion_guidance import CAUSAL_GUIDANCE
from bench.harness.experiments.compare_synthesis import DOCKER_VERIFY
from bench.harness.experiments.diagnose_synthesis import checkpoint_payload, replay_payload, response_summary
from bench.harness.experiments.navigation_skill import ROOT, digest, dump, vram_samples
from bench.harness.experiments.provider import CompatibleProvider, TracedProvider, read_key
from bench.harness.experiments.tools import tree_digest
from bench.harness.framework.quality_scorer import QualityScorer


def extract_recorded_source(recorded, snapshot):
    """Extract every returned source block, positive text hit and source signature.

    Retain first-seen order, deduplicate identical snippets only. Nothing is chosen
    by reviewer relevance or a desired answer. File/line labels are mechanical.
    """
    snapshot = Path(snapshot).resolve()
    calls = {c["id"]: c["function"] for m in recorded["messages"] for c in m.get("tool_calls", [])}
    blocks, seen = [], {}
    containers = []

    def add(file, text, origin, kind, start=None):
        path = Path(file)
        path = path if path.is_absolute() else snapshot / path
        relative = str(path.resolve().relative_to(snapshot))
        if not isinstance(text, str) or not text.strip():
            return
        actual = path.read_text()
        if kind == "signature":
            if re.sub(r"\s+", " ", text).strip() not in re.sub(r"\s+", " ", actual):
                raise ValueError("Recorded signature does not match snapshot")
        else:
            lines = text.splitlines()
            expected = actual.splitlines()[start - 1:start - 1 + len(lines)]
            if len(lines) != len(expected) or any(
                (a.lstrip() if i == 0 else a) != (b.lstrip() if i == 0 else b)
                for i, (a, b) in enumerate(zip(lines, expected))
            ):
                raise ValueError("Recorded source does not match snapshot: " + relative)
        key = (relative, text)
        if key in seen:
            blocks[seen[key]]["origins"].append(origin)
            return
        seen[key] = len(blocks)
        blocks.append({"file": relative, "text": text, "kind": kind, "line_start": start,
                       "sha256": digest(text.encode()), "origins": [origin]})

    for message in recorded["messages"]:
        if message["role"] != "tool":
            continue
        origin = message["tool_call_id"]
        function = calls[origin]
        try:
            data = json.loads(message["content"])
        except ValueError:
            data = None
        before = sum(len(b["origins"]) for b in blocks)
        if isinstance(data, dict):
            if isinstance(data.get("source"), str):
                add(data["file"], data["source"], origin, "source", data["line_start"])
            if function["name"] == "investigate":
                metadata = [s for s in data["symbols"] if s.get("lines_shown", 0) > 0]
                code = re.findall(r"```rust\n(.*?)\n```", data["answer"], re.S)
                if len(metadata) != len(code):
                    raise ValueError("Investigation blocks and provenance do not align")
                for symbol, text in zip(metadata, code):
                    # This is generated truncation metadata, not returned Rust source.
                    text = re.sub(r"\n\n// \.\.\. \(\d+ more lines\)$", "", text)
                    add(symbol["file"], text, origin, "source", symbol["line_start"])
            for hit in data.get("matches", []):
                rows = hit.get("before", []) + [{"line": hit["line"], "text": hit["line_text"]}] + hit.get("after", [])
                for line in rows:
                    add(hit["file"], line["text"], origin, "source", line["line"])
            for symbol in data.get("symbols", []) + data.get("results", []):
                if isinstance(symbol.get("signature"), str):
                    add(symbol.get("file", data.get("file")), symbol["signature"], origin, "signature")
        elif function["name"] == "grep_search":
            for line in message["content"].splitlines():
                match = re.match(r"^([^:]+):(\d+):(.*)$", line)
                if match:
                    add(match[1], match[3], origin, "source", int(match[2]))
        added = sum(len(b["origins"]) for b in blocks) - before
        containers.append({"tool_call_id": origin, "tool_name": function["name"], "retained_source_items": added})
    text = "\n\n".join(
        f"{b['file']}" + (f":{b['line_start']}" if b['line_start'] else "")
        + "\n```rust\n" + b["text"] + "\n```" for b in blocks
    )
    return text, {"source_blocks": blocks, "tool_response_coverage": containers,
                  "method": "All recorded source, positive text hits and source signatures; first-seen order; exact-snippet deduplication; no reviewer selection or added facts"}


def presentation_payload(recorded, presentation, compact):
    payload = replay_payload(recorded, CAUSAL_GUIDANCE)
    if any(k in payload for k in ("reasoning_effort", "reasoning", "chat_template_kwargs")):
        raise ValueError("The approved diagnostic uses the existing server default")
    if presentation == "compact":
        payload["messages"] = copy.deepcopy(recorded["messages"][:2]) + [
            {"role": "user", "content": compact}, {"role": "user", "content": CAUSAL_GUIDANCE}]
    elif presentation != "accumulated":
        raise ValueError("Unknown presentation")
    return payload


def default_xhigh_verified(proof, props, previous):
    return (proof.get("source_hashes") == previous["proof"]["source_hashes"]
            and proof.get("template_sha256") == previous["proof"]["template_sha256"]
            == hashlib.sha256(props["chat_template"].encode()).hexdigest()
            and proof.get("config_reasoning_effort") is None
            and all(r.get("template_kwargs") == {} and r.get("xhigh_instruction_present") is True
                    for r in proof.get("payloads", {}).values())
            and len(proof.get("payloads", {})) == 2)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=os.environ.get("STRATA_BASE_URL"))
    parser.add_argument("--env-file")
    parser.add_argument("--api-key-env", default="STRATA_API_KEY")
    parser.add_argument("--source-run", required=True)
    parser.add_argument("--reasoning-proof", required=True, help="Previously reviewed Docker verification JSON")
    parser.add_argument("--container", default="strata")
    parser.add_argument("--out", required=True)
    args = parser.parse_args(argv)
    if not args.base_url:
        parser.error("Set STRATA_BASE_URL or --base-url")
    source, out = Path(args.source_run).resolve(), Path(args.out).resolve()
    if out.exists() or out.is_relative_to(source) or source.is_relative_to(out):
        parser.error("Use a fresh separate output directory")
    if out.is_relative_to(ROOT) and subprocess.run(["git", "-C", str(ROOT), "check-ignore", "--quiet", str(out)]).returncode:
        parser.error("Artifacts must be ignored or outside the repository")
    before = tree_digest(source)
    source_path, recorded = checkpoint_payload(source, "C", 16)
    compact, audit = extract_recorded_source(recorded, source / "repository")
    payloads = {p: presentation_payload(recorded, p, compact) for p in ("accumulated", "compact")}
    if len(json.dumps(payloads["compact"])) >= len(json.dumps(payloads["accumulated"])):
        raise ValueError("Extraction did not produce a smaller context")
    if any(p.get("temperature") != 0 or p.get("max_tokens") != 4096 or p.get("stream") is not False for p in payloads.values()):
        raise ValueError("Recorded inference options differ from the approved profile")
    key = read_key(args.api_key_env, args.env_file)
    api = CompatibleProvider(args.base_url.removesuffix("/v1"), recorded["model"], key, timeout=300)
    props, settings, state = [api.request(route) for route in ("/props", "/settings", "/v1/status")]
    if settings.get("defaults") or state.get("activity", {}).get("in_flight") or not state.get("loaded"):
        raise ValueError("Requires unchanged empty shared defaults and an already loaded idle model")
    if state["model"] != recorded["model"] or state["context"]["native"] != 131072:
        raise ValueError("Model/context changed")
    out.mkdir(parents=True, mode=0o700)
    previous = json.loads(Path(args.reasoning_proof).read_text())
    check = subprocess.run(["docker", "exec", "-i", args.container, "/opt/strata/.venv/bin/python", "-B", "-c", DOCKER_VERIFY],
                           input=json.dumps({"shared_defaults": {}, "payloads": payloads}).encode(), capture_output=True, timeout=60)
    if check.returncode:
        raise ValueError("Docker CPU-only verification failed")
    proof = json.loads(check.stdout)
    if not default_xhigh_verified(proof, props, previous):
        raise ValueError("Default xhigh deployment verification failed")
    dump(out / "evidence_audit.json", audit)
    (out / "compact_evidence.txt").write_text(compact)
    dump(out / "reasoning_verification.json", {"proof": proof, "props": props, "settings": settings, "status_before": state})
    schedule = [("accumulated", 1), ("compact", 1), ("compact", 2), ("accumulated", 2)]
    dump(out / "manifest.json", {"source_run": str(source), "checkpoint_request_sha256": digest(source_path.read_bytes()),
        "source_tree_sha256_before": before, "recorded_messages_sha256": digest(recorded["messages"]),
        "source_evidence_sha256": digest(compact.encode()), "endpoint_sha256": digest(args.base_url.encode()),
        "model": recorded["model"], "schedule": schedule, "default_reasoning": "xhigh; template verified",
        "guidance": CAUSAL_GUIDANCE, "scope": "Evidence-presentation synthesis diagnostic; no navigation benchmark",
        "evaluation_only_topics_not_added_to_input": ["hidden-file handling", "rule precedence", "directory pruning", "matcher-state transitions"]})
    rows = []
    try:
        for presentation, repetition in schedule:
            label = f"{presentation}_{repetition}"
            print("Synthesis diagnostic " + label, flush=True)
            pre = api.request("/v1/status")
            if pre["started"] != state["started"] or not pre["loaded"] or pre["activity"]["in_flight"] or api.request("/settings") != settings:
                raise ValueError("Server state changed or another generation is active")
            provider = TracedProvider(args.base_url, recorded["model"], key, trace_dir=out / label / "wire", timeout=300)
            gpu_before = vram_samples()
            started = time.perf_counter()
            raw = provider.request("/chat/completions", payloads[presentation])
            row = response_summary(raw, provider, time.perf_counter() - started)
            expected = proof["payloads"][presentation]["expected_prompt_tokens"]
            row.update({"presentation": presentation, "repetition": repetition,
                        "requested_reasoning_effort": "omitted/server-default", "expected_prompt_tokens": expected,
                        "verified_effective_reasoning_effort": "xhigh" if row["usage"]["prompt_tokens"] == expected else None,
                        "quality": asdict(QualityScorer().score(row["final_answer"], str(source / "repository"), "symbol_grounding"))})
            rows.append(row)
            dump(out / label / "summary.json", row)
            dump(out / label / "observations.json", provider.observations)
            dump(out / label / "environment.json", {"status_before": pre, "status_after": api.request("/v1/status"),
                                                    "gpu_before": gpu_before, "gpu_after": vram_samples()})
            dump(out / "results.json", rows)
            print(json.dumps({k: row[k] for k in ("presentation", "repetition", "finish_reason", "usage", "elapsed_seconds", "verified_effective_reasoning_effort")}), flush=True)
            if row["verified_effective_reasoning_effort"] is None:
                raise ValueError("Live input count disagrees with deployed renderer; stop experiment")
    finally:
        after = tree_digest(source)
        dump(out / "source_integrity.json", {"before": before, "after": after, "unchanged": before == after})
        dump(out / "environment_after.json", {"settings": api.request("/settings"), "status": api.request("/v1/status")})
        if after != before:
            raise ValueError("Preserved checkpoint artifacts changed")


if __name__ == "__main__":
    main()
