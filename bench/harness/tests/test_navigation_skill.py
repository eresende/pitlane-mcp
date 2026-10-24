"""Regression tests for experimental controls, accounting and failure capture."""
import json
from pathlib import Path
import subprocess
import time

import pytest

from bench.harness.experiments.navigation_skill import (
    system_prompt, schedule, validate_smoke, require_smoke, load_reviews,
)
from bench.harness.experiments.provider import CompatibleProvider, ProviderError, usage_summary, read_key
from bench.harness.experiments.tools import ExperimentTools, DeadlineExceeded, deadline, make_snapshot, tree_digest
from bench.harness.experiments.report import describe, summaries
from bench.harness.framework.agentic_loop import AgenticLoop
from bench.harness.framework.models import ToolDef, Message


def response(content="answer", calls=None, usage=None):
    return {"choices": [{"message": {"content": content, "tool_calls": calls}, "finish_reason": "stop"}],
            **({"usage": usage} if usage is not None else {})}


def test_skill_isolation_and_legacy_prompt_override(tmp_path):
    sentinel = "unique skill payload"
    assert system_prompt("A", tmp_path, sentinel) == system_prompt("B", tmp_path, sentinel)
    assert sentinel not in system_prompt("B", tmp_path, sentinel)
    assert system_prompt("C", tmp_path, sentinel) == system_prompt("A", tmp_path, sentinel) + "\n\n" + sentinel
    backend = CompatibleProvider("unused", "model", "secret")
    captured = []
    backend.request = lambda route, payload: captured.append(payload) or response()
    tools = ExperimentTools(tmp_path)
    result = AgenticLoop().run("identical task", backend, tools, mode="mcp", system_prompt="neutral")
    assert result.status == "completed"
    assert captured[0]["messages"] == [{"role": "system", "content": "neutral"}, {"role": "user", "content": "identical task"}]
    assert {t["function"]["name"] for t in captured[0]["tools"]} == {"read_file", "grep_search", "list_directory"}


def test_free_vram_headroom_uses_the_tightest_device():
    from bench.harness.experiments.navigation_skill import DEFAULT_CONFIG, free_vram_gib
    samples = {'card0': {'mem_info_vram_used': 1 * 2**30, 'mem_info_vram_total': 8 * 2**30},
               'card1': {'mem_info_vram_used': int(19.84 * 2**30), 'mem_info_vram_total': int(23.98 * 2**30)}}
    assert 4.0 < free_vram_gib(samples) < 4.2
    assert free_vram_gib({'card0': {'mem_info_vram_used': None, 'mem_info_vram_total': None}}) is None
    assert free_vram_gib(None) is None
    # A too-tight profile must be refusable before any task run.
    assert json.loads(DEFAULT_CONFIG.read_text())['min_free_vram_gib'] >= 2


def test_cumulative_provider_usage_missing_and_reasoning_subset():
    backend = CompatibleProvider("unused", "model", "secret")
    backend.parse(response(usage={"prompt_tokens": 10, "completion_tokens": 8, "total_tokens": 18,
                                  "completion_tokens_details": {"reasoning_tokens": 5},
                                  "prompt_tokens_details": {"cached_tokens": 4}}))
    backend.parse(response(usage={"prompt_tokens": 30, "completion_tokens": 4, "total_tokens": 34,
                                  "prompt_tokens_details": {"cached_tokens": 6}}))
    assert usage_summary(backend.usages) == {"prompt_tokens": 40, "completion_tokens": 12, "total_tokens": 52,
                                             "reasoning_tokens": None, "cached_tokens": 10}
    backend.parse(response())
    assert all(v is None for v in usage_summary(backend.usages).values())
    assert usage_summary([{"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0, "reasoning_tokens": 0}])["total_tokens"] == 0


@pytest.mark.parametrize("arguments", ["{bad json", "[]", "null", '{"path":12}', '{}'])
def test_invalid_arguments_are_rejected_and_counted(tmp_path, arguments):
    backend = CompatibleProvider("unused", "model", "secret")
    parsed = backend.parse(response(calls=[{"id": "call1", "type": "function", "function": {"name": "read_file", "arguments": arguments}}]))
    tools = ExperimentTools(tmp_path)
    call = parsed.message.tool_calls[0]
    assert tools.execute(call.name, call.arguments).content.startswith("Error: invalid_arguments")
    assert tools.events[0]["error"] == "invalid_arguments"


def test_errors_retries_duplicates_and_escape(tmp_path):
    (tmp_path / "file.rs").write_text("fn actual_symbol() {}")
    tools = ExperimentTools(tmp_path)
    tools.execute("read_file", {"path": "missing"})
    tools.execute("read_file", {"path": "missing"})
    tools.execute("read_file", {"path": "file.rs"})
    tools.execute("read_file", {"path": "file.rs"})
    tools.execute("read_file", {"path": "/etc/passwd"})
    tools.execute("list_directory", {"glob": "../../*"})
    tools.execute("locate_code", {})  # Not offered in A; cannot be dispatched to MCP.
    assert tools.events[1]["retry_after_failure"]
    assert tools.events[0]["arguments"] == {"path": "missing"}
    assert tools.events[3]["redundant_successful_call"]
    assert tools.events[4]["error"] == tools.events[5]["error"] == "path_outside_snapshot"
    assert tools.events[6]["error"] == "unknown_tool"
    assert tools.total_response_bytes() == sum(e["result_bytes"] for e in tools.events)


def test_mcp_iserror_no_argument_repair_and_native_access(tmp_path):
    class MCP:
        def get_tool_definitions(self):
            return [ToolDef("locate_code", "server description", {"type": "object", "required": ["project", "query"], "properties": {"project": {"type": "string"}, "query": {"type": "string"}}})]
        def _call_tool(self, name, args):
            return {"result": {"isError": True, "content": [{"type": "text", "text": "failure"}]}}
        def _extract_content(self, raw):
            return raw["result"]["content"][0]["text"]
    tools = ExperimentTools(tmp_path, MCP())
    assert tools.native_names.issubset(tools.definitions)
    tools.execute("locate_code", {"query": "something"})
    assert tools.events[-1]["error"] == "invalid_arguments"
    tools.execute("locate_code", {"project": str(tmp_path), "query": "something"})
    assert tools.events[-1]["error"] == "mcp_tool_error"


def test_protocol_never_accepts_inline_or_reasoning_as_final():
    backend = CompatibleProvider("unused", "model", "secret")
    with pytest.raises(ProviderError, match="empty_or_unstructured"):
        backend.parse(response("<tool_call>not a structured call</tool_call>"))
    data = response(None)
    data["choices"][0]["message"]["reasoning_content"] = "private thought"
    with pytest.raises(ProviderError):
        backend.parse(data)
    data = response("partial")
    data["choices"][0]["finish_reason"] = "length"
    with pytest.raises(ProviderError, match="output_truncated"):
        backend.parse(data)


def test_actual_tool_round_trip_serialization():
    backend = CompatibleProvider("unused", "model", "secret")
    captures = []
    def request(route, payload=None):
        if route == "/models":
            return {"data": [{"id": "model"}]}
        captures.append(payload)
        if len(captures) == 1:
            return response(None, [{"id": "call-id", "type": "function", "function": {"name": "benchmark_echo", "arguments": '{"nonce":"pitlane-probe-7381"}'}}], usage={"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15})
        return response("pitlane-probe-7381", usage={"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15})
    backend.request = request
    assert backend.preflight()["passed"]
    assert captures[1]["messages"][-1] == {"role": "tool", "content": "pitlane-probe-7381", "tool_call_id": "call-id"}
    assert captures[1]["messages"][-2]["tool_calls"][0]["id"] == "call-id"
    assert "private thought" not in json.dumps(captures)


def test_rotation_has_45_fresh_instances_and_balances_order():
    tasks = list("12345")
    order = schedule(tasks, 3, 42)
    assert len(order) == len(set(order)) == 45
    assert order == schedule(tasks, 3, 42)
    for task in tasks:
        for arm in "ABC":
            positions = []
            for repetition in range(1, 4):
                arms = [a for t, a, r in order if t == task and r == repetition]
                positions.append(arms.index(arm))
            assert sorted(positions) == [0, 1, 2]


def test_snapshot_excludes_untracked_and_instruction_files(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    (repo / "main.rs").write_text("fn real_function() {}")
    (repo / "AGENTS.md").write_text("LEAKED_SKILL")
    subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "test: fixture"], check=True)
    (repo / "untracked_secret").write_text("secret")
    exported = tmp_path / "snapshot"
    info = make_snapshot(repo, exported)
    assert info["excluded_tracked_instruction_files"] == ["AGENTS.md"]
    assert [p.name for p in exported.iterdir()] == ["main.rs"]
    assert info["sha256"] == tree_digest(exported)


def test_hard_deadline_interrupts_blocking_call():
    started = time.monotonic()
    with pytest.raises(DeadlineExceeded):
        with deadline(0.02):
            time.sleep(1)
    assert time.monotonic() - started < 0.5


def test_admission_requires_technical_readiness_independent_of_correctness(tmp_path):
    manifest = {"phase": "smoke", "identity": "identity", "skill_sha256": "skill-hash", "config": {"context_window": 131072},
                "preflight": {"model": {"meta": {"n_ctx": 131072}}}}
    rows = [{"task_id": "task", "configuration": a, "run_number": 1, "status": "completed",
             "tool_calls": 1, "failed_tool_calls": 0, "provider_protocol_errors": 0,
             "tool_events": [{"error": None}],
             "mcp_tool_calls": int(a != "A"), "mcp_available": a != "A", "skill_loaded": a == "C",
             "loaded_skill_sha256": "skill-hash" if a == "C" else None,
             "task_prompt_sha256": "task", "system_prompt_sha256": "skill" if a == "C" else "base",
             "tool_schema_sha256": "native" if a == "A" else "mcp", "repository_unchanged": True,
             "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15, "task_success": None,
             "inputs_unchanged": True, "response_metadata": [{"finish_reason": "stop"}],
             "usage_per_request": [{"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}]} for a in "ABC"]
    (tmp_path / "run_manifest.json").write_text(json.dumps(manifest))
    (tmp_path / "results.jsonl").write_text("\n".join(json.dumps(r) for r in rows))
    assert validate_smoke(rows, manifest)["technical_passed"]
    require_smoke(tmp_path, "identity")  # Unreviewed quality is not a technical failure.
    reviews = {f"task/{a}/1": {"correctness_score": 1, "evidence": "Verified against snapshot/main.rs"} for a in "ABC"}
    (tmp_path / "reviews.json").write_text(json.dumps(reviews))
    require_smoke(tmp_path, "identity")
    with pytest.raises(ValueError):
        require_smoke(tmp_path, "different model or skill")
    reviews["task/A/1"]["correctness_score"] = 0.5
    (tmp_path / "reviews.json").write_text(json.dumps(reviews))
    require_smoke(tmp_path, "identity")  # Partial quality remains an observation.
    rows[0]["status"] = "error"
    (tmp_path / "results.jsonl").write_text("\n".join(json.dumps(r) for r in rows))
    with pytest.raises(ValueError):
        require_smoke(tmp_path, "identity")


def test_missing_metrics_and_variability_are_not_fabricated():
    assert describe([None, None])["mean"] is None
    assert describe([2])["stdev"] is None
    assert describe([2, 4])["median"] == 3
    row = {"configuration": "A", "status": "completed", "task_success": None, "tool_calls": 0, "failed_tool_calls": 0}
    result = summaries([row])["A"]
    assert result["tool_failure_rate"] is None
    assert result["successful_only"]["total_tokens"]["n"] == 0


def test_key_file_is_literal_and_not_shell_executed(tmp_path):
    path = tmp_path / ".env"
    path.write_text('OTHER=unused\nSTRATA_API_KEY="literal-$(false)"\n')
    assert read_key("STRATA_API_KEY", str(path)) == "literal-$(false)"


@pytest.mark.parametrize("link,allowed", [("src", True), ("../../outside", False)])
def test_snapshot_preserves_only_internal_links(tmp_path, link, allowed):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    (repo / "src").mkdir()
    (repo / "src/lib.rs").write_text("fn real_function() {}")
    (repo / "alias").symlink_to(link)
    subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "test: fixture"], check=True)
    if allowed:
        make_snapshot(repo, tmp_path / "snapshot")
        assert (tmp_path / "snapshot/alias/lib.rs").read_text() == "fn real_function() {}"
    else:
        with pytest.raises(ValueError, match="symlinks"):
            make_snapshot(repo, tmp_path / "snapshot")


def test_failed_request_makes_session_total_unavailable():
    backend = CompatibleProvider("unused", "model", "secret")
    backend.parse(response(usage={"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}))
    def fail(*_):
        raise ProviderError("provider_http_500")
    backend.request = fail
    with pytest.raises(ProviderError):
        backend.chat([Message("user", "task")], [])
    assert usage_summary(backend.usages)["total_tokens"] is None


def test_infrastructure_failure_does_not_fabricate_metrics(tmp_path, monkeypatch):
    from types import SimpleNamespace
    from bench.harness.experiments import navigation_skill as experiment
    def fail(*_):
        raise RuntimeError("Potentially sensitive provider body")
    monkeypatch.setattr(experiment, "_run_one", fail)
    monkeypatch.setattr(experiment, "repo_identity", lambda _: {})
    row = experiment.run_one(SimpleNamespace(id="task", category="architecture"), "B", 1,
                             {"model": "model", "quantization": "iq1_m"},
                             tmp_path, tmp_path, "secret", "unused", "skill", {}, tmp_path)
    assert row["tool_calls"] is None
    assert row["total_tokens"] is None
    assert row["execution_seconds"] is None
    assert "sensitive" not in json.dumps(row)
    assert summaries([row])["B"]["tool_failure_rate"] is None


@pytest.mark.parametrize("request_budget", [2, 25])
def test_reserved_final_turn_within_budget_and_no_extra_tools(tmp_path, request_budget):
    from bench.harness.framework.agentic_loop import FINAL_TURN_PROMPT
    backend = CompatibleProvider("unused", "model", "secret")
    captures = []
    def request(route, payload):
        captures.append(payload)
        if len(captures) < request_budget:
            return response(None, [{"id": f"call{len(captures)}", "type": "function", "function": {"name": "read_file", "arguments": '{"path":"file.rs"}'}}])
        return response("file.rs contains actual_symbol")
    backend.request = request
    (tmp_path / "file.rs").write_text("fn actual_symbol() {}")
    tools = ExperimentTools(tmp_path)
    result = AgenticLoop().run("task", backend, tools, max_iterations=request_budget,
                               system_prompt="neutral", reserve_final_turn=True)
    assert result.status == "completed"
    assert len(captures) == request_budget
    assert len(tools.events) == request_budget - 1
    assert "tools" in captures[0] and "tools" not in captures[-1]
    assert captures[-1]["messages"][-1] == {"role": "user", "content": FINAL_TURN_PROMPT}
    assert captures[-1]["messages"][-2]["role"] == "tool"
    assert captures[-1]["messages"][0] == {"role": "system", "content": "neutral"}


def test_tool_request_during_reserved_final_turn_is_failure(tmp_path):
    backend = CompatibleProvider("unused", "model", "secret")
    backend.request = lambda *args: response("not a final answer", [{"id": "call1", "type": "function", "function": {"name": "read_file", "arguments": '{"path":"file.rs"}'}}])
    tools = ExperimentTools(tmp_path)
    result = AgenticLoop().run("task", backend, tools, max_iterations=1,
                               system_prompt="neutral", reserve_final_turn=True)
    assert result.status == "error"
    assert result.error == "tool_call_during_final_synthesis"
    assert result.final_answer == ""
    assert not tools.events


def test_legacy_iteration_budget_does_not_force_synthesis(tmp_path):
    backend = CompatibleProvider("unused", "model", "secret")
    captures = []
    def request(route, payload):
        captures.append(payload)
        return response(None, [{"id": "call1", "type": "function", "function": {"name": "read_file", "arguments": '{"path":"file.rs"}'}}])
    backend.request = request
    (tmp_path / "file.rs").write_text("fn actual_symbol() {}")
    result = AgenticLoop().run("task", backend, ExperimentTools(tmp_path), max_iterations=1,
                               system_prompt="neutral")
    assert result.status == "max_iterations"
    assert "tools" in captures[0]
    assert len(captures[0]["messages"]) == 2


def test_content_with_tool_calls_is_not_mistaken_for_final_answer(tmp_path):
    backend = CompatibleProvider("unused", "model", "secret")
    data = response("I will check the file.", [{"id": "call1", "type": "function", "function": {"name": "read_file", "arguments": '{"path":"file.rs"}'}}])
    # Some compatible providers use stop with structured tool calls. Tool calls
    # still take precedence; this is not a final answer.
    backend.request = lambda *args: data
    (tmp_path / "file.rs").write_text("fn actual_symbol() {}")
    tools = ExperimentTools(tmp_path)
    result = AgenticLoop().run("task", backend, tools, max_iterations=1, system_prompt="neutral")
    assert result.status == "max_iterations" and result.final_answer == ""
    assert len(tools.events) == 1
    assert backend.response_metadata[0]["finish_reason"] == "stop"
    assert backend.response_metadata[0]["raw_tool_call_count"] == 1
    assert backend.response_metadata[0]["assistant_content_bytes"] > 0


def test_neutral_completion_guidance_identical_and_tool_agnostic(tmp_path):
    from bench.harness.experiments.navigation_skill import NEUTRAL
    assert "Task completion:" in NEUTRAL
    assert "Pitlane" not in NEUTRAL and "MCP" not in NEUTRAL
    assert system_prompt("A", tmp_path, "skill") == system_prompt("B", tmp_path, "skill")
    assert system_prompt("C", tmp_path, "skill").startswith(system_prompt("A", tmp_path, "skill"))


def test_trace_capture_preserves_wire_without_headers_or_key(tmp_path, monkeypatch):
    from bench.harness.experiments.provider import TracedProvider
    observed = []
    raw = response("answer", usage={"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15})
    raw["choices"][0]["message"]["reasoning_content"] = "private diagnostic reasoning"
    def request(self, route, payload=None):
        observed.append((route, payload))
        return raw
    monkeypatch.setattr(CompatibleProvider, "request", request)
    backend = TracedProvider("unused", "model", "credential-sentinel-913", trace_dir=tmp_path / "wire")
    parsed = backend.chat([Message("user", "task")], [])
    assert parsed.message.content == "answer"
    assert json.loads((tmp_path / "wire/request_01.json").read_text()) == observed[0][1]
    assert json.loads((tmp_path / "wire/response_01.json").read_text()) == raw
    assert backend.observations[0]["reasoning_content"] == "private diagnostic reasoning"
    assert "credential-sentinel-913" not in "".join(p.read_text() for p in (tmp_path / "wire").iterdir())
    assert "Authorization" not in (tmp_path / "wire/request_01.json").read_text()
