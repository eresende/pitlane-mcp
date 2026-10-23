"""Matched evidence and reasoning-verification controls for diagnostic comparisons."""
import copy
import hashlib

import pytest

from bench.harness.experiments.compare_synthesis import (
    docker_proof, experiment_payload, verification_passes, CAUSAL_GUIDANCE,
)
from bench.harness.framework.agentic_loop import FINAL_TURN_PROMPT


def test_factorial_changes_only_final_instruction_and_explicit_medium():
    recorded = {"model": "existing-model", "temperature": 0, "max_tokens": 4096, "stream": False,
                "messages": [{"role": "system", "content": "unchanged skill"},
                             {"role": "tool", "tool_call_id": "one", "content": "unchanged evidence"}],
                "tools": [{"type": "function"}]}
    before = copy.deepcopy(recorded)
    payloads = {label: experiment_payload(recorded, label) for label in "ABCD"}
    assert recorded == before
    for label, payload in payloads.items():
        assert payload["messages"][:-1] == recorded["messages"]
        assert payload["messages"][-1]["content"] == (CAUSAL_GUIDANCE if label in "BD" else FINAL_TURN_PROMPT)
        assert "tools" not in payload and "tool_choice" not in payload
        assert payload.get("reasoning_effort") == ("medium" if label in "CD" else None)
        assert {k: v for k, v in payload.items() if k not in {"messages", "reasoning_effort"}} == {
            k: v for k, v in recorded.items() if k not in {"messages", "tools"}}
    assert all(term not in CAUSAL_GUIDANCE for term in ("ripgrep", "gitignore", "prun", "add_child", "Walk"))


def test_docker_proof_degrades_to_not_verified_instead_of_raising():
    from types import SimpleNamespace
    assert docker_proof(SimpleNamespace(returncode=0, stdout=b'{"payloads": {}}')) == {"payloads": {}}
    assert docker_proof(SimpleNamespace(returncode=0, stdout=b"warning: banner\n")) == {
        "verification_error": "docker_cpu_verification_unparseable"}
    assert docker_proof(SimpleNamespace(returncode=1, stdout=b"")) == {
        "verification_error": "docker_cpu_verification_failed"}
    # An unverified proof can never satisfy the reasoning-control requirement.
    assert not verification_passes({"verification_error": "docker_cpu_verification_unparseable"},
                                   {"chat_template": "actual template"}, {})


@pytest.mark.parametrize("field,value", [("reasoning_effort", "xhigh"), ("reasoning", {"effort": "high"}), ("chat_template_kwargs", {})])
def test_implicit_default_cannot_already_contain_reasoning_override(field, value):
    with pytest.raises(ValueError, match="already overrides"):
        experiment_payload({field: value}, "A")


def test_accepted_parameter_or_different_counts_alone_cannot_verify_control():
    props = {"chat_template": "actual template"}
    hashes = {"serve/server.py": "hash", "serve/frontend.py": "other"}
    proof = {"source_hashes": hashes, "template_sha256": hashlib.sha256(props["chat_template"].encode()).hexdigest(),
             "config_reasoning_effort": None, "effort_position": "start", "payloads": {}}
    for label in "ABCD":
        proof["payloads"][label] = {"expected_prompt_tokens": 100, "encoded_ids_sha256": label,
            "template_kwargs": {} if label in "AB" else {"reasoning_effort": "medium"},
            "xhigh_instruction_present": label in "AB", "low_instruction_present": False}
    assert verification_passes(proof, props, hashes)
    changed = copy.deepcopy(proof)
    changed["payloads"]["C"]["xhigh_instruction_present"] = True
    assert not verification_passes(changed, props, hashes)
    changed = copy.deepcopy(proof)
    changed["source_hashes"] = {}
    assert not verification_passes(changed, props, hashes)
    assert not verification_passes(proof, {"chat_template": "different deployed template"}, hashes)
