"""Offline regressions for infrastructure-review findings; no inference services."""
import copy
import json
from pathlib import Path
import stat
import subprocess

import pytest

from bench.harness.experiments.pilot import audit_wire, session_valid
from bench.harness.experiments.provider import CompatibleProvider, ProviderError, TracedProvider
from bench.harness.experiments.tools import make_snapshot
from bench.harness.framework.models import Message


@pytest.mark.parametrize("finish", [None, "content_filter", "tool_calls", "unknown"])
def test_visible_content_with_invalid_final_finish_is_not_success(finish):
    provider = CompatibleProvider("unused", "model", "secret")
    with pytest.raises(ProviderError, match="invalid_finish_reason"):
        provider.parse({"choices": [{"message": {"content": "visible fragment"}, "finish_reason": finish}]})
    assert provider.protocol_errors == 1


@pytest.mark.parametrize("data", [None, [], {"choices": []},
    {"choices": [{"message": {"tool_calls": "malformed"}}]},
    {"choices": [{"message": {"tool_calls": [{"id": 7, "type": "function"}]}}]}])
def test_malformed_provider_envelopes_count_protocol_errors(data):
    provider = CompatibleProvider("unused", "model", "secret")
    with pytest.raises(ProviderError, match="invalid_provider_tool_protocol"):
        provider.parse(data)
    assert provider.protocol_errors == 1


def test_preflight_requires_authoritative_consistent_usage():
    provider = CompatibleProvider("unused", "model", "secret")
    responses = iter([
        {"choices": [{"message": {"tool_calls": [{"id": "probe", "type": "function",
          "function": {"name": "benchmark_echo", "arguments": '{"nonce":"pitlane-probe-7381"}'}}]}, "finish_reason": "tool_calls"}]},
        {"choices": [{"message": {"content": "pitlane-probe-7381"}, "finish_reason": "stop"}]},
        {"choices": [{"message": {"content": "pitlane-probe-7381"}, "finish_reason": "stop"}],
         "usage": {"prompt_tokens": 900, "completion_tokens": 10, "total_tokens": 910}},
    ])
    provider.request = lambda route, payload=None: {"data": [{"id": "model"}]} if route == "/models" else next(responses)
    with pytest.raises(ProviderError, match="token_accounting"):
        provider.preflight()


def test_private_wire_permissions_and_no_credential_serialization(tmp_path, monkeypatch):
    raw = {"choices": [{"message": {"content": "answer"}, "finish_reason": "stop"}]}
    monkeypatch.setattr(CompatibleProvider, "request", lambda *a, **k: raw)
    backend = TracedProvider("unused", "model", "credential-sentinel", trace_dir=tmp_path / "wire")
    backend.chat([Message("user", "task")], [])
    assert stat.S_IMODE((tmp_path / "wire").stat().st_mode) == 0o700
    for path in (tmp_path / "wire").iterdir():
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        assert "credential-sentinel" not in path.read_text()


def test_snapshot_excludes_nested_instructions_and_dotenv_variants(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    instruction_files = [
        "src/AGENTS.md",
        "src/SKILL.md",
        "CLAUDE.local.md",
        ".codex/config.toml",
        ".cursorrules",
        ".cursor/rules/test.mdc",
        ".clinerules/prompt.md",
        ".github/copilot-instructions.md",
        ".github/instructions/rust.instructions.md",
        ".env.local",
        ".env.production",
    ]
    for name in instruction_files + ["real.rs"]:
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("fixture")
    subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "test: fixture"], check=True)
    snapshot = tmp_path / "snapshot"
    info = make_snapshot(repo, snapshot)
    assert [str(p.relative_to(snapshot)) for p in snapshot.rglob("*") if p.is_file()] == ["real.rs"]
    assert set(instruction_files) <= set(info["excluded_tracked_instruction_files"])
    assert "real.rs" not in info["excluded_tracked_instruction_files"]


@pytest.mark.parametrize("fault", ["length", "usage", "context", "source", "transport"])
def test_shared_execution_gate_stops_failures_without_using_quality(fault):
    config = {"max_iterations": 25, "context_window": 131072, "max_output_tokens": 4096}
    row = {"status": "completed", "response_metadata": [{"finish_reason": "stop"}],
           "provider_protocol_errors": 0, "repository_unchanged": True, "inputs_unchanged": True,
           "tool_events": [], "usage_per_request": [{"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}],
           "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12, "correctness_score": 0}
    assert session_valid(row, config)
    changed = copy.deepcopy(row)
    if fault == "length": changed["response_metadata"][-1]["finish_reason"] = "length"
    if fault == "usage": changed["total_tokens"] = 11
    if fault == "context": config["context_window"] = 4100
    if fault == "source": changed["inputs_unchanged"] = False
    if fault == "transport": changed["tool_events"] = [{"error": "tool_transport_error"}]
    assert not session_valid(changed, config)


def _audited_run(phase="full"):
    """One valid arm-A attempt and the manifest fields the shared audit reads."""
    model = {"id": "m"}
    manifest = {
        "phase": phase,
        "schedule": [["t", "A", 1]],
        "config": {"max_iterations": 2, "max_output_tokens": 4096, "context_window": 131072},
        "technical_readiness": {"baseline_rows": [{"configuration": "A", "tool_schema_sha256": "native"}]},
        "skill_sha256": "skill",
        "task_prompt_sha256": {"t": "task"},
        "system_prompt_sha256": {"A": "neutral"},
        "preflight": {"model": model},
    }
    row = {
        "task_id": "t",
        "configuration": "A",
        "run_number": 1,
        "status": "completed",
        "response_metadata": [{"finish_reason": "stop"}],
        "provider_protocol_errors": 0,
        "tool_events": [],
        "repository_unchanged": True,
        "inputs_unchanged": True,
        "skill_loaded": False,
        "loaded_skill_sha256": None,
        "mcp_available": False,
        "mcp_tool_calls": 0,
        "tool_schema_sha256": "native",
        "task_prompt_sha256": "task",
        "system_prompt_sha256": "neutral",
        "model_metadata_before": {"data": [model]},
        "model_metadata_after": {"data": [model]},
        "usage_per_request": [{"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}],
        "prompt_tokens": 10,
        "completion_tokens": 2,
        "total_tokens": 12,
        "wire_controls_verified": True,
    }
    return manifest, row


def test_full_runs_share_the_pilot_audit_and_stop_rule():
    from bench.harness.experiments.navigation_skill import CHECKED_PHASES, should_stop, validate_phase

    assert CHECKED_PHASES == ("pilot", "full")
    manifest, row = _audited_run(phase="full")
    validation = validate_phase([row], manifest)
    assert validation["phase"] == "full"
    assert validation["technical_passed"] and validation["full_benchmark_executed"]
    assert not should_stop(row, manifest["config"], validation, True)
    # A breach that stops a pilot stops a full run too.
    for field, value, check in (
        ("skill_loaded", True, "skill_isolation"),
        ("wire_controls_verified", False, "wire_isolation"),
        ("tool_schema_sha256", "changed", "schemas_unchanged"),
    ):
        broken = copy.deepcopy(row)
        broken[field] = value
        validation = validate_phase([broken], manifest)
        assert not validation["checks"][check]
        assert should_stop(broken, manifest["config"], validation, True)
    # Smoke keeps its own three-arm comparison.
    assert "three_arms_one_task" in validate_phase([], {"phase": "smoke"})["checks"]
    assert validate_phase([], {"phase": "preflight"}) == {"phase": "preflight"}


def test_audited_phases_refuse_to_run_without_traces_or_a_validated_smoke(tmp_path):
    from bench.harness.experiments.navigation_skill import DEFAULT_CONFIG, main

    for phase in ("pilot", "full"):
        for supplied in ([], ["--capture-traces"], ["--validated-smoke", str(tmp_path / "smoke")]):
            with pytest.raises(SystemExit) as stopped:
                main(["--phase", phase, "--config", str(DEFAULT_CONFIG), "--out", str(tmp_path / "out"), *supplied])
            assert stopped.value.code == 2
    assert not (tmp_path / "out").exists()


def test_unknown_tool_guidance_stops_before_any_request(tmp_path):
    from bench.harness.experiments.navigation_skill import DEFAULT_CONFIG, main
    from bench.harness.experiments.tools import GUIDANCE_MODES, IsolatedMCP, guidance_mode

    config = json.loads(DEFAULT_CONFIG.read_text())
    config["tool_guidance"] = "off"
    config_path = tmp_path / "config.json"
    config_path.write_text(json.dumps(config))
    with pytest.raises(SystemExit) as stopped:
        main(["--phase", "smoke", "--config", str(config_path), "--out", str(tmp_path / "out")])
    assert stopped.value.code == 2
    assert not (tmp_path / "out").exists()
    # The server treats anything but `none` as full, so only two modes are accepted.
    assert GUIDANCE_MODES == ("full", "none")
    assert guidance_mode({}) == "full"
    assert guidance_mode(config | {"tool_guidance": "none"}) == "none"
    for typo in ("off", "None", "FULL", ""):
        with pytest.raises(ValueError, match="tool_guidance"):
            guidance_mode({"tool_guidance": typo})
    with pytest.raises(ValueError, match="guidance"):
        IsolatedMCP(tmp_path, guidance="off")


class _StatsServer:
    """Minimal MCP stand-in that answers whatever the guidance probe extracts."""

    def __init__(self, payload):
        self.payload = payload
        self.calls = []

    def _call_tool(self, name, arguments):
        self.calls.append((name, arguments))
        return {}

    def _extract_content(self, raw):
        return self.payload


def test_declared_tool_guidance_must_be_observed_on_the_server(tmp_path):
    from bench.harness.experiments.navigation_skill import GuidanceMismatch, verify_guidance

    with_steering = json.dumps({"total_symbols": 1, "steering": {"recommended_next_tool": "locate_code"}})
    without_steering = json.dumps({"total_symbols": 1})
    server = _StatsServer(without_steering)
    assert verify_guidance(server, tmp_path, "none") == {
        "declared": "none",
        "steering_observed": False,
        "verified": True,
    }
    # The probe names the snapshot and nothing else.
    assert server.calls == [("get_index_stats", {"project": str(tmp_path)})]
    assert verify_guidance(_StatsServer(with_steering), tmp_path, "full")["steering_observed"] is True
    # A server built before the switch keeps attaching steering under `none`.
    for payload, declared in ((with_steering, "none"), (without_steering, "full")):
        with pytest.raises(GuidanceMismatch, match="Tool guidance mismatch") as stopped:
            verify_guidance(_StatsServer(payload), tmp_path, declared)
        assert stopped.value.detail == {"declared": declared, "steering_observed": declared != "full"}
    with pytest.raises(GuidanceMismatch, match="could not be verified") as stopped:
        verify_guidance(_StatsServer("failed to deserialize parameters"), tmp_path, "none")
    assert stopped.value.detail["steering_observed"] is None
    # The server reports some failures as an error object in the content with
    # isError unset. Under `none` that must not pass as "steering omitted".
    for declared in ("none", "full"):
        with pytest.raises(GuidanceMismatch, match="could not be verified") as stopped:
            verify_guidance(_StatsServer(json.dumps({"error": {"code": "INTERNAL_ERROR"}})), tmp_path, declared)
        assert stopped.value.detail == {"declared": declared, "steering_observed": None}


def test_index_preparation_rejects_an_error_reported_in_content(tmp_path):
    from bench.harness.experiments.tools import IsolatedMCP

    def server(payload):
        class Server(IsolatedMCP):
            def _call_tool(self, name, arguments):
                text = json.dumps(payload)
                return {"result": {"isError": False, "content": [{"type": "text", "text": text}]}}

        return Server(tmp_path)

    with pytest.raises(RuntimeError, match="index_preparation_failed"):
        server({"error": {"code": "INTERNAL_ERROR", "message": "Cannot canonicalize path"}})._ensure_project_ready(str(tmp_path))
    ready = server({"status": "ready", "index": {"status": "indexed"}})
    ready._ensure_project_ready(str(tmp_path))
    assert ready.ready["status"] == "ready"


def test_index_preparation_records_verified_guidance_and_shuts_down_on_mismatch(tmp_path, monkeypatch):
    from bench.harness.experiments import navigation_skill as experiment

    class FakeIsolatedMCP:
        steering = False
        started = []

        def __init__(self, home, tier="default", guidance="full"):
            self.home, self.tier, self.guidance = home, tier, guidance
            self.ready = {"index": {"embeddings": "disabled"}, "embeddings": {"status": "disabled"}}
            self.closed = False
            self.started.append(self)

        def startup(self, repo_path):
            self.repo_path = repo_path

        def _call_tool(self, name, arguments):
            payloads = {
                "server_info": {"name": "pitlane-mcp", "version": "test"},
                "get_index_stats": {"total_symbols": 1, **({"steering": {}} if self.steering else {})},
            }
            self.text = json.dumps(payloads[name])
            return {"result": {"content": []}}

        def _extract_content(self, raw):
            return self.text

        def shutdown(self):
            self.closed = True

    monkeypatch.setattr(experiment, "IsolatedMCP", FakeIsolatedMCP)
    index = experiment.prepare_index(tmp_path, tmp_path / "home", 5, "none")
    assert index["tool_guidance"] == {"declared": "none", "steering_observed": False, "verified": True}
    assert index["server"]["name"] == "pitlane-mcp"
    server = FakeIsolatedMCP.started[-1]
    assert (server.tier, server.guidance, server.repo_path, server.closed) == ("all", "none", str(tmp_path), True)
    monkeypatch.setattr(FakeIsolatedMCP, "steering", True)
    with pytest.raises(experiment.GuidanceMismatch):
        experiment.prepare_index(tmp_path, tmp_path / "home", 5, "none")
    assert FakeIsolatedMCP.started[-1].closed


def test_server_calls_cannot_leave_the_snapshot_or_change_the_index(tmp_path):
    from bench.harness.experiments.tools import ExperimentTools
    from bench.harness.framework.models import ToolDef

    class MCP:
        def __init__(self):
            self.calls = []

        def get_tool_definitions(self):
            def schema(properties, required):
                return {"type": "object", "properties": properties, "required": required}

            text = {"type": "string"}
            return [
                ToolDef("read_code_unit", "d", schema({"project": text, "file_path": text}, ["project"])),
                ToolDef("search_knowledge", "d", schema({"project": text, "document": text}, ["project", "document"])),
                ToolDef("ensure_project_ready", "d", schema({"path": text, "watch": {"type": "boolean"}}, ["path"])),
                ToolDef("doctor", "d", schema({"project": text, "repair": {"type": "boolean"}}, ["project"])),
            ]

        def _call_tool(self, name, arguments):
            self.calls.append((name, arguments))
            return {"result": {"content": [{"type": "text", "text": "{}"}]}}

        def _extract_content(self, raw):
            return raw["result"]["content"][0]["text"]

    mcp = MCP()
    tools = ExperimentTools(tmp_path, mcp)
    root = str(tmp_path)
    for name, arguments in (
        ("read_code_unit", {"project": root, "file_path": "../outside.rs"}),
        ("read_code_unit", {"project": root, "file_path": "/etc/passwd"}),
        ("search_knowledge", {"project": root, "document": "../../README.md"}),
        ("read_code_unit", {"project": root + "/elsewhere", "file_path": "src/lib.rs"}),
    ):
        assert tools.execute(name, arguments).content == "Error: path_outside_snapshot"
    for name, arguments in (
        ("ensure_project_ready", {"path": root, "watch": True}),
        ("ensure_project_ready", {"path": root, "force": True}),
        ("doctor", {"project": root, "repair": True}),
    ):
        assert tools.execute(name, arguments).content == "Error: read_only_policy"
    assert mcp.calls == []
    tools.execute("ensure_project_ready", {"path": root})
    tools.execute("read_code_unit", {"project": root, "file_path": "src/lib.rs"})
    assert [arguments for _, arguments in mcp.calls] == [
        {"path": root, "watch": False},
        {"project": root, "file_path": "src/lib.rs"},
    ]


def test_audited_attempt_is_recorded_and_its_traces_stay_private(tmp_path, monkeypatch):
    from types import SimpleNamespace

    from bench.harness.experiments import navigation_skill as experiment
    from bench.harness.experiments.navigation_skill import run_one

    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "a.rs").write_text("fn real() {}\n")
    subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
    subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "test: fixture"], check=True)
    snapshot = tmp_path / "snapshot"
    make_snapshot(repo, snapshot)
    home = tmp_path / "mcp-state"
    home.mkdir()
    responses = iter([
        {"choices": [{"message": {"tool_calls": [{"id": "call1", "type": "function", "function": {"name": "read_file", "arguments": '{"path":"a.rs"}'}}]}, "finish_reason": "tool_calls"}]},
        {"choices": [{"message": {"content": "fn real() is in a.rs."}, "finish_reason": "stop"}]},
    ])
    usage = {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
    monkeypatch.setattr(CompatibleProvider, "request", lambda self, route, payload=None: (
        {"data": [{"id": "model"}]} if route == "/models" else {**next(responses), "usage": usage}))
    config = {"model": "model", "temperature": 0, "max_output_tokens": 4096, "request_options": {},
              "context_window": 131072, "timeout_seconds": 60, "setup_timeout_seconds": 60,
              "max_iterations": 2, "reserve_final_turn": True}
    task = SimpleNamespace(id="task", category="symbol_grounding", prompt="Where is real()?", prompt_suffix=None)
    instance = tmp_path / "instances/task/A/run_1"
    instance.mkdir(parents=True, mode=0o700)
    row = run_one(task, "A", 1, config, snapshot, home, "secret", "unused", "skill",
                  experiment.repo_identity(repo), repo, instance / "wire")
    assert row["status"] == "completed" and row["termination_kind"] == "budget_synthesis"
    assert (row["tool_calls"], row["total_tokens"], row["mcp_available"], row["skill_loaded"]) == (1, 24, False, False)
    # main() records input integrity after every attempt.
    row["inputs_unchanged"] = True
    assert session_valid(row, config)
    # The transmitted requests pass the wire audit that gates every audited attempt.
    schema = experiment.digest(json.loads((instance / "wire/request_01.json").read_text())["tools"])
    assert audit_wire(instance / "wire", task.prompt, experiment.system_prompt("A", snapshot, "skill"), config, schema)
    # Wire files were already private; the parsed conversation and observations hold the same material.
    for name in ("observations.json", "parsed_result.json"):
        assert stat.S_IMODE((instance / name).stat().st_mode) == 0o600
    assert stat.S_IMODE((instance / "wire").stat().st_mode) == 0o700


def test_report_states_the_phase_and_the_verified_guidance(tmp_path):
    from bench.harness.experiments.navigation_skill import write_report

    manifest = {"phase": "full", "environment": {"declared": {}}, "repo": {"commit": "abc123"},
                "config": {"model": "model"}, "identity": "identity", "schedule": [], "repetitions": 1}
    (tmp_path / "index_setup.json").write_text(json.dumps({
        "cold_setup_seconds": 1.5,
        "tool_guidance": {"declared": "none", "steering_observed": False, "verified": True}}))
    write_report(tmp_path, manifest, [], {"phase": "full"})
    report = (tmp_path / "report.md").read_text()
    assert "Phase: full. Runs: 0." in report
    assert "Tool guidance `none` was verified before the first run: get_index_stats omitted its steering field." in report
    assert "invoke full with --validated-smoke --capture-traces" in report
    assert json.loads((tmp_path / "summary.json").read_text())["execution_validation"] == {"phase": "full"}
