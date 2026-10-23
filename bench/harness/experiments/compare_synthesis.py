"""Matched-checkpoint synthesis diagnostics with verified Strata template controls.

No benchmark arms, repository tools, engine starts or server-setting writes.
Docker verification imports the deployed frontend/tokenizer in a separate CPU-only
process. Raw responses and reasoning must remain in ignored local artifacts.
"""
from __future__ import annotations

import argparse
from dataclasses import asdict
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess
import time

from bench.harness.experiments.diagnose_synthesis import checkpoint_payload, replay_payload, response_summary
from bench.harness.experiments.completion_guidance import CAUSAL_GUIDANCE
from bench.harness.experiments.navigation_skill import ROOT, digest, dump, vram_samples
from bench.harness.experiments.provider import CompatibleProvider, TracedProvider, read_key
from bench.harness.experiments.tools import tree_digest
from bench.harness.framework.agentic_loop import FINAL_TURN_PROMPT
from bench.harness.framework.quality_scorer import QualityScorer


def experiment_payload(recorded, label):
    if label not in "ABCD" or len(label) != 1:
        raise ValueError("Unknown experiment")
    if any(k in recorded for k in ("reasoning_effort", "reasoning", "chat_template_kwargs")):
        raise ValueError("Checkpoint already overrides reasoning; cannot label it server-default")
    payload = replay_payload(recorded, CAUSAL_GUIDANCE if label in "BD" else FINAL_TURN_PROMPT)
    if label in "CD":
        payload["reasoning_effort"] = "medium"
    if payload.get("temperature") != 0 or payload.get("max_tokens") != 4096 or payload.get("stream") is not False:
        raise ValueError("Unexpected checkpoint inference settings")
    return payload


# Runs ONLY in a short-lived docker exec Python process: no Service.__init__,
# StrataEngine, server, CUDA/HIP context, config writes or weight loading.
DOCKER_VERIFY = r'''
import hashlib, json, pathlib, sys
from serve.frontend import ChatTemplate, openai_to_messages
from serve.server import Service, literal_tags
import strata_tokenizer as ST
inp=json.load(sys.stdin)
args=pathlib.Path('/proc/1/cmdline').read_bytes().decode().split('\0')
cfgpath=args[args.index('--config')+1]
cfg=json.loads(pathlib.Path(cfgpath).read_text())
if cfg.get('effort_position','start')!='start':
    raise ValueError('This verifier requires start-position effort')
tpath=pathlib.Path(cfg['tokenizer'])
template=ChatTemplate(tpath/'chat_template.jinja')
vocab=json.loads((tpath/'vocab.json').read_text())
tokens=[None]*len(vocab)
for t,i in vocab.items(): tokens[i]=t
tok=ST.Tokenizer(tokens,(tpath/'merges.txt').read_text().split('\n'),json.loads((tpath/'token_type.json').read_text()))
svc=Service.__new__(Service)
svc.shared=inp['shared_defaults']; svc.config_effort=cfg.get('reasoning_effort')
svc.template=template; svc.tok=tok; svc.literals=literal_tags(getattr(tok,'control_tokens',()))
svc.effort_end=False
rows={}
for label,payload in inp['payloads'].items():
    req=svc.with_shared(payload,'openai')
    messages,tools,kwargs=openai_to_messages(req)
    rendered=svc.render_prompt(messages,tools,kwargs)
    ids=svc.encode_prompt(messages,tools,kwargs)
    rows[label]={'template_kwargs':kwargs,'expected_prompt_tokens':len(ids),
                 'rendered_prompt_sha256':hashlib.sha256(rendered.encode()).hexdigest(),
                 'encoded_ids_sha256':hashlib.sha256(json.dumps(ids).encode()).hexdigest(),
                 'xhigh_instruction_present':'Reasoning effort is set to xhigh.' in rendered,
                 'low_instruction_present':'Reasoning effort is set to low.' in rendered}
files=['serve/frontend.py','serve/server.py']
print(json.dumps({'config_reasoning_effort':svc.config_effort,'effort_position':'start',
                 'source_hashes':{n:hashlib.sha256(pathlib.Path(n).read_bytes()).hexdigest() for n in files},
                 'template_sha256':hashlib.sha256(template.source.encode()).hexdigest(),
                 'payloads':rows,'engine_started':False,'model_weights_loaded':False}))
'''


def verification_passes(proof, props, local_hashes):
    """Require deployed code/template agreement and distinct effective prompt controls."""
    if proof.get("source_hashes") != local_hashes or proof.get("template_sha256") != hashlib.sha256(props["chat_template"].encode()).hexdigest():
        return False
    if proof.get("config_reasoning_effort") is not None or proof.get("effort_position") != "start":
        return False
    rows = proof.get("payloads", {})
    for label in "ABCD":
        row = rows.get(label, {})
        if not isinstance(row.get("expected_prompt_tokens"), int):
            return False
        if label in "AB":
            if row.get("template_kwargs") != {} or row.get("xhigh_instruction_present") is not True:
                return False
        elif row.get("template_kwargs") != {"reasoning_effort": "medium"} or row.get("xhigh_instruction_present") is not False or row.get("low_instruction_present") is not False:
            return False
    return all(rows[a]["encoded_ids_sha256"] != rows[b]["encoded_ids_sha256"] for a, b in (("A", "C"), ("B", "D")))


def docker_proof(result):
    """Parse the deployed-renderer verification; degrade to a marker instead of raising."""
    if result.returncode != 0:
        return {"verification_error": "docker_cpu_verification_failed"}
    try:
        return json.loads(result.stdout)
    except ValueError:
        # A non-JSON banner on stdout must degrade to "not verified", not crash.
        return {"verification_error": "docker_cpu_verification_unparseable"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=os.environ.get("STRATA_BASE_URL"))
    parser.add_argument("--env-file")
    parser.add_argument("--api-key-env", default="STRATA_API_KEY")
    parser.add_argument("--source-run", required=True)
    parser.add_argument("--strata-source", required=True, help="Read-only deployed source counterpart for hash checks")
    parser.add_argument("--container", default="strata")
    parser.add_argument("--out", required=True)
    args = parser.parse_args(argv)
    if not args.base_url:
        parser.error("Set STRATA_BASE_URL or --base-url")
    source, out = Path(args.source_run).resolve(), Path(args.out).resolve()
    if out.exists() or out.is_relative_to(source) or source.is_relative_to(out):
        parser.error("Use a fresh output directory separate from the source run")
    if out.is_relative_to(ROOT) and subprocess.run(["git", "-C", str(ROOT), "check-ignore", "--quiet", str(out)]).returncode:
        parser.error("Artifacts must be ignored or outside the repository")
    before = tree_digest(source)
    source_path, recorded = checkpoint_payload(source, "C", 16)
    payloads = {label: experiment_payload(recorded, label) for label in "ABCD"}
    key = read_key(args.api_key_env, args.env_file)
    api = CompatibleProvider(args.base_url.removesuffix("/v1"), recorded["model"], key, timeout=300)
    out.mkdir(parents=True, mode=0o700)
    models, props, settings, status = [api.request(route) for route in ("/v1/models", "/props", "/settings", "/v1/status")]
    if status.get("activity", {}).get("in_flight") or not status.get("loaded"):
        raise ValueError("Server must already be loaded and idle")
    if status.get("model") != recorded["model"] or status.get("context", {}).get("native") != 131072:
        raise ValueError("Model/context differ from the approved profile")
    local_hashes = {n: hashlib.sha256((Path(args.strata_source) / n).read_bytes()).hexdigest()
                    for n in ("serve/frontend.py", "serve/server.py")}
    check = subprocess.run(["docker", "exec", "-i", args.container, "/opt/strata/.venv/bin/python", "-B", "-c", DOCKER_VERIFY],
                           input=json.dumps({"shared_defaults": settings.get("defaults", {}), "payloads": payloads}).encode(),
                           capture_output=True, timeout=60)
    proof = docker_proof(check)
    verified = (not settings.get("defaults") and verification_passes(proof, props, local_hashes)
                if check.returncode == 0 else False)
    dump(out / "reasoning_verification.json", {"medium_template_control_verified": verified, "proof": proof,
        "local_source_hashes": local_hashes, "settings": settings, "props": props, "models": models, "status": status,
        "per_request_effective_effort_echo": None,
        "method": "Deployed code and live template; independent CPU rendering/tokenization; cross-check each live request's prompt_tokens"})
    order = list("ABCD" if verified else "AB")
    random.Random(20261018).shuffle(order)
    dump(out / "manifest.json", {"source_run": str(source), "source_request": str(source_path.relative_to(source)),
        "source_request_sha256": digest(source_path.read_bytes()), "source_tree_sha256_before": before,
        "evidence_messages_sha256": digest(recorded["messages"]), "model": recorded["model"],
        "endpoint_sha256": digest(args.base_url.encode()), "generic_guidance": FINAL_TURN_PROMPT,
        "causal_guidance": CAUSAL_GUIDANCE, "schedule": order, "seed": 20261018, "repetitions": 1,
        "medium_verified_before_generation": verified, "scope": "Prompted tools-disabled synthesis, not navigation arms"})
    rows = []
    medium_still_verified = verified
    try:
        for label in order:
            if label in "CD" and not medium_still_verified:
                continue
            print("Synthesis experiment " + label, flush=True)
            case_out = out / label
            provider = TracedProvider(args.base_url, recorded["model"], key, trace_dir=case_out / "wire", timeout=300)
            state_before = api.request("/v1/status")
            if api.request("/settings") != settings or state_before["started"] != status["started"] or not state_before["loaded"] or state_before["activity"]["in_flight"]:
                raise ValueError("Server state/defaults changed or another inference is active")
            vram_before = vram_samples()
            started = time.perf_counter()
            raw = provider.request("/chat/completions", payloads[label])
            row = response_summary(raw, provider, time.perf_counter() - started)
            expected = proof.get("payloads", {}).get(label, {}).get("expected_prompt_tokens")
            count_matches = expected is not None and row["usage"]["prompt_tokens"] == expected
            effective = ("medium" if label in "CD" else "xhigh") if verified and count_matches else None
            if verified and not count_matches:
                medium_still_verified = False
            row.update({"experiment": label, "requested_reasoning_effort": "medium" if label in "CD" else "omitted/server-default",
                "verified_effective_reasoning_effort": effective, "expected_prompt_tokens": expected,
                "live_prompt_count_matches_deployed_render": count_matches,
                "guidance": "concise_causal" if label in "BD" else "generic",
                "evidence_messages_sha256": digest(payloads[label]["messages"][:-1]),
                "quality": asdict(QualityScorer().score(row["final_answer"], str(source / "repository"), "symbol_grounding"))})
            rows.append(row)
            dump(case_out / "summary.json", row)
            dump(case_out / "observations.json", provider.observations)
            dump(case_out / "environment.json", {"status_before": state_before, "status_after": api.request("/v1/status"),
                                                 "vram_before": vram_before, "vram_after": vram_samples()})
            print(json.dumps({k: row[k] for k in ("experiment", "finish_reason", "usage", "elapsed_seconds", "verified_effective_reasoning_effort", "quality")}), flush=True)
        dump(out / "results.json", rows)
    finally:
        after = tree_digest(source)
        dump(out / "source_integrity.json", {"before": before, "after": after, "unchanged": before == after})
        dump(out / "environment_after.json", {"models": api.request("/v1/models"), "settings": api.request("/settings"), "status": api.request("/v1/status")})
        if after != before:
            raise ValueError("Original checkpoint artifacts changed")


if __name__ == "__main__":
    main()
