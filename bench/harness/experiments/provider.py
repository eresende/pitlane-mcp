"""Strict, no-retry OpenAI-compatible adapter for controlled experiments.

Legacy adapters deliberately remain unchanged. Usage here is authoritative only
when supplied by the provider; the loop's legacy numeric counters are not exported.
"""
from __future__ import annotations

import json
import os
import time
from pathlib import Path
import urllib.error
import urllib.request

from bench.harness.framework.backends import (
    _serialize_openai_compatible_message,
    _serialize_openai_compatible_tool,
)
from bench.harness.framework.models import ChatResponse, Message, TokenUsage, ToolCall, ToolDef


class ProviderError(RuntimeError):
    pass


def read_key(env_name: str, env_file: str | None = None) -> str:
    """Read one literal dotenv value, without executing shell or logging secrets."""
    if env_file:
        for line in Path(env_file).expanduser().read_text().splitlines():
            key, sep, value = line.strip().removeprefix("export ").partition("=")
            if sep and key.strip() == env_name:
                return value.strip().strip("\"'")
        raise ValueError(f"Credential variable {env_name} absent from supplied file")
    return os.environ.get(env_name, "")


def number(value):
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def usage_summary(usages: list[dict]) -> dict:
    """Sum over ALL requests; one missing field makes its cumulative sum unknown."""
    fields = ("prompt_tokens", "completion_tokens", "total_tokens", "reasoning_tokens", "cached_tokens")
    return {key: sum(row[key] for row in usages)
            if usages and all(row.get(key) is not None for row in usages) else None
            for key in fields}


class CompatibleProvider:
    def __init__(self, base_url: str, model: str, api_key: str, *, temperature=0.0,
                 max_tokens=4096, request_options=None, timeout=120.0):
        self.base_url = base_url.rstrip("/")
        self.model = model
        self._key = api_key
        self.temperature = temperature
        self.max_tokens = max_tokens
        self.request_options = request_options or {}
        if set(self.request_options) & {"model", "messages", "tools", "stream", "temperature", "max_tokens"}:
            raise ValueError("Request options cannot override controlled request fields")
        self.timeout = timeout
        self.usages: list[dict] = []
        self.response_metadata: list[dict] = []
        self.protocol_errors = 0

    def request(self, route, payload=None):
        headers = {"Content-Type": "application/json"}
        if self._key:
            headers["Authorization"] = "Bearer " + self._key
        req = urllib.request.Request(self.base_url + route, headers=headers,
                                     data=None if payload is None else json.dumps(payload).encode())
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as response:
                return json.load(response)
        except urllib.error.HTTPError as exc:
            # Never propagate provider bodies, headers or credentials into artifacts.
            raise ProviderError(f"provider_http_{exc.code}") from None
        except (urllib.error.URLError, OSError, ValueError):
            raise ProviderError("provider_transport_or_json_error") from None

    def chat(self, messages, tools):
        payload = dict(self.request_options, model=self.model,
                       messages=[_serialize_openai_compatible_message(m) for m in messages],
                       temperature=self.temperature, max_tokens=self.max_tokens, stream=False)
        if tools:
            payload["tools"] = [_serialize_openai_compatible_tool(t) for t in tools]
        try:
            data = self.request("/chat/completions", payload)
        except BaseException:
            # A failed/in-flight request may have consumed tokens without usage.
            # Known prior usage must not masquerade as a complete session total.
            self.usages.append({"request_failed": True})
            raise
        return self.parse(data)

    def parse(self, data):
        # Some OpenAI-compatible gateways wrap the payload as {"data": {...}, "success": true}.
        if (isinstance(data, dict) and "choices" not in data
                and isinstance(data.get("data"), dict) and "choices" in data["data"]):
            data = data["data"]
        raw = data.get("usage") if isinstance(data, dict) else None
        raw = raw if isinstance(raw, dict) else {}
        details = raw.get("completion_tokens_details") or {}
        details = details if isinstance(details, dict) else {}
        prompt_details = raw.get("prompt_tokens_details") or {}
        prompt_details = prompt_details if isinstance(prompt_details, dict) else {}
        self.usages.append({**{k: number(raw.get(k)) for k in
                               ("prompt_tokens", "completion_tokens", "total_tokens")},
                            "reasoning_tokens": number(details.get("reasoning_tokens")),
                            "cached_tokens": number(prompt_details.get("cached_tokens"))})
        try:
            if not isinstance(data, dict):
                raise ValueError()
            choice = data["choices"][0]
            msg = choice["message"]
            if not isinstance(msg, dict) or not isinstance(msg.get("tool_calls") or [], list):
                raise ValueError()
            self.response_metadata.append({
                "finish_reason": choice.get("finish_reason"),
                "message_keys": sorted(msg),
                "assistant_content_bytes": len((msg.get("content") or "").encode()) if isinstance(msg.get("content"), str) else None,
                "raw_tool_call_count": len(msg.get("tool_calls") or []),
                "reasoning_content_bytes": len((msg.get("reasoning_content") or "").encode()) if isinstance(msg.get("reasoning_content"), str) else None,
            })
            content = msg.get("content") or ""
            if not isinstance(content, str):
                raise ValueError()
            calls = []
            for raw_call in msg.get("tool_calls") or []:
                if not isinstance(raw_call, dict) or raw_call.get("type") != "function" or not isinstance(raw_call.get("id"), str) or not raw_call["id"]:
                    raise ValueError()
                function = raw_call["function"]
                if not isinstance(function, dict) or not isinstance(function.get("name"), str) or not function["name"]:
                    raise ValueError()
                arguments = function.get("arguments")
                if isinstance(arguments, str):
                    try:
                        arguments = json.loads(arguments)
                    except ValueError:
                        pass  # Executor counts and rejects malformed JSON; never silently repairs it.
                calls.append(ToolCall(raw_call["id"], function["name"], arguments))
            if len({c.id for c in calls}) != len(calls):
                raise ValueError()
            if choice.get("finish_reason") == "length":
                raise ProviderError("output_truncated")
            if choice.get("finish_reason") not in (("stop", "tool_calls") if calls else ("stop",)):
                raise ProviderError("invalid_finish_reason")
            if not calls and (not content.strip() or "<tool_call>" in content):
                raise ProviderError("empty_or_unstructured_tool_response")
        except (KeyError, IndexError, TypeError, ValueError):
            self.protocol_errors += 1
            raise ProviderError("invalid_provider_tool_protocol") from None
        except ProviderError as exc:
            if str(exc) != "output_truncated":
                self.protocol_errors += 1
            raise
        usage = self.usages[-1]
        return ChatResponse(Message("assistant", content, calls or None),
                            TokenUsage(*(usage[k] or 0 for k in
                                         ("prompt_tokens", "completion_tokens", "total_tokens"))),
                            finish_reason=choice.get("finish_reason"))

    def preflight(self):
        models = self.request("/models")
        available = next((m for m in models.get("data", []) if m.get("id") == self.model), None)
        if available is None:
            raise ProviderError("requested_model_not_available")
        nonce = "pitlane-probe-7381"
        tools = [ToolDef("benchmark_echo", "Return the supplied nonce unchanged.",
                         {"type": "object", "properties": {"nonce": {"type": "string"}}, "required": ["nonce"]})]
        messages = [Message("user", f"Call benchmark_echo with nonce {nonce}. After its result, reply with that nonce.")]
        first = self.chat(messages, tools).message
        if not first.tool_calls or len(first.tool_calls) != 1:
            raise ProviderError("preflight_missing_tool_call")
        call = first.tool_calls[0]
        if call.name != "benchmark_echo" or call.arguments != {"nonce": nonce}:
            raise ProviderError("preflight_wrong_tool_arguments")
        messages += [first, Message("tool", nonce, tool_call_id=call.id)]
        final = self.chat(messages, tools).message
        if final.tool_calls or nonce not in final.content:
            raise ProviderError("preflight_tool_result_not_consumed")
        self._probe_synthesis_evidence(nonce)
        fields = ("prompt_tokens", "completion_tokens", "total_tokens")
        if not all(all(type(u.get(k)) is int for k in fields) and
                   u["total_tokens"] == u["prompt_tokens"] + u["completion_tokens"] for u in self.usages):
            raise ProviderError("preflight_token_accounting_invalid")
        return {"passed": True, "model": available, "usage": self.usages,
                "scope": "One synthetic structured tool round trip plus one tool-free synthesis probe; reliability assessed separately in task runs."}

    def _probe_synthesis_evidence(self, nonce):
        """Verify a tool-free synthesis request still reaches the model with evidence.

        Some hosted gateways silently drop ``role: "tool"`` messages when a
        request declares no tools, so a final synthesis that reuses the raw
        history can answer from nothing. The experiment therefore transmits a
        flattened transcript. This probe proves that shape is delivered and
        counted, instead of discovering the problem after a full pilot.
        """
        from bench.harness.framework.agentic_loop import _synthesis_messages

        evidence = "repository evidence line about traversal and matching.\n" * 400
        conversation = [
            Message("system", "Probe system."),
            Message("user", "Probe task."),
            Message("assistant", None,
                    [ToolCall(id="probe-1", name="benchmark_echo", arguments={"nonce": nonce})]),
            Message("tool", f"{evidence}ACCESS {nonce}", tool_call_id="probe-1"),
        ]
        messages = _synthesis_messages("Answer only from the collected evidence.", "Report the ACCESS value.",
                                       conversation, "Do not request tools.")
        reply = self.chat(messages, []).message
        if reply.tool_calls or nonce not in reply.content:
            raise ProviderError("preflight_synthesis_evidence_missing")
        if (self.usages[-1].get("prompt_tokens") or 0) < 500:
            raise ProviderError("preflight_synthesis_usage_implausible")


def _write_trace_json(path, value):
    # Traces contain source and reasoning even though authentication is excluded.
    with open(path, "w", opener=lambda name, flags: os.open(name, flags, 0o600)) as handle:
        handle.write(json.dumps(value, indent=2) + "\n")


class TracedProvider(CompatibleProvider):
    """Capture wire responses independently of parser/loop completion decisions."""
    def __init__(self, *args, trace_dir, **kwargs):
        super().__init__(*args, **kwargs)
        self.trace_dir = Path(trace_dir)
        self.trace_dir.mkdir(parents=True, mode=0o700)
        self.observations = []

    def request(self, route, payload=None):
        if route != "/chat/completions":
            return super().request(route, payload)
        index = len(self.observations) + 1
        _write_trace_json(self.trace_dir / f"request_{index:02}.json", payload)
        start = time.perf_counter()
        raw = super().request(route, payload)
        _write_trace_json(self.trace_dir / f"response_{index:02}.json", raw)
        choice = (raw.get("choices") or [{}])[0]
        msg = choice.get("message") or {}
        self.observations.append({
            "request": index, "finish_reason": choice.get("finish_reason"),
            "message_keys": sorted(msg), "assistant_content": msg.get("content"),
            "reasoning_content": msg.get("reasoning_content"),
            "tool_calls": msg.get("tool_calls"), "usage": raw.get("usage"),
            "request_message_roles": [m.get("role") for m in payload["messages"]],
            "tool_choice": payload.get("tool_choice", "omitted"),
            "elapsed_seconds": time.perf_counter() - start,
        })
        return raw
