"""Data models for the benchmark framework."""

from __future__ import annotations

from dataclasses import asdict, dataclass


@dataclass
class ToolCall:
    """A tool call requested by the model."""

    id: str
    name: str
    arguments: dict


@dataclass
class Message:
    """A single message in a conversation."""

    role: str  # "system" | "user" | "assistant" | "tool"
    content: str
    tool_calls: list[ToolCall] | None = None
    tool_call_id: str | None = None


@dataclass
class ToolDef:
    """Definition of a tool exposed to the model."""

    name: str
    description: str
    parameters: dict  # JSON Schema


@dataclass
class ToolResult:
    """Result from executing a tool call."""

    content: str
    byte_size: int
    latency_ms: float


@dataclass
class TokenUsage:
    """Token usage counts from a single LLM call."""

    prompt_tokens: int
    completion_tokens: int
    total_tokens: int


@dataclass
class ChatResponse:
    """Response from a model backend."""

    message: Message
    usage: TokenUsage


@dataclass
class ModelMetadata:
    """Metadata about a model for reports."""

    name: str
    provider: str
    parameter_count: str | None
    context_window: int


@dataclass
class ToolCallRecord:
    """Record of a single tool call during an agentic loop run."""

    iteration: int
    tool_name: str
    arguments: dict
    result_bytes: int
    latency_ms: float


@dataclass
class RunResult:
    """Complete result of a single benchmark run."""

    prompt_id: str
    mode: str  # "mcp" | "baseline"
    run_index: int
    status: str  # "completed" | "max_iterations" | "timeout" | "error"
    final_answer: str
    conversation: list[Message]
    tool_calls: list[ToolCallRecord]
    token_usage: TokenUsage
    total_context_bytes: int
    wall_clock_seconds: float
    error: str | None = None


@dataclass
class QualityRecord:
    """Quality evaluation of a single answer."""

    grounded_files_count: int
    grounded_symbols_count: int
    ungrounded_references_count: int
    is_negative_correct: bool | None
    quality_score: float  # 0.0 to 1.0


@dataclass
class PromptRow:
    """A single prompt from a JSONL prompt set."""

    id: str
    category: str
    prompt: str
    prompt_suffix: str | None = None
    claim: str | None = None


@dataclass
class ClaimSummary:
    """Summary of benchmark results for a single README claim."""

    claim: str
    prompts_tested: int
    avg_efficiency_ratio: float | None
    avg_quality_score: float
    avg_latency_delta_seconds: float | None
    verdict: str  # "validated" | "partially_supported" | "insufficient_data"


@dataclass
class BenchmarkConfig:
    """Full configuration for a benchmark run."""

    model_name: str
    model_provider: str
    backend_type: str  # "ollama" | "openrouter"
    repo_path: str
    repo_commit: str | None
    repo_clean: bool | None
    harness_commit: str | None
    harness_clean: bool | None
    pitlane_version: str | None
    ollama_version: str | None
    prompt_set_path: str
    prompt_set_sha256: str
    prompt_count: int
    runs_per_prompt: int
    max_iterations: int
    timeout_seconds: float
    temperature: float
    context_window: int
    gpu_name: str | None
    gpu_vram_gb: float | None
    cpu_model: str | None
    ram_gb: float | None
    timestamp: str

    def to_dict(self) -> dict:
        """Serialize to a plain dictionary for JSON output."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> BenchmarkConfig:
        """Deserialize from a plain dictionary."""
        return cls(**data)
