"""Model backend implementations for the benchmark framework.

Provides a ModelBackend Protocol and concrete implementations for Ollama
and OpenRouter.  Uses only stdlib (urllib.request) for HTTP — no requests
dependency.
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
import uuid
from typing import Any, Protocol

from bench.harness.framework.models import (
    ChatResponse,
    Message,
    ModelMetadata,
    TokenUsage,
    ToolCall,
    ToolDef,
)


# ---------------------------------------------------------------------------
# ModelBackend Protocol
# ---------------------------------------------------------------------------


class ModelBackend(Protocol):
    """Common interface for LLM providers."""

    def chat(self, messages: list[Message], tools: list[ToolDef]) -> ChatResponse:
        """Send messages + tool definitions, return assistant response."""
        ...

    def metadata(self) -> ModelMetadata:
        """Return model name, provider, parameter count, context window."""
        ...


# ---------------------------------------------------------------------------
# OllamaBackend
# ---------------------------------------------------------------------------


class OllamaBackend:
    """Backend that talks to a local Ollama instance via /api/chat."""

    def __init__(
        self,
        model: str,
        *,
        base_url: str = "http://localhost:11434",
        temperature: float = 0.0,
        num_ctx: int = 8192,
        timeout: float = 120.0,
    ) -> None:
        self._model = model
        self._base_url = base_url.rstrip("/")
        self._temperature = temperature
        self._num_ctx = num_ctx
        self._timeout = timeout

        # Validate model availability on init
        self._validate_model()

    # -- public API ----------------------------------------------------------

    def chat(self, messages: list[Message], tools: list[ToolDef]) -> ChatResponse:
        """Send a chat request to Ollama and return a parsed ChatResponse."""
        payload: dict[str, Any] = {
            "model": self._model,
            "messages": [self._serialize_message(m) for m in messages],
            "stream": False,
            "options": {
                "temperature": self._temperature,
                "num_ctx": self._num_ctx,
            },
        }
        if tools:
            payload["tools"] = [self._serialize_tool(t) for t in tools]

        data = self._post("/api/chat", payload)
        return self.parse_response(data)

    def metadata(self) -> ModelMetadata:
        """Return metadata about the configured model."""
        return ModelMetadata(
            name=self._model,
            provider="ollama",
            parameter_count=None,
            context_window=self._num_ctx,
        )

    # -- static parser (testable without network) ----------------------------

    @staticmethod
    def parse_response(data: dict) -> ChatResponse:
        """Parse a raw Ollama /api/chat JSON response into a ChatResponse.

        This is a static method so it can be tested independently of any
        network calls.
        """
        msg_data = data.get("message", {})
        content = msg_data.get("content", "")
        role = msg_data.get("role", "assistant")

        # Parse tool calls — Ollama doesn't provide explicit IDs, so we
        # generate a UUID for each one.
        raw_tool_calls = msg_data.get("tool_calls") or []
        tool_calls: list[ToolCall] = []
        for tc in raw_tool_calls:
            func = tc.get("function", {})
            tool_calls.append(
                ToolCall(
                    id=str(uuid.uuid4()),
                    name=func.get("name", ""),
                    arguments=func.get("arguments", {}),
                )
            )

        message = Message(
            role=role,
            content=content,
            tool_calls=tool_calls if tool_calls else None,
        )

        # Token usage from Ollama-specific fields
        usage = TokenUsage(
            prompt_tokens=data.get("prompt_eval_count", 0),
            completion_tokens=data.get("eval_count", 0),
            total_tokens=data.get("prompt_eval_count", 0)
            + data.get("eval_count", 0),
        )

        return ChatResponse(message=message, usage=usage)

    # -- private helpers -----------------------------------------------------

    def _validate_model(self) -> None:
        """Check that the model is available locally via GET /api/tags."""
        try:
            req = urllib.request.Request(
                f"{self._base_url}/api/tags",
                method="GET",
            )
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                body = json.loads(resp.read().decode())
        except (urllib.error.URLError, OSError) as exc:
            raise ConnectionError(
                f"Cannot connect to Ollama at {self._base_url}. Is it running?"
            ) from exc

        available = [m.get("name", "") for m in body.get("models", [])]
        # Ollama tags may include `:latest` suffix — match with or without
        matches = any(
            name == self._model or name.startswith(f"{self._model}:")
            for name in available
        )
        if not matches:
            raise ValueError(
                f"Model {self._model!r} not found. "
                f"Run `ollama pull {self._model}` first. "
                f"Available: {', '.join(available)}"
            )

    def _post(self, path: str, payload: dict) -> dict:
        """POST JSON to Ollama and return the parsed response body."""
        url = f"{self._base_url}{path}"
        body = json.dumps(payload).encode()
        req = urllib.request.Request(
            url,
            data=body,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                return json.loads(resp.read().decode())
        except urllib.error.HTTPError as exc:
            error_body = exc.read().decode() if exc.fp else ""
            if exc.code == 400 and "does not support tools" in error_body.lower():
                raise ValueError(
                    f"Model {self._model!r} does not support tool calling."
                ) from exc
            raise
        except (urllib.error.URLError, OSError) as exc:
            raise ConnectionError(
                f"Cannot connect to Ollama at {self._base_url}. Is it running?"
            ) from exc

    @staticmethod
    def _serialize_message(msg: Message) -> dict:
        """Convert a Message dataclass to the Ollama wire format."""
        out: dict[str, Any] = {"role": msg.role, "content": msg.content}
        if msg.tool_calls:
            out["tool_calls"] = [
                {
                    "function": {
                        "name": tc.name,
                        "arguments": tc.arguments,
                    }
                }
                for tc in msg.tool_calls
            ]
        if msg.tool_call_id is not None:
            out["tool_call_id"] = msg.tool_call_id
        return out

    @staticmethod
    def _serialize_tool(tool: ToolDef) -> dict:
        """Convert a ToolDef to the Ollama tool format."""
        return {
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            },
        }


# ---------------------------------------------------------------------------
# OpenRouterBackend
# ---------------------------------------------------------------------------


class OpenRouterBackend:
    """Backend that talks to OpenRouter's OpenAI-compatible API.

    Reads the API key from the ``OPENROUTER_API_KEY`` environment variable.
    """

    API_URL = "https://openrouter.ai/api/v1/chat/completions"

    def __init__(
        self,
        model: str,
        *,
        temperature: float = 0.0,
        max_tokens: int = 4096,
        timeout: float = 120.0,
    ) -> None:
        self._model = model
        self._temperature = temperature
        self._max_tokens = max_tokens
        self._timeout = timeout

        self._api_key = os.environ.get("OPENROUTER_API_KEY", "")
        if not self._api_key:
            raise ValueError(
                "OPENROUTER_API_KEY environment variable is not set. "
                "Get a key at https://openrouter.ai/keys"
            )

    # -- public API ----------------------------------------------------------

    def chat(self, messages: list[Message], tools: list[ToolDef]) -> ChatResponse:
        """Send a chat request to OpenRouter and return a parsed ChatResponse."""
        payload: dict[str, Any] = {
            "model": self._model,
            "messages": [self._serialize_message(m) for m in messages],
            "temperature": self._temperature,
            "max_tokens": self._max_tokens,
        }
        if tools:
            payload["tools"] = [self._serialize_tool(t) for t in tools]

        data = self._post(payload)
        return self._parse_response(data)

    def metadata(self) -> ModelMetadata:
        """Return metadata about the configured model."""
        return ModelMetadata(
            name=self._model,
            provider="openrouter",
            parameter_count=None,
            context_window=self._max_tokens,
        )

    # -- private helpers -----------------------------------------------------

    def _post(self, payload: dict) -> dict:
        """POST JSON to OpenRouter and return the parsed response body."""
        body = json.dumps(payload).encode()
        req = urllib.request.Request(
            self.API_URL,
            data=body,
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {self._api_key}",
                "HTTP-Referer": "https://github.com/eresende/pitlane-mcp",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                return json.loads(resp.read().decode())
        except urllib.error.HTTPError as exc:
            error_body = exc.read().decode() if exc.fp else ""
            raise RuntimeError(
                f"OpenRouter API error ({exc.code}): {error_body}"
            ) from exc
        except (urllib.error.URLError, OSError) as exc:
            raise ConnectionError(
                "Cannot connect to OpenRouter API."
            ) from exc

    @staticmethod
    def _parse_response(data: dict) -> ChatResponse:
        """Parse an OpenAI-compatible response into a ChatResponse."""
        choices = data.get("choices", [])
        if not choices:
            return ChatResponse(
                message=Message(role="assistant", content=""),
                usage=TokenUsage(prompt_tokens=0, completion_tokens=0, total_tokens=0),
            )

        choice = choices[0]
        msg_data = choice.get("message", {})
        content = msg_data.get("content") or ""

        # Parse tool calls
        raw_tool_calls = msg_data.get("tool_calls") or []
        tool_calls: list[ToolCall] = []
        for tc in raw_tool_calls:
            func = tc.get("function", {})
            arguments = func.get("arguments", "{}")
            if isinstance(arguments, str):
                try:
                    arguments = json.loads(arguments)
                except json.JSONDecodeError:
                    arguments = {}
            tool_calls.append(
                ToolCall(
                    id=tc.get("id", str(uuid.uuid4())),
                    name=func.get("name", ""),
                    arguments=arguments,
                )
            )

        message = Message(
            role=msg_data.get("role", "assistant"),
            content=content,
            tool_calls=tool_calls if tool_calls else None,
        )

        usage_data = data.get("usage", {})
        usage = TokenUsage(
            prompt_tokens=usage_data.get("prompt_tokens", 0),
            completion_tokens=usage_data.get("completion_tokens", 0),
            total_tokens=usage_data.get("total_tokens", 0),
        )

        return ChatResponse(message=message, usage=usage)

    @staticmethod
    def _serialize_message(msg: Message) -> dict:
        """Convert a Message to the OpenAI wire format."""
        out: dict[str, Any] = {"role": msg.role, "content": msg.content}
        if msg.tool_calls:
            out["tool_calls"] = [
                {
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": json.dumps(tc.arguments),
                    },
                }
                for tc in msg.tool_calls
            ]
        if msg.tool_call_id is not None:
            out["tool_call_id"] = msg.tool_call_id
        return out

    @staticmethod
    def _serialize_tool(tool: ToolDef) -> dict:
        """Convert a ToolDef to the OpenAI tool format."""
        return {
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            },
        }
