"""Opt-in A/B/C experiment, built on the existing local AgenticLoop.

python -m bench.harness.experiments.navigation_skill --help
"""
from __future__ import annotations

import argparse
from dataclasses import asdict
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
import time

from bench.harness.framework.agentic_loop import AgenticLoop, FINAL_TURN_PROMPT
from bench.harness.framework.benchmark_runner import _detect_cpu, _detect_ram_gb
from bench.harness.framework.prompt_loader import load_prompts
from bench.harness.framework.quality_scorer import QualityScorer
from bench.harness.experiments.provider import CompatibleProvider, TracedProvider, read_key, usage_summary, ProviderError
from bench.harness.experiments.tools import (
    DeadlineExceeded, ExperimentTools, IsolatedMCP, deadline, git,
    make_snapshot, repo_identity, tree_digest,
)
from bench.harness.experiments.report import write_report
from bench.harness.experiments.completion_guidance import ANSWER_GUIDANCE, CAUSAL_GUIDANCE
from bench.harness.experiments.task_quality import assess, review_template, VERSION, TASKS
from bench.harness.experiments.pilot import require_technical_smoke, validate_pilot, audit_wire, observe_strata, accounting_valid, session_valid

ROOT = Path(__file__).resolve().parents[3]
DEFAULT_CONFIG = ROOT / "bench/harness/configs/navigation-skill.json"
NEUTRAL = (
    "You are a repository analysis assistant. Answer the user's question using evidence "
    "from the repository. Name exact files and symbols, explain relevant relationships, "
    "and identify uncertainty. Available tools are read-only. "
    "Task completion: collect enough evidence to address the requested parts, then "
    "give a final answer without requesting tools. Avoid repeating searches that "
    "do not resolve a specific remaining uncertainty. If evidence is insufficient, "
    "state the missing parts explicitly instead of continuing open-ended exploration. "
    + ANSWER_GUIDANCE + " "
    "Repository root and project path: {repo}."
)


def digest(value):
    return hashlib.sha256(value if isinstance(value, bytes) else json.dumps(value, sort_keys=True).encode()).hexdigest()


def dump(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def system_prompt(arm, root, skill):
    base = NEUTRAL.format(repo=str(root))
    return base + ("\n\n" + skill if arm == "C" else "")


def schedule(tasks, repetitions, seed):
    """Seeded task shuffle and balanced cyclic arm order across repetitions."""
    rng = random.Random(seed)
    tasks = list(tasks)
    rng.shuffle(tasks)
    arms = list("ABC")
    rng.shuffle(arms)
    return [(task, arms[(position + repetition + i) % 3], repetition + 1)
            for repetition in range(repetitions)
            for i, task in enumerate(tasks) for position in range(3)]


def vram_samples():
    samples = {}
    for device in Path("/sys/class/drm").glob("card[0-9]*/device"):
        fields = {}
        for name in ("mem_info_vram_used", "mem_info_vram_total"):
            try:
                fields[name] = int((device / name).read_text())
            except (OSError, ValueError):
                fields[name] = None
        if any(v is not None for v in fields.values()):
            samples[str(device.parent.name)] = fields
    return samples or None


def load_reviews(out, rows):
    path = Path(out) / "reviews.json"
    if not path.exists():
        return
    reviews = json.loads(path.read_text())
    for row in rows:
        key = f"{row['task_id']}/{row['configuration']}/{row['run_number']}"
        review = reviews.get(key, {})
        score = review.get("correctness_score")
        if score is not None and (type(score) not in (int, float) or score not in (0, 0.5, 1)):
            raise ValueError("Manual correctness must be 0, 0.5 or 1")
        if score is not None and not review.get("evidence"):
            raise ValueError("Correctness review requires repository-grounded evidence")
        row["correctness_score"] = score
        row["task_success"] = score == 1 and row["status"] == "completed" if score is not None else None
        row.update(assess(row["task_id"], review.get("task_quality")))
        row["task_quality_success"] = (row["task_quality_score"] == 1 and row["status"] == "completed"
                                       if row["task_quality_score"] is not None else None)
        for metric in ("appropriate_mcp_selection", "unnecessary_mcp_calls", "redundant_repository_searches"):
            row[metric] = review.get(metric)


def validate_smoke(rows, manifest):
    tests = {
        "three_arms_one_task": len(rows) == 3 and {r["configuration"] for r in rows} == set("ABC") and len({r["task_id"] for r in rows}) == 1,
        "all_completed": bool(rows) and all(r["status"] == "completed" for r in rows),
        "terminal_stop": bool(rows) and all(r.get("response_metadata") and
                                             r["response_metadata"][-1]["finish_reason"] == "stop" for r in rows),
        "tools_exercised_and_valid": bool(rows) and all(r["tool_calls"] is not None and r["tool_calls"] > 0 and r["failed_tool_calls"] == 0 and r["provider_protocol_errors"] == 0 for r in rows),
        "mcp_exercised_in_B_C": all(r["mcp_tool_calls"] is not None and r["mcp_tool_calls"] > 0 for r in rows if r["configuration"] in "BC"),
        "A_has_no_mcp": all(r["mcp_tool_calls"] == 0 and not r["mcp_available"] for r in rows if r["configuration"] == "A"),
        "skill_only_in_C": all(r["skill_loaded"] == (r["configuration"] == "C") and
                              r.get("loaded_skill_sha256") == (manifest["skill_sha256"] if r["configuration"] == "C" else None) for r in rows),
        "identical_task_prompts": len({r["task_prompt_sha256"] for r in rows}) == 1,
        "A_B_identical_system_prompt": len({r["system_prompt_sha256"] for r in rows if r["configuration"] in "AB"}) == 1,
        "B_C_identical_tools": len({r["tool_schema_sha256"] for r in rows if r["configuration"] in "BC"}) == 1,
        "provider_usage_available": bool(rows) and all(all(r[k] is not None for k in ("prompt_tokens", "completion_tokens", "total_tokens")) for r in rows),
        "provider_accounting_valid": bool(rows) and all(accounting_valid(r) for r in rows),
        "sessions_valid": bool(rows) and all(session_valid(r, manifest["config"]) for r in rows),
        "repository_unchanged": bool(rows) and all(r["repository_unchanged"] for r in rows),
        "execution_inputs_unchanged": bool(rows) and all(r.get("inputs_unchanged") is True for r in rows),
        "context_verified": manifest.get("preflight", {}).get("model", {}).get("meta", {}).get("n_ctx") == manifest["config"]["context_window"],
    }
    return {"checks": tests, "technical_passed": all(tests.values()),
            "correctness_review_passed": len(rows) == 3 and all(r["task_success"] is True for r in rows),
            "full_benchmark_executed": manifest["phase"] == "full"}


def require_smoke(path, identity):
    path = Path(path)
    manifest = json.loads((path / "run_manifest.json").read_text())
    rows = [json.loads(line) for line in (path / "results.jsonl").read_text().splitlines()]
    result = validate_smoke(rows, manifest)
    if manifest["phase"] != "smoke" or manifest["identity"] != identity or not result["technical_passed"]:
        raise ValueError("Experiment requires matching smoke inputs and passing technical checks; correctness is measured separately")


def prepare_index(snapshot, home, timeout):
    mcp = IsolatedMCP(home, tier="all")
    start = time.perf_counter()
    try:
        with deadline(timeout):
            mcp.startup(str(snapshot))
            first = mcp.ready
            if first.get("index", {}).get("embeddings") == "running" or first.get("embeddings", {}).get("status") == "running":
                definition = next(t for t in mcp.get_tool_definitions() if t.name == "wait_for_embeddings")
                props = definition.parameters.get("properties", {})
                arguments = {k: str(snapshot) for k in ("project", "path") if k in props}
                if "timeout_secs" in props:
                    arguments["timeout_secs"] = max(1, int(timeout - (time.perf_counter() - start)))
                response = mcp._call_tool("wait_for_embeddings", arguments)
                if response.get("result", {}).get("isError"):
                    raise ValueError("embedding_preparation_failed")
                mcp._ensure_project_ready(str(snapshot))
            if mcp.ready.get("index", {}).get("embeddings") == "running":
                raise ValueError("Embeddings must finish before navigation timing")
            return {"cold_setup_seconds": time.perf_counter() - start,
                    "initial_readiness": first, "final_readiness": mcp.ready,
                    "server": json.loads(mcp._extract_content(mcp._call_tool("server_info", {})))}
    finally:
        mcp.shutdown()


def run_one(task, arm, repetition, config, snapshot, home, key, base_url, skill, before, repo, trace_dir=None):
    started = time.perf_counter()
    try:
        return _run_one(task, arm, repetition, config, snapshot, home, key, base_url, skill, before, repo, trace_dir)
    except (Exception, DeadlineExceeded) as exc:
        # Preserve failed session setup as a run rather than silently losing it.
        # Do not serialize exception bodies from subprocesses or remote providers.
        return {
            "task_id": task.id, "category": task.category, "configuration": arm,
            "model": config["model"], "quantization": config["quantization"],
            "agent_harness": "pitlane-local-AgenticLoop/skill-experiment-v1", "run_number": repetition,
            "status": "error", "task_success": None, "correctness_score": None,
            "termination_kind": "unavailable", "response_metadata": [],
            "quality_score": None, "quality_details": None,
            **usage_summary([]), "tool_output_bytes": None, "tool_response_tokens": None,
            "tool_calls": None, "native_tool_calls": None, "mcp_tool_calls": None,
            "failed_tool_calls": None, "invalid_tool_arguments": None, "tool_retries": None,
            "provider_retries": 0, "provider_protocol_errors": 0,
            "execution_seconds": None, "session_setup_seconds": time.perf_counter() - started,
            "error_classification": "setup_timeout" if isinstance(exc, DeadlineExceeded) else "infrastructure_" + type(exc).__name__,
            "mcp_available": False, "skill_loaded": False, "loaded_skill_sha256": None,
            "appropriate_mcp_selection": None, "unnecessary_mcp_calls": None,
            "redundant_repository_searches": None, "exact_redundant_successful_calls": None,
            "native_fallback_calls": None, "task_prompt_sha256": None, "system_prompt_sha256": None,
            "tool_schema_sha256": None, "tool_names": [], "usage_per_request": [],
            "tool_events": [], "final_answer": "", "gpu_before": None, "gpu_after": None,
            "model_metadata_before": None, "model_metadata_after": None,
            "repository_unchanged": repo_identity(repo) == before,
        }


def _run_one(task, arm, repetition, config, snapshot, home, key, base_url, skill, before, repo, trace_dir=None):
    tools = None
    mcp = IsolatedMCP(home) if arm != "A" else None
    backend_type = TracedProvider if trace_dir else CompatibleProvider
    backend = backend_type(base_url, config["model"], key, temperature=config["temperature"],
                                 max_tokens=config["max_output_tokens"], request_options=config["request_options"],
                                 timeout=config["timeout_seconds"], **({"trace_dir": trace_dir} if trace_dir else {}))
    setup_start = time.perf_counter()
    try:
        with deadline(config["setup_timeout_seconds"]):
            if mcp:
                mcp.startup(str(snapshot))
            tools = ExperimentTools(snapshot, mcp)
        setup_seconds = time.perf_counter() - setup_start
        # Metadata observations are outside measured navigation time.
        def model_sample():
            try:
                return backend.request("/models")
            except ProviderError as exc:
                return {"unavailable": str(exc)}
        model_before = model_sample()
        gpu_before = vram_samples()
        text = task.prompt + ("\n\n" + task.prompt_suffix if task.prompt_suffix else "")
        system = system_prompt(arm, snapshot, skill)
        start = time.perf_counter()
        result = None
        timeout = False
        try:
            with deadline(config["timeout_seconds"]):
                result = AgenticLoop().run(text, backend, tools, max_iterations=config["max_iterations"],
                                          timeout_seconds=config["timeout_seconds"], prompt_id=task.id,
                                          mode=arm, run_index=repetition, repo_path=str(snapshot), system_prompt=system,
                                          reserve_final_turn=config.get("reserve_final_turn", False),
                                          final_turn_prompt=CAUSAL_GUIDANCE)
        except DeadlineExceeded:
            timeout = True
        elapsed = time.perf_counter() - start
        if trace_dir:
            dump(Path(trace_dir).parent / "observations.json", backend.observations)
            if result:
                dump(Path(trace_dir).parent / "parsed_result.json", asdict(result))
        gpu_after = vram_samples()
        model_after = model_sample()
        status = "timeout" if timeout else result.status
        answer = "" if result is None else result.final_answer
        events = tools.events
        quality = asdict(QualityScorer().score(answer, str(snapshot), task.category))
        first_mcp = next((i for i, e in enumerate(events) if e["kind"] == "mcp"), len(events))
        row = {
            "task_id": task.id, "category": task.category, "configuration": arm,
            "model": config["model"], "quantization": config["quantization"],
            "agent_harness": "pitlane-local-AgenticLoop/skill-experiment-v1", "run_number": repetition,
            "status": status, "task_success": None, "correctness_score": None,
            "termination_kind": "budget_synthesis" if result and any(
                msg.role == "user" and msg.content == CAUSAL_GUIDANCE for msg in result.conversation
            ) else "natural" if status == "completed" else "unfinished",
            "quality_score": quality["quality_score"], "quality_details": quality,
            **usage_summary(backend.usages), "tool_output_bytes": tools.total_response_bytes(),
            "tool_response_tokens": None, "tool_calls": len(events),
            "native_tool_calls": sum(e["kind"] == "native" for e in events),
            "mcp_tool_calls": sum(e["kind"] == "mcp" for e in events),
            "failed_tool_calls": sum(e["error"] is not None for e in events),
            "invalid_tool_arguments": sum(e["error"] == "invalid_arguments" for e in events),
            "tool_retries": sum(e["retry_after_failure"] for e in events), "provider_retries": 0,
            "provider_protocol_errors": backend.protocol_errors,
            "execution_seconds": elapsed, "session_setup_seconds": setup_seconds,
            "error_classification": "timeout" if timeout else result.error or (None if status == "completed" else status),
            "mcp_available": mcp is not None, "skill_loaded": skill in system,
            "loaded_skill_sha256": digest(skill.encode()) if skill in system else None,
            "appropriate_mcp_selection": None, "unnecessary_mcp_calls": None,
            "redundant_repository_searches": None,
            "exact_redundant_successful_calls": sum(e["redundant_successful_call"] for e in events),
            "native_fallback_calls": sum(e["kind"] == "native" for e in events[first_mcp + 1:]),
            "task_prompt_sha256": digest(text.encode()), "system_prompt_sha256": digest(system.encode()),
            "tool_schema_sha256": digest([asdict(t) for t in tools.get_tool_definitions()]),
            "tool_names": [t.name for t in tools.get_tool_definitions()],
            "usage_per_request": backend.usages, "tool_events": events, "final_answer": answer,
            "response_metadata": backend.response_metadata,
            "trace_capture": bool(trace_dir),
            "gpu_before": gpu_before, "gpu_after": gpu_after,
            "model_metadata_before": model_before, "model_metadata_after": model_after,
            "repository_unchanged": repo_identity(repo) == before,
        }
        return row
    finally:
        if mcp:
            mcp.shutdown()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--phase", choices=["smoke", "pilot", "full", "preflight", "report"], default="smoke")
    parser.add_argument("--config", default=str(DEFAULT_CONFIG))
    parser.add_argument("--repo", default=str(ROOT / "bench/repos/ripgrep"))
    parser.add_argument("--prompts", default=str(ROOT / "bench/harness/prompts/ripgrep.jsonl"))
    parser.add_argument("--skill", default=str(ROOT / "skills/pitlane-navigation/SKILL.md"))
    parser.add_argument("--base-url", default=os.environ.get("STRATA_BASE_URL"))
    parser.add_argument("--api-key-env", default="STRATA_API_KEY")
    parser.add_argument("--env-file")
    parser.add_argument("--out", required=True)
    parser.add_argument("--capture-traces", action="store_true", help="Save private wire bodies/responses and parsed conversations under the ignored artifact directory")
    parser.add_argument("--validated-smoke", help="Technically validated smoke artifact directory; required for pilot/full")
    args = parser.parse_args(argv)
    out = Path(args.out).resolve()
    if args.phase == "report":
        manifest = json.loads((out / "run_manifest.json").read_text())
        rows = [json.loads(line) for line in (out / "results.jsonl").read_text().splitlines()]
        load_reviews(out, rows)
        validation = (validate_pilot(rows, manifest) if manifest["phase"] == "pilot" else
                      validate_smoke(rows, manifest) if manifest["phase"] == "smoke" else {"phase": "full"})
        write_report(out, manifest, rows, validation)
        return
    if not args.base_url:
        parser.error("Set STRATA_BASE_URL or --base-url; no endpoint is hardcoded")
    if out.exists() and any(out.iterdir()):
        parser.error("Use a new artifact directory; runs never overwrite or mix inputs")
    repo = Path(args.repo).resolve()
    if out == repo or out.is_relative_to(repo):
        parser.error("Artifacts must be outside the target repository")
    if out.is_relative_to(ROOT) and subprocess.run(
        ["git", "-C", str(ROOT), "check-ignore", "--quiet", str(out)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode != 0:
        parser.error("Artifacts inside the harness repository must use an ignored directory such as results/")
    config = json.loads(Path(args.config).read_text())
    if args.phase == "pilot" and (not args.capture_traces or not args.validated_smoke or
                                  len(config.get("pilot_tasks", [])) != 5 or set(config["pilot_tasks"]) != set(TASKS)):
        parser.error("Pilot requires --capture-traces, --validated-smoke and the five preregistered pilot_tasks")
    if config["configurations"] != {"A": {"name": "native", "mcp": False, "skill": False},
                                     "B": {"name": "mcp-only", "mcp": True, "skill": False},
                                     "C": {"name": "mcp-skill", "mcp": True, "skill": True}}:
        parser.error("Invalid A/B/C treatment definitions")
    key = read_key(args.api_key_env, args.env_file)
    skill = Path(args.skill).read_text()
    if not skill.strip():
        parser.error("Skill treatment must contain nonempty instructions")
    before = repo_identity(repo)
    tracked_changes = git(repo, "diff", "HEAD", "--name-only").decode().strip()
    if tracked_changes:
        parser.error("Target tracked files differ from HEAD; commit or use a clean target checkout")
    sources = {str(p.relative_to(ROOT)): digest(p.read_bytes()) for p in
               sorted((ROOT / "bench/harness").rglob("*.py")) if "__pycache__" not in p.parts}
    embedding = {k: digest(v.encode()) if any(s in k for s in ("KEY", "TOKEN", "AUTH")) else v
                 for k, v in os.environ.items() if k.startswith("PITLANE_EMBED_")}
    mcp_binary = shutil.which("pitlane-mcp")
    if not mcp_binary:
        parser.error("pitlane-mcp must already be installed")
    binary_sha = digest(Path(mcp_binary).read_bytes())
    config_sha = digest(Path(args.config).read_bytes())
    def inputs_unchanged():
        try:
            return (digest(Path(args.config).read_bytes()) == config_sha and
                    digest(Path(args.skill).read_text().encode()) == digest(skill.encode()) and
                    digest(Path(args.prompts).read_bytes()) == prompts_sha and
                    all(digest((ROOT / p).read_bytes()) == sha for p, sha in sources.items()) and
                    shutil.which("pitlane-mcp") == mcp_binary and digest(Path(mcp_binary).read_bytes()) == binary_sha)
        except OSError:
            return False
    prompts_sha = digest(Path(args.prompts).read_bytes())
    identity = digest({"config": config, "repo": before, "prompts": digest(Path(args.prompts).read_bytes()),
                       "skill": digest(skill.encode()), "harness_sources": sources,
                       "provider_endpoint_sha256": digest(args.base_url.encode()), "embedding": embedding,
                       "capture_traces": args.capture_traces,
                       "mcp_binary_sha256": binary_sha})
    admission = None
    if args.phase == "pilot":
        admission = require_technical_smoke(args.validated_smoke, {
            "repo": before, "config": config, "prompt_set_sha256": digest(Path(args.prompts).read_bytes()),
            "skill_sha256": digest(skill.encode()), "provider_endpoint_sha256": digest(args.base_url.encode()),
            "embedding_configuration": embedding, "capture_traces": args.capture_traces, "harness_sources": sources,
            "mcp_binary_sha256": binary_sha})
    if args.phase == "full":
        if not args.validated_smoke:
            parser.error("Full execution requires --validated-smoke after review")
        require_smoke(args.validated_smoke, identity)
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    out.chmod(0o700)
    provider_type = TracedProvider if args.capture_traces else CompatibleProvider
    provider = provider_type(args.base_url, config["model"], key, temperature=config["temperature"],
                                  max_tokens=config["max_output_tokens"], request_options=config["request_options"],
                                  **({"trace_dir": out / "preflight-wire"} if args.capture_traces else {}))
    try:
        preflight = provider.preflight()
        if preflight["model"].get("meta", {}).get("n_ctx") != config["context_window"]:
            raise ProviderError("context_window_not_verified")
    except ProviderError as exc:
        dump(out / "preflight.json", {"passed": False, "error": str(exc)})
        raise
    dump(out / "preflight.json", preflight)
    initial_environment = observe_strata(args.base_url, config["model"], key) if args.phase == "pilot" else None
    if args.phase == "preflight":
        print("Provider tool round trip passed; no benchmark executed.")
        return
    all_tasks = {task.id: task for task in load_prompts(args.prompts)}
    selected = ([config["smoke_prompt_id"]] if args.phase == "smoke" else
                config["pilot_tasks"] if args.phase == "pilot" else list(config["tasks"]))
    if not set(selected).issubset(all_tasks):
        parser.error("Configured task IDs missing from the existing prompt suite")
    snapshot = out / "repository"
    snapshot_info = make_snapshot(repo, snapshot)
    home = out / "mcp-state"
    home.mkdir()
    order = schedule(selected, 1 if args.phase in {"smoke", "pilot"} else 3, config["seed"])
    manifest = {"schema_version": 1, "phase": args.phase, "identity": identity,
                "created_at": datetime.now(timezone.utc).isoformat(), "repo": before,
                "config": config, "schedule": order, "snapshot": snapshot_info,
                "capture_traces": args.capture_traces,
                "prompt_set_sha256": digest(Path(args.prompts).read_bytes()),
                "skill_sha256": digest(skill.encode()), "harness_sources": sources,
                "mcp_binary_sha256": binary_sha,
                "config_sha256": config_sha,
                "harness_commit": git(ROOT, "rev-parse", "HEAD").decode().strip(),
                "harness_dirty": bool(git(ROOT, "status", "--porcelain")),
                "provider_endpoint_sha256": digest(args.base_url.encode()),
                "embedding_configuration": embedding, "preflight": preflight,
                "environment": {"declared": config["declared_environment"],
                                "observed_client_cpu": _detect_cpu(), "observed_client_ram_gb": _detect_ram_gb(),
                                "initial_gpu_vram": vram_samples(), "model": preflight["model"],
                                "server_config_change_monitoring": "unavailable"}}
    if args.phase == "pilot":
        rubric_path = ROOT / "bench/harness/experiments/TASK_QUALITY_RUBRIC_V1.md"
        manifest.update(technical_readiness=admission,
                        quality_rubric={"version": VERSION, "checks": TASKS, "document_sha256": digest(rubric_path.read_bytes())},
                        task_prompt_sha256={t: digest((all_tasks[t].prompt + ("\n\n" + all_tasks[t].prompt_suffix if all_tasks[t].prompt_suffix else "")).encode()) for t in selected},
                        system_prompt_sha256={a: digest(system_prompt(a, snapshot, skill).encode()) for a in "ABC"})
        manifest["strata_control_sha256"] = initial_environment["control_sha256"]
        dump(out / "environment_initial.json", initial_environment)
        (out / rubric_path.name).write_bytes(rubric_path.read_bytes())
    dump(out / "run_manifest.json", manifest)
    try:
        index = prepare_index(snapshot, home, config["setup_timeout_seconds"])
    except DeadlineExceeded:
        dump(out / "index_setup.json", {"error": "setup_timeout"})
        raise SystemExit("Index preparation timed out; no task runs started")
    dump(out / "index_setup.json", index)
    rows = []
    for task_id, arm, repetition in order:
        if repo_identity(repo) != before or tree_digest(snapshot) != snapshot_info["sha256"]:
            raise RuntimeError("Repository state changed; stopping experiment")
        if not inputs_unchanged():
            raise RuntimeError("Experiment inputs changed; stopping before the next attempt")
        print(f"Running {task_id} {arm} repetition {repetition}", flush=True)
        instance = out / "instances" / task_id / arm / f"run_{repetition}"
        if args.phase == "pilot":
            try:
                env_before = observe_strata(args.base_url, config["model"], key)
                if env_before["control_sha256"] != manifest["strata_control_sha256"]:
                    raise ValueError("Strata controls changed before the next attempt")
            except (ProviderError, ValueError):
                manifest["execution_stop"] = "environment_verification_before_run"
                dump(out / "run_manifest.json", manifest)
                write_report(out, manifest, rows, validate_pilot(rows, manifest))
                print("Environment verification failed; no next task attempt started.", flush=True)
                break
        row = run_one(all_tasks[task_id], arm, repetition, config, snapshot, home, key,
                      args.base_url, skill, before, repo, instance / "wire" if args.capture_traces else None)
        row["repository_unchanged"] &= tree_digest(snapshot) == snapshot_info["sha256"]
        row["inputs_unchanged"] = inputs_unchanged()
        if args.phase == "pilot":
            task = all_tasks[task_id]
            row["wire_controls_verified"] = audit_wire(instance / "wire", task.prompt + ("\n\n" + task.prompt_suffix if task.prompt_suffix else ""),
                                                        system_prompt(arm, snapshot, skill), config, admission["wire_schema_sha256"][arm])
            try:
                env_after = observe_strata(args.base_url, config["model"], key)
                row["environment_unchanged"] = env_after["control_sha256"] == manifest["strata_control_sha256"]
            except (ProviderError, ValueError):
                env_after = {"unavailable": True}
                row["environment_unchanged"] = False
            instance.mkdir(parents=True, exist_ok=True)
            dump(instance / "environment.json", {"before": env_before, "after": env_after})
        rows.append(row)
        instance.mkdir(parents=True, exist_ok=True)
        dump(instance / "result.json", row)
        validation = (validate_pilot(rows, manifest) if args.phase == "pilot" else
                      validate_smoke(rows, manifest) if args.phase == "smoke" else {"phase": "full"})
        write_report(out, manifest, rows, validation)
        print(f"  {row['status']}; grounding={row['quality_score']}; tokens={row['total_tokens']}; seconds={row['execution_seconds']}", flush=True)
        if not session_valid(row, config) or (args.phase == "pilot" and not validation["continue_execution"]):
            print("Technical execution/isolation failed; remaining attempts will not run.", flush=True)
            break
    dump(out / "reviews.json", {f"{r['task_id']}/{r['configuration']}/{r['run_number']}":
                               {"correctness_score": None, "evidence": "", "appropriate_mcp_selection": None,
                                "unnecessary_mcp_calls": None, "redundant_repository_searches": None,
                                **({"task_quality": review_template(r["task_id"])} if args.phase == "pilot" else {})} for r in rows})
    print(f"Report: {out / 'report.md'}. Stopped after {len(rows)} runs.", flush=True)


if __name__ == "__main__":
    main()
