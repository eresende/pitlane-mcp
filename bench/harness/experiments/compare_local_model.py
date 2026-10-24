"""Loaded-only LM Studio preflight and four fixed-evidence synthesis requests.

Never downloads/loads/unloads a model or writes inference settings. Request bodies
come directly from the reviewed evidence-presentation diagnostic; only model ID
changes. Raw conversations/reasoning remain in a fresh ignored artifact directory.
"""
from __future__ import annotations

import argparse
import copy
from dataclasses import asdict
import json
import os
from pathlib import Path
import struct
import subprocess
import time

from bench.harness.experiments.diagnose_synthesis import response_summary
from bench.harness.experiments.navigation_skill import ROOT, digest, dump, vram_samples
from bench.harness.experiments.provider import CompatibleProvider, TracedProvider, usage_summary
from bench.harness.experiments.tools import tree_digest
from bench.harness.framework.quality_scorer import QualityScorer


def loaded_model(native, legacy, identifier):
    """Fail closed before generation, so a request cannot trigger JIT loading."""
    for model in native.get("models", []):
        for instance in model.get("loaded_instances", []):
            if instance.get("id") != identifier:
                continue
            old = next((m for m in legacy.get("data", []) if m.get("id") == identifier), {})
            if model.get("quantization", {}).get("name") != "Q4_K_M" or old.get("quantization") != "Q4_K_M":
                raise ValueError("Loaded model is not the requested Q4_K_M quantization")
            if instance.get("config", {}).get("context_length", 0) < 131072 or old.get("loaded_context_length", 0) < 131072:
                raise ValueError("Loaded context is smaller than the controlled 131072 profile")
            if old.get("state") != "loaded" or not model.get("capabilities", {}).get("trained_for_tool_use"):
                raise ValueError("Loaded state/tool capability metadata missing")
            return {"model": model, "instance": instance, "legacy": old}
    raise ValueError("Exact requested model is not already loaded; no automatic load is permitted")


def replay_for_model(recorded, identifier):
    payload = copy.deepcopy(recorded)
    if payload.get("tools") or payload.get("tool_choice") or any(k in payload for k in ("reasoning_effort", "reasoning", "chat_template_kwargs")):
        raise ValueError("Reference payload must be tools-disabled with native-default reasoning")
    if payload.get("temperature") != 0 or payload.get("max_tokens") != 4096 or payload.get("stream") is not False:
        raise ValueError("Reference inference settings changed")
    payload["model"] = identifier
    return payload


def gguf_metadata(path):
    """Read selected metadata only; skip tensor data and large tokenizer arrays."""
    formats = {0: "B", 1: "b", 2: "H", 3: "h", 4: "I", 5: "i", 6: "f", 7: "?", 10: "Q", 11: "q", 12: "d"}
    wanted = {"general.name", "general.architecture", "general.file_type", "general.quantization_version", "tokenizer.chat_template"}
    result = {}
    with Path(path).open("rb") as f:
        def scalar(fmt):
            return struct.unpack("<" + fmt, f.read(struct.calcsize(fmt)))[0]
        def string(keep=True):
            length = scalar("Q")
            if keep:
                return f.read(length).decode("utf-8")
            f.seek(length, 1)
        def value(kind, keep):
            if kind == 8:
                return string(keep)
            if kind == 9:
                element, count = scalar("I"), scalar("Q")
                if element in formats:
                    f.seek(struct.calcsize(formats[element]) * count, 1)
                else:
                    for _ in range(count): value(element, False)
                return None
            if kind not in formats:
                raise ValueError("Unsupported GGUF metadata type")
            parsed = scalar(formats[kind])
            return parsed if keep else None
        if f.read(4) != b"GGUF":
            raise ValueError("Not a GGUF model")
        version, tensors, entries = scalar("I"), scalar("Q"), scalar("Q")
        if version not in (2, 3):
            raise ValueError("Unsupported GGUF version")
        for _ in range(entries):
            key = string()
            keep = key in wanted or key.endswith(".context_length")
            parsed = value(scalar("I"), keep)
            if keep: result[key] = parsed
    return {"file": str(Path(path).resolve()), "size_bytes": Path(path).stat().st_size,
            "gguf_version": version, "tensor_count": tensors, "metadata": result}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=os.environ.get("LM_STUDIO_BASE_URL"))
    parser.add_argument("--model", required=True)
    parser.add_argument("--model-file", required=True)
    parser.add_argument("--source-run", required=True, help="Reviewed four-request evidence-presentation diagnostic")
    parser.add_argument("--out", required=True)
    args = parser.parse_args(argv)
    if not args.base_url:
        parser.error("Set LM_STUDIO_BASE_URL or --base-url")
    source, out = Path(args.source_run).resolve(), Path(args.out).resolve()
    if out.exists() or out.is_relative_to(source) or source.is_relative_to(out):
        parser.error("Use a fresh separate output directory")
    if out.is_relative_to(ROOT) and subprocess.run(["git", "-C", str(ROOT), "check-ignore", "--quiet", str(out)]).returncode:
        parser.error("Raw artifacts must be ignored or outside the repository")
    reference = json.loads((source / "manifest.json").read_text())
    checkpoint = Path(reference["source_run"])
    source_before, checkpoint_before = tree_digest(source), tree_digest(checkpoint)
    file_metadata = gguf_metadata(args.model_file)
    api = CompatibleProvider(args.base_url.rstrip("/").removesuffix("/v1"), args.model, "", timeout=20)
    def state():
        native, legacy = api.request("/api/v1/models"), api.request("/api/v0/models")
        return loaded_model(native, legacy, args.model)
    initial = state()
    out.mkdir(parents=True, mode=0o700)
    dump(out / "environment_before.json", initial)
    dump(out / "local_gguf_metadata.json", file_metadata)
    proof = TracedProvider(args.base_url.rstrip("/") + ("" if args.base_url.rstrip("/").endswith("/v1") else "/v1"),
                           args.model, "", trace_dir=out / "preflight-wire", temperature=0, max_tokens=4096, timeout=300)
    try:
        preflight = proof.preflight()
        if any(u.get(k) is None for u in proof.usages for k in ("prompt_tokens", "completion_tokens", "total_tokens")):
            raise ValueError("Preflight token accounting unavailable")
        if state() != initial:
            raise ValueError("Loaded model configuration changed during preflight")
        dump(out / "preflight.json", preflight)
        dump(out / "preflight_observations.json", proof.observations)
    except BaseException as exc:
        dump(out / "preflight_failure.json", {"classification": str(exc), "usage": usage_summary(proof.usages)})
        raise
    print("Structured tool round trip and token reporting passed.", flush=True)
    schedule = [("accumulated", 1), ("compact", 1), ("compact", 2), ("accumulated", 2)]
    dump(out / "manifest.json", {"source_run": str(source), "source_tree_sha256_before": source_before,
        "checkpoint": str(checkpoint), "checkpoint_sha256_before": checkpoint_before,
        "model": args.model, "endpoint_sha256": digest(args.base_url.encode()), "schedule": schedule,
        "reasoning_requested": "omitted/native default", "reasoning_capability": initial["model"]["capabilities"].get("reasoning"),
        "verified_effective_reasoning_effort": None,
        "settings": {"temperature": 0, "max_tokens": 4096, "stream": False, "tools": "omitted"},
        "scope": "Cross-model prompted synthesis only; no navigation benchmark; native model tokenizer counts are not equivalent costs"})
    rows = []
    try:
        for presentation, repetition in schedule:
            if state() != initial:
                raise ValueError("Loaded model state changed; refusing automatic reload")
            label = f"{presentation}_{repetition}"
            recorded_path = source / label / "wire/request_01.json"
            recorded = json.loads(recorded_path.read_text())
            payload = replay_for_model(recorded, args.model)
            provider = TracedProvider(proof.base_url, args.model, "", trace_dir=out / label / "wire", timeout=300)
            print("Synthesis " + label, flush=True)
            gpu_before = vram_samples()
            start = time.perf_counter()
            raw = provider.request("/chat/completions", payload)
            row = response_summary(raw, provider, time.perf_counter() - start)
            msg = raw["choices"][0]["message"]
            # Preserve provider differences instead of assuming Strata's field name.
            reasoning_fields = {k: len(msg[k].encode()) for k in ("reasoning", "reasoning_content") if isinstance(msg.get(k), str)}
            row.update({"presentation": presentation, "repetition": repetition, "model": args.model,
                "quantization": initial["model"]["quantization"], "requested_reasoning_effort": "omitted/native default",
                "verified_effective_reasoning_effort": None, "reasoning_fields_bytes": reasoning_fields,
                "message_keys": sorted(msg), "reference_payload_sha256": digest(recorded_path.read_bytes()),
                "unchanged_evidence_messages_sha256": digest(payload["messages"]),
                "quality": asdict(QualityScorer().score(row["final_answer"], str(checkpoint / "repository"), "symbol_grounding"))})
            if "<think>" in (msg.get("content") or ""):
                row["unseparated_reasoning_in_content"] = True
                row["accepted_terminal_response"] = False
                row["final_answer"] = ""
                row["error_classification"] = "unseparated_reasoning_requires_review"
            rows.append(row)
            dump(out / label / "summary.json", row)
            dump(out / label / "observations.json", provider.observations)
            dump(out / label / "environment.json", {"loaded_model": state(), "gpu_before": gpu_before, "gpu_after": vram_samples()})
            dump(out / "results.json", rows)
            print(json.dumps({k: row[k] for k in ("presentation", "repetition", "finish_reason", "usage", "elapsed_seconds", "message_keys")}), flush=True)
    finally:
        after, checkpoint_after = tree_digest(source), tree_digest(checkpoint)
        dump(out / "source_integrity.json", {"reference_before": source_before, "reference_after": after,
                                             "checkpoint_before": checkpoint_before, "checkpoint_after": checkpoint_after,
                                             "unchanged": source_before == after and checkpoint_before == checkpoint_after})
        dump(out / "environment_after.json", state())
        if source_before != after or checkpoint_before != checkpoint_after:
            raise ValueError("Preserved diagnostic inputs changed")


if __name__ == "__main__":
    main()
