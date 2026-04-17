"""Versioned schemas for benchmark suites and run manifests."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field


@dataclass
class SuiteRepo:
    """Repository source for a benchmark suite."""

    path: str


@dataclass
class SuitePrompts:
    """Prompt-set source for a benchmark suite."""

    path: str


@dataclass
class SuiteDefaults:
    """Default execution parameters for a suite."""

    runs: int = 3
    max_iterations: int = 25
    timeout_seconds: float = 300.0
    mode: str = "both"


@dataclass
class SuiteScorer:
    """Scorer metadata for a suite."""

    version: str = "v1"


@dataclass
class SuiteManifest:
    """Static description of a benchmark suite."""

    schema_version: str
    suite_id: str
    repo: SuiteRepo
    prompts: SuitePrompts
    defaults: SuiteDefaults = field(default_factory=SuiteDefaults)
    scorer: SuiteScorer = field(default_factory=SuiteScorer)
    tags: list[str] = field(default_factory=list)

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> "SuiteManifest":
        return cls(
            schema_version=str(data["schema_version"]),
            suite_id=str(data["suite_id"]),
            repo=SuiteRepo(**data["repo"]),
            prompts=SuitePrompts(**data["prompts"]),
            defaults=SuiteDefaults(**data.get("defaults", {})),
            scorer=SuiteScorer(**data.get("scorer", {})),
            tags=list(data.get("tags", [])),
        )


@dataclass
class RunManifest:
    """Immutable identity record for one benchmark invocation."""

    schema_version: str
    run_id: str
    suite_id: str
    suite_manifest_path: str | None
    suite_manifest_sha256: str | None
    repo_path: str
    repo_commit: str | None
    repo_clean: bool | None
    prompt_set_path: str
    prompt_set_sha256: str
    model_name: str
    backend_type: str
    runtime_type: str
    mode: str
    runs_per_prompt: int
    max_iterations: int
    timeout_seconds: float
    temperature: float
    context_window: int
    harness_commit: str | None
    harness_clean: bool | None
    pitlane_version: str | None
    ollama_version: str | None
    scorer_version: str
    prompt_filter: list[str] = field(default_factory=list)
    resume_enabled: bool = False
    force_enabled: bool = False
    created_at: str = ""

    def to_dict(self) -> dict:
        return asdict(self)

