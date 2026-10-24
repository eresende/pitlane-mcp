"""Offline regressions for infrastructure-review findings; no inference services."""
import copy
import json
from pathlib import Path
import stat
import subprocess

import pytest

from bench.harness.experiments.pilot import session_valid
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
    for name in ("src/AGENTS.md", "src/SKILL.md", ".codex/config.toml", ".env.local", ".env.production", "real.rs"):
        path = repo / name
        path.parent.mkdir(exist_ok=True)
        path.write_text("fixture")
    subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "test: fixture"], check=True)
    snapshot = tmp_path / "snapshot"
    make_snapshot(repo, snapshot)
    assert [str(p.relative_to(snapshot)) for p in snapshot.rglob("*") if p.is_file()] == ["real.rs"]


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
