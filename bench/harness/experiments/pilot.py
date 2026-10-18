"""Technical-only pilot admission and fail-fast execution checks."""
import json
from pathlib import Path

CONTROL_FIELDS = ("configurations", "model", "quantization", "context_window", "temperature",
                  "max_output_tokens", "request_options", "reasoning_configuration", "completion_guidance",
                  "max_iterations", "reserve_final_turn", "timeout_seconds", "setup_timeout_seconds")
EXECUTION_SOURCES = ("framework/agentic_loop.py", "framework/executors.py", "framework/backends.py",
                     "framework/mcp_executor.py", "experiments/tools.py", "experiments/provider.py",
                     "experiments/completion_guidance.py")


def require_technical_smoke(path, current):
    # Import lazily: the controller uses this module, while its legacy smoke
    # validator remains available for old manifests and reports.
    from bench.harness.experiments.navigation_skill import validate_smoke, digest
    path = Path(path)
    old = json.loads((path / "run_manifest.json").read_text())
    rows = [json.loads(s) for s in (path / "results.jsonl").read_text().splitlines()]
    if old["phase"] != "smoke" or not validate_smoke(rows, old)["technical_passed"]:
        raise ValueError("Pilot requires a technically passing smoke; quality is evaluated separately")
    if not old.get("mcp_binary_sha256") or old["mcp_binary_sha256"] != current.get("mcp_binary_sha256"):
        raise ValueError("Smoke control mismatch: mcp_binary_sha256; obtain a fresh smoke with the binary pinned")
    for key in ("repo", "prompt_set_sha256", "skill_sha256", "provider_endpoint_sha256", "embedding_configuration", "capture_traces"):
        if old.get(key) != current.get(key):
            raise ValueError("Smoke control mismatch: " + key)
    for key in CONTROL_FIELDS:
        if old["config"].get(key) != current["config"].get(key):
            raise ValueError("Smoke inference/control mismatch: " + key)
    for source in EXECUTION_SOURCES:
        key = "bench/harness/" + source
        if not old["harness_sources"].get(key) or old["harness_sources"][key] != current["harness_sources"].get(key):
            raise ValueError("Smoke execution source mismatch: " + key)
    # Controller/report/rubric changes are explicit in the new manifest, rather
    # than requiring an identical all-files hash that would force another smoke.
    schemas = {}
    for row in rows:
        first = path / "instances" / row["task_id"] / row["configuration"] / f"run_{row['run_number']}" / "wire/request_01.json"
        schemas[row["configuration"]] = digest(json.loads(first.read_text())["tools"])
    return {"source_smoke": str(path.resolve()), "source_identity": old["identity"],
            "admission": "technical_only", "wire_schema_sha256": schemas,
            "baseline_rows": [{"configuration": r["configuration"], "tool_schema_sha256": r["tool_schema_sha256"]} for r in rows]}


def validate_pilot(rows, manifest):
    baseline = {r["configuration"]: r for r in manifest["technical_readiness"]["baseline_rows"]}
    expected = manifest["schedule"]
    checks = {
        "environment_stable": not manifest.get("execution_stop") and all(r.get("environment_unchanged") is True for r in rows),
        "schedule_prefix": [[r["task_id"], r["configuration"], r["run_number"]] for r in rows] == [list(x) for x in expected[:len(rows)]] and len(rows) <= len(expected),
        "valid_termination": all(r["status"] == "completed" and r.get("response_metadata") and r["response_metadata"][-1]["finish_reason"] == "stop" for r in rows),
        "provider_and_transport": all(r["provider_protocol_errors"] == 0 and not any(e["error"] == "tool_transport_error" for e in r.get("tool_events", [])) for r in rows),
        "repository_unchanged": all(r["repository_unchanged"] for r in rows),
        "execution_inputs_unchanged": all(r.get("inputs_unchanged") is True for r in rows),
        "skill_isolation": all(r["skill_loaded"] == (r["configuration"] == "C") and r["loaded_skill_sha256"] == (manifest["skill_sha256"] if r["configuration"] == "C" else None) for r in rows),
        "mcp_isolation": all(r["mcp_available"] == (r["configuration"] != "A") and (r["mcp_tool_calls"] == 0 if r["configuration"] == "A" else True) for r in rows),
        "schemas_unchanged": all(r.get("tool_schema_sha256") == baseline[r["configuration"]]["tool_schema_sha256"] for r in rows),
        "exact_task_prompt": all(r["task_prompt_sha256"] == manifest["task_prompt_sha256"][r["task_id"]] for r in rows),
        "exact_system_prompt": all(r["system_prompt_sha256"] == manifest["system_prompt_sha256"][r["configuration"]] for r in rows),
        "model_unchanged": all(r.get("model_metadata_before") == r.get("model_metadata_after") == {"object":"list", "data":[manifest["preflight"]["model"]]} for r in rows),
        "context_and_budget": all(0 < len(r.get("usage_per_request", [])) <= manifest["config"]["max_iterations"] and all(isinstance(u.get("prompt_tokens"), int) and u["prompt_tokens"] + manifest["config"]["max_output_tokens"] <= manifest["config"]["context_window"] for u in r.get("usage_per_request", [])) for r in rows),
        "provider_accounting": all(accounting_valid(r) for r in rows),
        "wire_isolation": all(r.get("wire_controls_verified") is True for r in rows),
    }
    prefix_passed = all(checks.values())
    complete = len(rows) == len(expected)
    return {"phase": "pilot", "checks": checks, "continue_execution": prefix_passed,
            "pilot_complete": complete, "planned_runs": len(expected), "executed_runs": len(rows),
            "technical_passed": prefix_passed and complete,
            "correctness_evaluation": {"strict_reviewed": sum(r.get("correctness_score") is not None for r in rows),
                                       "task_reviewed": sum(r.get("task_quality_score") is not None for r in rows)},
            "full_benchmark_executed": False}


def accounting_valid(row):
    usage = row.get("usage_per_request", [])
    fields = ("prompt_tokens", "completion_tokens", "total_tokens")
    return bool(usage) and all(all(type(u.get(k)) is int and u[k] >= 0 for k in fields) and u["total_tokens"] == u["prompt_tokens"] + u["completion_tokens"] for u in usage) and all(row.get(k) == sum(u[k] for u in usage) for k in fields)


def session_valid(row, config):
    """Execution-only checks shared by smoke, pilot and full runs."""
    metadata = row.get("response_metadata") or []
    usage = row.get("usage_per_request") or []
    return bool(row.get("status") == "completed" and metadata and
                metadata[-1].get("finish_reason") == "stop" and
                row.get("provider_protocol_errors") == 0 and accounting_valid(row) and
                row.get("repository_unchanged") is True and row.get("inputs_unchanged") is True and
                not any(e.get("error") == "tool_transport_error" for e in row.get("tool_events", [])) and
                0 < len(usage) <= config.get("max_iterations", 25) and
                all(u["prompt_tokens"] + config.get("max_output_tokens", 4096) <= config["context_window"] for u in usage))


def audit_wire(wire, task_text, system, config, baseline_schema):
    """Audit actual transmitted requests, not only intended prompt hashes."""
    from bench.harness.experiments.navigation_skill import digest
    from bench.harness.experiments.completion_guidance import CAUSAL_GUIDANCE
    files = sorted(Path(wire).glob("request_*.json"))
    if not files:
        return False
    for i, file in enumerate(files):
        payload = json.loads(file.read_text())
        messages = payload["messages"]
        if messages[:2] != [{"role":"system", "content":system}, {"role":"user", "content":task_text}]:
            return False
        if [m for m in messages if m["role"] == "system"] != messages[:1]:
            return False
        if [m["content"] for m in messages if m["role"] == "user"] not in ([task_text], [task_text, CAUSAL_GUIDANCE]):
            return False
        if any(payload.get(k) != v for k, v in {"model":config["model"], "temperature":config["temperature"], "max_tokens":config["max_output_tokens"], "stream":False, **config["request_options"]}.items()):
            return False
        if set(payload) != {"model", "messages", "temperature", "max_tokens", "stream", *config["request_options"], *({"tools"} if "tools" in payload else set())}:
            return False
        if "tools" not in payload:
            if i + 1 != config["max_iterations"] or messages[-1] != {"role":"user", "content":CAUSAL_GUIDANCE}:
                return False
        else:
            # Schema was already compared to the historical smoke at the row
            # level; verify every wire request retains the same definitions.
            if digest(payload["tools"]) != baseline_schema:
                return False
    return True


def observe_strata(base_url, model, key):
    """Read-only, outside navigation timing; no model/config mutation endpoints."""
    from bench.harness.experiments.provider import CompatibleProvider
    from bench.harness.experiments.navigation_skill import digest
    api = CompatibleProvider(base_url.rstrip('/').removesuffix('/v1'), model, key, timeout=15)
    values = {route: api.request(route) for route in ('/settings', '/props', '/v1/status')}
    status = values['/v1/status']
    if not status.get('loaded') or status.get('model') != model or status.get('activity', {}).get('in_flight'):
        raise ValueError('Strata must remain loaded with the requested model and idle between runs')
    control = {'settings': values['/settings'], 'template_sha256': digest(values['/props']['chat_template'].encode()),
               **{k: status.get(k) for k in ('model', 'loaded', 'engine', 'started', 'context', 'cache_max_tokens', 'concurrency')}}
    return {'control_sha256': digest(control), 'control': control, 'observations': values}
