# Benchmark Harness Reliability Roadmap

This document captures the planned refactor of `bench/harness` into a reliable,
reproducible benchmark harness.

The current harness is functional enough for ad hoc experiments, but not
reliable enough for benchmark claims. The main problem is not that it cannot run
benchmarks. The problem is that there are multiple partially overlapping
execution paths with inconsistent assumptions, mutable inputs, and weak
reproducibility guarantees.

## Goals

The refactor should produce one benchmark system with:

- one canonical Python entrypoint
- explicit run manifests
- resumable per-instance execution
- runtime adapters for local, OpenCode, and remote execution
- scoring decoupled from execution
- AWS as a launcher, not a benchmark implementation

## Current State

Today there are three incompatible execution paths:

- `bench/harness/bench_runner.py`
  - local runner for the current Ollama/OpenRouter benchmark path
- `bench/harness/bench_opencode.py`
  - separate OpenCode-specific benchmark runner
- `bench/harness/aws_bench.sh`
  - AWS launcher that also encodes benchmark semantics and provisioning logic

This means the harness behaves more like three separate products than one
benchmark system with multiple runtimes.

## Main Problems

### 1. Split execution paths

`bench_runner.py`, `bench_opencode.py`, and `aws_bench.sh` all define benchmark
behavior differently.

Consequences:

- incompatible output models
- duplicated logic
- drift between local and cloud behavior
- no single trustworthy benchmark command

### 2. Weak environment control

The local harness depends on mutable machine state:

- local Python environment
- local Ollama state
- local `pitlane-mcp` binary on `PATH`
- current repo checkout state

The AWS path is also too mutable:

- clones a moving branch
- downloads the latest release tarball
- installs mutable dependencies at runtime

That is acceptable for debugging, but not for benchmark-grade reproducibility.

### 3. Missing immutable run identity

The current runner records useful metadata, but there is no single immutable run
manifest that captures:

- exact harness commit
- exact pitlane version or source SHA
- exact model/backend configuration
- exact prompt set hash
- exact repo revision
- exact scorer version

Without that, result comparisons are too easy to invalidate.

### 4. Execution and scoring are coupled

The current runner executes, scores, writes outputs, and prints summaries in one
pass.

That makes it hard to:

- regrade old runs after improving the scorer
- fix token parsing without rerunning expensive executions
- debug one stage independently from the other

### 5. Poor resumability

Long benchmark runs should be resumable at instance granularity.

The right unit is:

- `(prompt_id, mode, run_index)`

Today the harness is much closer to "run the whole job and hope it finishes."

### 6. Attached-server mode is not benchmark-safe

The OpenCode smoke run showed that attached server mode can accidentally evaluate
against the wrong repo or stale session state.

Attached mode is acceptable for debugging, but should not be treated as
canonical benchmark execution.

## External Patterns Worth Copying

This roadmap is informed by evaluation systems like:

- SWE-bench
- OpenHands evaluation harness
- OpenHands benchmarks
- OpenAI Evals

Useful patterns shared across those systems:

- isolated execution environments
- explicit dataset and run manifests
- per-instance logs and outputs
- separate execution and grading stages
- resumable and selectively rerunnable workloads

The point is not to copy their infrastructure wholesale. The point is to adopt
the reliability properties they treat as first-class.

References:

- https://www.swebench.com/SWE-bench/reference/harness/
- https://docs.openhands.dev/openhands/usage/developers/evaluation-harness
- https://github.com/OpenHands/benchmarks
- https://github.com/openai/evals

## Target Architecture

The target design has three layers.

### 1. Dataset layer

Benchmark suites should be described by explicit manifests.

Each suite should define:

- suite id
- repo path or repo source
- repo revision if pinned
- prompt set path and hash
- scorer version
- default timeout
- default max iterations
- default runs
- tags like `symbol_grounding`, `trace`, `architecture`

### 2. Runtime layer

One runtime interface with pluggable execution environments:

- `local`
- `docker`
- `opencode`
- `remote`

The runtime is responsible for:

- preparing the workspace
- preparing tool/backend state
- launching one benchmark instance
- collecting raw outputs and machine-readable events

### 3. Evaluation layer

One orchestrator should:

- load the suite
- build a run manifest
- enumerate instances
- resume or skip completed instances
- execute each instance through a runtime adapter
- persist execution outputs
- invoke grading separately
- generate derived exports and summaries

## Proposed Repository Layout

Keep `bench/harness` as the root, but reorganize around these modules:

- `bench/harness/run.py`
  - canonical CLI entrypoint
- `bench/harness/launch_ec2.py`
  - cloud launcher wrapper
- `bench/harness/schemas.py`
  - versioned models for manifests and results
- `bench/harness/manifest.py`
  - run-manifest construction and validation
- `bench/harness/resume.py`
  - per-instance resume and skip logic
- `bench/harness/grade.py`
  - grading and regrading CLI
- `bench/harness/suites/`
  - benchmark suite manifests
- `bench/harness/runtimes/`
  - runtime adapters
- `bench/harness/scorers/`
  - scoring implementations
- `bench/harness/export/`
  - CSV, markdown, and report exports

## Canonical Entrypoint

The benchmark system should converge on one command:

```bash
python -m bench.harness.run \
  --suite bench/harness/suites/ripgrep-core-v1.json \
  --runtime local \
  --backend ollama \
  --model qwen3:8b \
  --mode both \
  --runs 1 \
  --max-iterations 1 \
  --out results/ripgrep-qwen3-1 \
  --resume
```

Compatibility wrappers can remain temporarily:

- `bench_runner.py` should delegate to `bench.harness.run`
- `bench_opencode.py` should eventually delegate to `bench.harness.run --runtime opencode`

## Schemas and Manifests

### Run manifest

Every benchmark run should start by writing `run_manifest.json`.

Minimum required fields:

- `schema_version`
- `run_id`
- `suite_id`
- `suite_manifest_path`
- `suite_manifest_sha256`
- `repo_path`
- `repo_commit`
- `repo_clean`
- `prompt_set_path`
- `prompt_set_sha256`
- `model_name`
- `backend_type`
- `runtime_type`
- `mode`
- `runs_per_prompt`
- `max_iterations`
- `timeout_seconds`
- `temperature`
- `context_window`
- `harness_commit`
- `harness_clean`
- `pitlane_version`
- `ollama_version`
- `host`
- `timestamp`
- `scorer_version`

### Instance identity

Each benchmark unit should be identified as:

- `instance_id = <prompt_id>__<mode>__r<run_index>`

### Output layout

Recommended filesystem layout:

```text
results/<run_id>/
  run_manifest.json
  instances/
    symbol_router_core__mcp__r1/
      instance_manifest.json
      execution.json
      score.json
      events.jsonl
      stdout.txt
      stderr.txt
      status.json
  results.jsonl
  summary.json
  summary.csv
  claim_report.md
```

CSV and markdown reports should be treated as derived outputs, not the canonical
source of truth.

## Runtime Adapter Interface

The runtime layer should expose a simple interface similar to:

```python
class RuntimeAdapter(Protocol):
    def prepare_run(self, run_manifest: RunManifest) -> None: ...
    def prepare_instance(self, instance: InstanceManifest) -> None: ...
    def execute_instance(self, instance: InstanceManifest) -> InstanceExecutionResult: ...
    def finalize_instance(self, instance: InstanceManifest) -> None: ...
    def finalize_run(self, run_manifest: RunManifest) -> None: ...
```

Planned adapters:

- `local_agentic`
  - current Ollama/OpenRouter + executors path
- `opencode`
  - current `bench_opencode.py` logic moved under the unified runner
- `remote_ssh`
  - remote execution on provisioned machines
- `docker`
  - preferred future canonical local runtime

## Scoring Model

Execution and scoring should be separate phases.

### Execution phase

Persist:

- final answer
- tool calls
- token usage
- context bytes
- status
- wall-clock duration
- error
- raw event log path

### Scoring phase

Persist independently:

- objective score fields
- subjective score fields
- scorer version
- contamination findings
- notes

This enables:

- regrading old runs without rerunning the model
- scorer iteration without execution drift
- parser fixes without execution drift

Recommended command:

```bash
python -m bench.harness.grade --run results/ripgrep-core-v1-qwen3-8b
```

## Suite Manifests

Add a `bench/harness/suites/` directory with suite manifests such as:

- `ripgrep-core-v1.json`
- `redis-core-v1.json`
- `gin-core-v1.json`
- `guava-core-v1.json`

Example:

```json
{
  "schema_version": "1",
  "suite_id": "ripgrep-core-v1",
  "repo": {
    "path": "bench/repos/ripgrep"
  },
  "prompts": {
    "path": "bench/harness/prompts.ripgrep.jsonl"
  },
  "defaults": {
    "runs": 3,
    "max_iterations": 25,
    "timeout_seconds": 300
  },
  "scorer": {
    "version": "v1"
  },
  "tags": ["symbol_grounding", "trace", "architecture"]
}
```

This is preferable to encoding suite semantics in shell commands and README
examples.

## AWS Strategy

The AWS path should stop owning benchmark semantics.

Target behavior:

- a launcher provisions a machine
- it uploads or reconstructs one pinned `run_manifest.json`
- it invokes the same Python runner as local mode
- it uploads intermediate and final artifacts to durable storage

### Rules for official cloud runs

- no moving branch names
- no "latest release" downloads
- no mutable benchmark inputs
- exact artifact versions only
- resumable instance execution

`aws_bench.sh` can remain temporarily, but only as a thin launcher.

## OpenCode Policy

Attached OpenCode server mode should be supported for debugging only.

It should be explicitly marked as:

- noncanonical
- lower reproducibility
- vulnerable to session contamination

Canonical OpenCode benchmarking should use fresh process or fresh configuration
per run or per instance.

## What Not To Build Yet

Do not build these before the reliability base exists:

- a distributed scheduler
- a web dashboard
- a human annotation UI
- complex significance tooling
- broad multi-provider abstraction beyond the currently needed runtimes
- attached-session OpenCode benchmarking as the official path

## Implementation Roadmap

### Patch Set 1: foundational reliability

Add:

- `bench/harness/run.py`
- `bench/harness/schemas.py`
- `bench/harness/manifest.py`
- `bench/harness/resume.py`
- `bench/harness/suites/ripgrep-core-v1.json`

Modify:

- `bench/harness/bench_runner.py`
- `bench/harness/framework/benchmark_runner.py`
- output/model helpers as needed

Scope:

- introduce `run_manifest.json`
- introduce per-instance output layout
- add `--resume`
- add `--force`
- add prompt/mode filtering
- keep current local Ollama path only

This is the minimum change set that makes long runs recoverable and results
traceable.

### Patch Set 2: split execution from scoring

Add:

- `bench/harness/grade.py`
- `bench/harness/scorers/base.py`
- `bench/harness/scorers/default.py`

Modify:

- orchestration and output-writing paths

Scope:

- execution writes raw artifacts
- grading writes score artifacts
- summaries are derived afterward

### Patch Set 3: runtime adapter extraction

Add:

- `bench/harness/runtimes/base.py`
- `bench/harness/runtimes/local_agentic.py`
- `bench/harness/runtimes/opencode.py`

Modify:

- move OpenCode logic out of `bench_opencode.py`
- refactor canonical runner to dispatch through adapters

Scope:

- unify local and OpenCode under one orchestration model

### Patch Set 4: AWS launcher cleanup

Add:

- `bench/harness/launch_ec2.py`
- optional remote bootstrap helper

Modify:

- `bench/harness/aws_bench.sh`

Scope:

- provision pinned environments
- run canonical Python runner remotely
- persist artifacts durably

### Patch Set 5: Docker runtime

Add:

- `bench/harness/runtimes/docker.py`
- container configuration

Scope:

- establish the future canonical local reproducible runtime

## Recommended Order

Implement in this order:

1. `run_manifest.json`
2. per-instance output layout
3. resume and force controls
4. execution/scoring split
5. runtime adapter extraction
6. AWS cleanup
7. Docker runtime

This order is deliberate. The base reliability layer is a dependency for every
other improvement.

## Success Criteria

After Patch Set 1 and 2, the harness should satisfy all of the following:

- rerunning with `--resume` skips completed instances
- interrupting a run loses no completed instance data
- every result can be traced back to one immutable run manifest
- scoring can be rerun independently
- summaries are derived from persisted artifacts

After Patch Set 3 and 4:

- local, OpenCode, and AWS all use the same manifest model
- official benchmark documentation centers on one canonical Python runner
- attached-server mode is clearly marked noncanonical
- remote runs are version-pinned and resumable

## Immediate Recommendation

Start with Patch Set 1 only.

Do not try to unify OpenCode and AWS in the first patch. The highest-ROI move is
to build the reliability base layer under the current local runner first:

- manifest
- per-instance layout
- resumability
- stable schema

Everything else should build on top of that.
