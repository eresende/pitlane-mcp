# OpenCode benchmark harness

This starter setup is configured for **OpenAI authentication via `OPENAI_API_KEY`** and **`openai/gpt-5.4` with `reasoningEffort: medium`**.

That matches OpenCode's model/provider config format and GPT-5.4's supported reasoning-effort settings.

## Files

- `bench_opencode.py` — runner
- `prompts.guava.jsonl` — starter prompt set
- `sample.opencode.with-mcp.json` — OpenAI + GPT-5.4 medium + Pitlane MCP
- `sample.opencode.no-mcp.json` — OpenAI + GPT-5.4 medium without Pitlane

## 1) Authenticate to OpenAI

The sample configs read your API key from the environment:

```bash
export OPENAI_API_KEY="your_openai_api_key_here"
```

OpenCode also supports authenticating providers through `/connect`, but using an env var is easier for repeatable CLI benchmarks.

## 2) Start two backends

```bash
export OPENAI_API_KEY="your_openai_api_key_here"

OPENCODE_CONFIG=sample.opencode.with-mcp.json opencode serve --port 4096
OPENCODE_CONFIG=sample.opencode.no-mcp.json   opencode serve --port 4097
```

## 3) Run the benchmark

```bash
python bench_opencode.py \
    --repo /home/eresende/projects/forks/ripgrep \
    --prompts prompts.ripgrep.jsonl \
    --target mcp=http://localhost:4096 \
    --target no_mcp=http://localhost:4097 \
    --agent build \
    --model openai/gpt-5.4-nano \
    --prompt-suffix "Ground your answer in the repository. Name exact files and symbols you used, and say clearly when something is not found." \
    --out out_ripgrep
```

## 4) Review outputs

- `out_guava/results.csv`
- `out_guava/results.jsonl`
- `out_guava/scores_template.csv`
- `out_guava/raw/...`

## Notes

The sample configs pin:
- provider: `openai`
- model: `openai/gpt-5.4`
- model options:
  - `reasoningEffort: medium`
  - `textVerbosity: low`
  - `reasoningSummary: auto`

So both benchmark targets stay on the same OpenAI model configuration, with the only intended difference being Pitlane MCP.

## Optional: run without attached servers

```bash
export OPENAI_API_KEY="your_openai_api_key_here"

python bench_opencode.py   --repo /absolute/path/to/guava   --prompts prompts.guava.jsonl   --target mcp=CONFIG:/absolute/path/to/sample.opencode.with-mcp.json   --target no_mcp=CONFIG:/absolute/path/to/sample.opencode.no-mcp.json   --agent build   --model openai/gpt-5.4   --out out_cold_start
```
