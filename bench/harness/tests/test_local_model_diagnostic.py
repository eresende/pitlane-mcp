"""Cross-model diagnostics cannot silently switch quantization or alter evidence."""
import copy
import struct

import pytest

from bench.harness.experiments.compare_local_model import loaded_model, replay_for_model, gguf_metadata


def inventory():
    return ({"models": [{"quantization": {"name": "Q4_K_M"}, "capabilities": {"trained_for_tool_use": True},
                          "loaded_instances": [{"id": "exact-instance", "config": {"context_length": 131072}}]}]},
            {"data": [{"id": "exact-instance", "state": "loaded", "quantization": "Q4_K_M", "loaded_context_length": 131072}]})


@pytest.mark.parametrize("fault", ["not_loaded", "wrong_quant", "short_context"])
def test_loaded_only_guard_rejects_unsafe_substitution(fault):
    native, legacy = inventory()
    assert loaded_model(native, legacy, "exact-instance")["instance"]["id"] == "exact-instance"
    if fault == "not_loaded": native["models"][0]["loaded_instances"] = []
    if fault == "wrong_quant": native["models"][0]["quantization"]["name"] = "Q3_K_S"
    if fault == "short_context": legacy["data"][0]["loaded_context_length"] = 8192
    with pytest.raises(ValueError): loaded_model(native, legacy, "exact-instance")


def test_replay_changes_only_model_and_preserves_reference():
    original = {"model": "reference", "messages": [{"role": "user", "content": "unaltered source and guidance"}],
                "temperature": 0, "max_tokens": 4096, "stream": False}
    before = copy.deepcopy(original)
    actual = replay_for_model(original, "exact-instance")
    assert original == before
    assert actual == {**original, "model": "exact-instance"}
    with pytest.raises(ValueError): replay_for_model({**original, "tools": [{"type": "function"}]}, "exact-instance")
    with pytest.raises(ValueError): replay_for_model({**original, "reasoning_effort": "xhigh"}, "exact-instance")


def test_gguf_inspection_skips_arrays_and_tensor_data(tmp_path):
    def string(text):
        raw = text.encode()
        return struct.pack("<Q", len(raw)) + raw
    data = b"GGUF" + struct.pack("<IQQ", 3, 999, 3)
    data += string("tokenizer.tokens") + struct.pack("<IIQ", 9, 8, 2) + string("not retained") + string("also omitted")
    data += string("general.name") + struct.pack("<I", 8) + string("local model")
    data += string("tokenizer.chat_template") + struct.pack("<I", 8) + string("existing template")
    path = tmp_path / "model.gguf"
    path.write_bytes(data)  # No tensor payload needed for metadata inspection.
    result = gguf_metadata(path)
    assert result["tensor_count"] == 999
    assert result["metadata"] == {"general.name": "local model", "tokenizer.chat_template": "existing template"}
