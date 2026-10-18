"""Evidence extraction and common arm guidance must not introduce answer facts."""
import copy
import json
from types import SimpleNamespace

import pytest

from bench.harness.experiments.compare_evidence import extract_recorded_source, presentation_payload
from bench.harness.experiments.completion_guidance import ANSWER_GUIDANCE, CAUSAL_GUIDANCE
from bench.harness.experiments.navigation_skill import system_prompt
from bench.harness.experiments.provider import CompatibleProvider


def fixture_messages(tool_content):
    return {"model": "existing", "temperature": 0, "max_tokens": 4096, "stream": False,
        "messages": [{"role": "system", "content": "existing system and skill"},
                     {"role": "user", "content": "existing question"},
                     {"role": "assistant", "content": "SENTINEL_PLAN", "tool_calls": [
                         {"id": "read1", "function": {"name": "read_code_unit", "arguments": "{}"}}]},
                     {"role": "tool", "tool_call_id": "read1", "content": json.dumps(tool_content)}],
        "tools": [{"type": "function"}]}


def test_extraction_uses_recorded_source_not_new_snapshot_evidence(tmp_path):
    (tmp_path / "code.rs").write_text("fn first() {}\nfn unobserved() {}\n")
    recorded = fixture_messages({"file": "code.rs", "source": "fn first() {}", "line_start": 1,
                                 "guidance": "SENTINEL_EXPLANATION"})
    before = copy.deepcopy(recorded)
    compact, audit = extract_recorded_source(recorded, tmp_path)
    assert recorded == before
    assert "fn first() {}" in compact
    assert all(s not in compact for s in ("unobserved", "SENTINEL_PLAN", "SENTINEL_EXPLANATION"))
    assert audit["source_blocks"][0]["origins"] == ["read1"]
    for presentation in ("accumulated", "compact"):
        payload = presentation_payload(recorded, presentation, compact)
        assert payload["messages"][:2] == recorded["messages"][:2]
        assert payload["messages"][-1] == {"role": "user", "content": CAUSAL_GUIDANCE}
        assert {k: v for k, v in payload.items() if k != "messages"} == {
            k: v for k, v in recorded.items() if k not in {"messages", "tools"}}
        assert "reasoning_effort" not in payload
    assert presentation_payload(recorded, "accumulated", compact)["messages"][:-1] == recorded["messages"]


def test_extraction_rejects_corrected_or_unrecorded_source(tmp_path):
    (tmp_path / "code.rs").write_text("fn first() {}\n")
    recorded = fixture_messages({"file": "code.rs", "source": "fn corrected() {}", "line_start": 1})
    with pytest.raises(ValueError, match="does not match"):
        extract_recorded_source(recorded, tmp_path)


def test_extract_investigation_strips_only_generated_truncation_marker(tmp_path):
    (tmp_path / "code.rs").write_text("fn first() {}\nfn second() {}\n")
    data = {"symbols": [{"file": "code.rs", "line_start": 1, "lines_shown": 1}],
            "answer": "SERVER_EXPLANATION\n```rust\nfn first() {}\n\n// ... (1 more lines)\n```"}
    recorded = fixture_messages(data)
    recorded["messages"][2]["tool_calls"][0]["function"]["name"] = "investigate"
    compact, _ = extract_recorded_source(recorded, tmp_path)
    assert "fn first() {}" in compact and "second" not in compact
    assert "SERVER_EXPLANATION" not in compact and "more lines" not in compact


def test_all_arm_paths_use_same_natural_and_reserved_completion_guidance(tmp_path, monkeypatch):
    from bench.harness.experiments import navigation_skill as nav
    class MCP:
        def __init__(self, *args): pass
        def startup(self, *args): pass
        def shutdown(self): pass
        def get_tool_definitions(self): return []
    captures = []
    def request(self, route, payload=None):
        if route == "/models": return {"data": []}
        captures.append(payload)
        return {"choices": [{"message": {"content": "answer"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}
    monkeypatch.setattr(CompatibleProvider, "request", request)
    monkeypatch.setattr(nav, "IsolatedMCP", MCP)
    monkeypatch.setattr(nav, "repo_identity", lambda _: {})
    config = {"model": "model", "quantization": "iq1_m", "temperature": 0, "max_output_tokens": 4096,
              "request_options": {}, "timeout_seconds": 10, "setup_timeout_seconds": 10,
              "max_iterations": 1, "reserve_final_turn": True}
    task = SimpleNamespace(id="task", category="symbol_grounding", prompt="task", prompt_suffix="")
    for arm in "ABC":
        row = nav._run_one(task, arm, 1, config, tmp_path, tmp_path, "secret", "unused", "SKILL_SENTINEL", {}, tmp_path)
        assert row["termination_kind"] == "budget_synthesis"
        assert captures[-1]["messages"][-1]["content"] == CAUSAL_GUIDANCE
        assert ANSWER_GUIDANCE in captures[-1]["messages"][0]["content"]
        assert "reasoning_effort" not in captures[-1]
    assert captures[0]["messages"][0] == captures[1]["messages"][0]
    assert "SKILL_SENTINEL" not in captures[0]["messages"][0]["content"]
    assert captures[2]["messages"][0]["content"] == captures[0]["messages"][0]["content"] + "\n\nSKILL_SENTINEL"
