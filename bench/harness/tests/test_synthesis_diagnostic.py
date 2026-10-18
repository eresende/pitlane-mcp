"""Replay controls must not inject evidence or change inference configuration."""
import copy
import json

import pytest

from bench.harness.experiments.diagnose_synthesis import (
    replay_payload, checkpoint_payload, response_summary, SYNTHESIS_INSTRUCTION,
)
from bench.harness.experiments.provider import CompatibleProvider


def test_replay_preserves_context_and_settings_without_mutating_source():
    original = {"messages": [{"role": "system", "content": "skill context"},
                             {"role": "tool", "tool_call_id": "one", "content": "recorded evidence"}],
                "model": "existing-model", "temperature": 0.0, "max_tokens": 4096,
                "stream": False, "reasoning_effort": "existing-value", "tools": [{"type": "function"}], "tool_choice": "auto"}
    before = copy.deepcopy(original)
    replay = replay_payload(original)
    assert original == before
    assert replay["messages"][:-1] == original["messages"]
    assert replay["messages"][-1] == {"role": "user", "content": SYNTHESIS_INSTRUCTION}
    assert {k: v for k, v in replay.items() if k != "messages"} == {
        k: v for k, v in original.items() if k not in {"messages", "tools", "tool_choice"}}


def test_checkpoint_includes_all_results_of_selected_request(tmp_path):
    wire = tmp_path / "instances/symbol_ignore_logic/C/run_1/wire"
    wire.mkdir(parents=True)
    request = {"messages": [{"role": "tool", "content": "new evidence", "tool_call_id": "sixteen"}]}
    (wire / "request_17.json").write_text(json.dumps(request))
    (wire / "response_16.json").write_text(json.dumps({"choices": [{"message": {"tool_calls": [{"id": "sixteen"}]}}]}))
    path, actual = checkpoint_payload(tmp_path, "C", 16)
    assert path.name == "request_17.json" and actual == request
    request["messages"][-1]["tool_call_id"] = "different"
    (wire / "request_17.json").write_text(json.dumps(request))
    with pytest.raises(ValueError, match="do not match"):
        checkpoint_payload(tmp_path, "C", 16)


@pytest.mark.parametrize("finish,content,accepted", [("stop", "answer", True), ("length", "partial answer", False), ("length", "", False)])
def test_summary_distinguishes_truncation_and_never_estimates_reasoning(finish, content, accepted):
    backend = CompatibleProvider("unused", "model", "unused")
    raw = {"choices": [{"finish_reason": finish, "message": {"content": content, "reasoning_content": "private"}}],
           "usage": {"prompt_tokens": 100, "completion_tokens": 4096, "total_tokens": 4196}}
    row = response_summary(raw, backend, 1.2)
    assert row["accepted_terminal_response"] is accepted
    assert row["final_answer"] == (content if accepted else "")
    assert row["usage"] == {"prompt_tokens": 100, "completion_tokens": 4096, "total_tokens": 4196, "reasoning_tokens": None}
    assert row["correctness_score"] is None and row["complete_enough_for_task"] is None
    assert row["tool_calls_executed"] == 0
