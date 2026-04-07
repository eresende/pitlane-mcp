# Semantic Search (Experimental)

Semantic search lets you find symbols by meaning rather than by name. Instead of matching keywords, pitlane-mcp generates embedding vectors for every indexed symbol and ranks results by cosine similarity to your query.

> **Experimental:** This feature requires a locally-running embedding server and adds significant indexing time for large codebases. The API and behaviour may change in future releases.

## How It Works

When `PITLANE_EMBED_URL` and `PITLANE_EMBED_MODEL` are set, `index_project` generates an embedding vector for every symbol and stores them in `embeddings.bin` alongside the regular index. Subsequent `search_symbols` calls with `"mode": "semantic"` embed the query and return symbols ranked by cosine similarity.

When the env vars are absent, pitlane-mcp behaves exactly as before — zero overhead, zero network calls.

## Prerequisites

Install [Ollama](https://ollama.com) and pull an embedding model:

```bash
ollama pull nomic-embed-text
```

## Quick Start

```bash
export PITLANE_EMBED_URL=http://localhost:11434/api/embed
export PITLANE_EMBED_MODEL=nomic-embed-text

# Index a project (generates embeddings.bin alongside index.bin)
pitlane index /your/project

# Semantic search via CLI
pitlane search /your/project "error handling when file cannot be read" --mode semantic

# Semantic search via MCP tool
# { "project": "/your/project", "query": "error handling when file cannot be read", "mode": "semantic" }
```

## Environment Variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `PITLANE_EMBED_URL` | yes | — | Full URL of the embedding endpoint (e.g. `http://localhost:11434/api/embed`) |
| `PITLANE_EMBED_MODEL` | yes | — | Model identifier to pass in requests (e.g. `nomic-embed-text`) |
| `PITLANE_EMBED_BATCH_SIZE` | no | `256` | Number of symbols per HTTP request. Reduce for large/slow models. |
| `PITLANE_EMBED_TIMEOUT` | no | `120` | Per-request timeout in seconds. Increase for large models. |

Both `PITLANE_EMBED_URL` and `PITLANE_EMBED_MODEL` must be set to non-empty strings for embeddings to be enabled. Either absent or empty disables the feature entirely.

## Model Recommendations

| Model | Size | VRAM | Quality | Indexing speed (ripgrep, 3k symbols) |
|---|---|---|---|---|
| `nomic-embed-text` | 274 MB | ~2 GB | good | ~17s |
| `mxbai-embed-large` | 670 MB | ~3 GB | better | ~45s |
| `qwen3-embedding:0.6b` | ~600 MB | ~3 GB | better | ~60s |
| `qwen3-embedding` | ~4 GB | ~7 GB | best | ~3m |

**Recommended default:** `nomic-embed-text` — fast, low VRAM, good quality for code search.

**For higher quality:** `mxbai-embed-large` is a strong middle ground. It scores well on retrieval benchmarks (MTEB) and uses modest VRAM.

**For best quality:** `qwen3-embedding` (the full 4B model) produces the best vectors, but requires ~7 GB VRAM and indexing large codebases takes significantly longer. It will saturate an 8 GB GPU, leaving little headroom for other processes. On 16 GB+ cards it runs comfortably alongside other workloads. On Apple M-series (unified memory), the same VRAM budget applies but is shared with system RAM — an M2/M3 with 16 GB or more is a good fit.

**Codebase size guidance:** For larger codebases, smaller models like `nomic-embed-text` are recommended — they index faster and use less memory, which matters more at scale. For small codebases, `qwen3-embedding:4b` can provide noticeably better quality results since the indexing cost is low and the richer embeddings have more impact on search precision.

```bash
ollama pull mxbai-embed-large
export PITLANE_EMBED_MODEL=mxbai-embed-large

# For qwen3 (large model — tune batch size and timeout)
ollama pull qwen3-embedding
export PITLANE_EMBED_MODEL=qwen3-embedding
export PITLANE_EMBED_BATCH_SIZE=32
export PITLANE_EMBED_TIMEOUT=300
```

## Supported Endpoint Formats

pitlane-mcp automatically detects the response format:

- **OpenAI-compatible** (`data[i].embedding`) — used by LM Studio and the Ollama `/v1/embeddings` endpoint
- **Ollama `/api/embed`** (`embeddings[i]`) — current Ollama API, supports batch input
- **Ollama `/api/embeddings` legacy** (`embedding`) — older single-input Ollama endpoint

Use `/api/embed` with Ollama for best performance (native batch support).

## Incremental Embedding

Embeddings are stored incrementally. Re-running `index_project` without `--force` skips symbols already in the store — only new or changed symbols are re-embedded. This makes subsequent indexing runs fast even on large codebases.

Use `--force` to rebuild embeddings from scratch (e.g. after switching models):

```bash
pitlane index /your/project --force
```

## Index Storage

Embedding vectors are stored at:

```
~/.pitlane/indexes/{project_hash}/embeddings.bin
```

This file uses the same bincode format as `index.bin`. It is safe to delete — pitlane-mcp will regenerate it on the next `index_project` call with embeddings enabled.

## Indexing Time

Indexing time scales linearly with symbol count and depends heavily on the embedding model and hardware:

| Codebase | Symbols | nomic-embed-text | mxbai-embed-large |
|---|---|---|---|
| ripgrep | 3,207 | ~17s | ~45s |
| gin | 1,184 | ~6s | ~15s |
| guava | 56,805 | ~5m | ~15m |

Indexing is a one-time cost per project. Subsequent runs with unchanged files complete in milliseconds.

To improve throughput on large codebases, set `OLLAMA_NUM_PARALLEL` in your Ollama systemd service override:

```ini
# /etc/systemd/system/ollama.service.d/override.conf
[Service]
Environment="OLLAMA_NUM_PARALLEL=4"
Environment="OLLAMA_MAX_QUEUE=512"
```

## Known Limitations

- **Indexing speed** — embedding generation is bottlenecked by the Ollama HTTP API, not by GPU compute. Each request has fixed overhead regardless of batch size. This is a fundamental limitation of the HTTP interface.
- **Model switching** — switching models invalidates all stored embeddings. Re-index with `--force` after changing `PITLANE_EMBED_MODEL`.
- **Dimension mismatch** — if the embedding model changes dimension (e.g. switching from nomic to qwen3), semantic search returns an error until you re-index with `--force`.
- **Connection errors** — Ollama may drop connections under heavy load. Affected symbols are skipped and reported in `embeddings_skipped`. Re-run without `--force` to fill in the gaps.
- **Large codebases** — the Linux kernel (~311k symbols) takes ~26 minutes with nomic-embed-text. Consider filtering to specific subdirectories for very large repos.

## LLM Usage Guide

This section provides guidance for LLM agents on how to get the most out of semantic search.

### When to Use Semantic Search

Prefer `"mode": "semantic"` over `"mode": "bm25"` or `"mode": "exact"` when:

- You know **what a symbol does** but not what it's called — e.g. "function that retries a failed HTTP request"
- You're exploring an unfamiliar codebase and want to find conceptually related code
- Keyword searches return too many unrelated results or nothing at all
- You're looking for **patterns** rather than specific names — e.g. "error recovery logic", "cache invalidation", "rate limiting"

Stick with `"mode": "bm25"` or `"mode": "exact"` when:

- You know the exact symbol name or a distinctive substring
- You're looking for a specific type, constant, or import
- Semantic search is not available (env vars not set)

### Writing Effective Queries

Semantic search ranks results by meaning, so query phrasing matters more than with keyword search.

**Do:**
- Describe the behaviour or intent: `"parse JWT token and extract claims"`
- Use domain language: `"connection pool exhaustion handling"`
- Be specific about the context: `"middleware that validates request authentication headers"`
- Combine action + subject: `"serialize struct to JSON bytes"`

**Avoid:**
- Single-word queries: `"auth"`, `"parse"`, `"error"` — too broad, low signal
- File or module names: use `get_file_outline` for that instead
- Overly generic phrases: `"utility function"`, `"helper method"`

### Interpreting Results

Semantic search returns results ranked by cosine similarity. A few things to keep in mind:

- The top result is not always the right one — scan the top 3–5 results before concluding
- If the top results look unrelated, rephrase the query with more context or switch to `"mode": "bm25"`
- Use `get_symbol` on promising results to read the full implementation before deciding
- A similarity score near 1.0 is a strong match; scores below ~0.5 are likely noise

### Recommended Workflow

```
1. search_symbols(query="<intent-based description>", mode="semantic")
   → scan top 5 results

2. get_symbol(symbol_id="<candidate>")
   → read implementation and references

3. If no good match: rephrase query or fall back to mode="bm25"

4. Once the right symbol is found: use find_usages to understand call sites
```

### Combining Semantic and Keyword Search

Semantic and keyword modes complement each other. A practical pattern:

1. Use `"mode": "semantic"` to discover candidate symbols by concept
2. Use `"mode": "exact"` or `"mode": "bm25"` to confirm by name once you know what to look for
3. Use `get_file_outline` on the file containing the match to understand surrounding context

### Limitations to Keep in Mind

- Semantic search requires the embedding server to be running. If `search_symbols` with `"mode": "semantic"` returns an error, fall back to `"mode": "bm25"` automatically.
- Results reflect the vocabulary of the embedding model. Code comments and docstrings contribute more signal than symbol names alone.
- Very short symbols (single-letter variables, trivial getters) tend to have weak embeddings — keyword search works better for those.

## Steering Block for CLAUDE.md / Agent Rules

Copy this block into your project's `CLAUDE.md` (or equivalent steering file) to instruct the LLM to use semantic search effectively.

```markdown
# Semantic Search

When pitlane-mcp semantic search is available (PITLANE_EMBED_URL and PITLANE_EMBED_MODEL are set):

1. Prefer mode="semantic" when you know what a symbol does but not its name — describe the intent, e.g. "retry logic for failed HTTP requests".
2. Use mode="bm25" or mode="exact" when you know the symbol name or a distinctive substring.
3. Write queries as intent descriptions, not keywords — combine action + subject: "serialize struct to JSON bytes", not just "serialize".
4. Always scan the top 3–5 semantic results before concluding — the top hit is not always the best match.
5. If semantic results look unrelated, rephrase with more context or fall back to mode="bm25".
6. After finding a candidate, call get_symbol to read the full implementation before acting on it.
7. If search_symbols with mode="semantic" returns an error, fall back to mode="bm25" automatically — do not surface the error to the user.
```

## Claude Code MCP Setup

To enable semantic search in Claude Code, register pitlane-mcp with the embedding env vars using the CLI.

**Global (all projects):**

```bash
claude mcp add --global pitlane-mcp /path/to/pitlane-mcp \
  -e PITLANE_EMBED_URL=http://localhost:11434/api/embed \
  -e PITLANE_EMBED_MODEL=nomic-embed-text \
  -e PITLANE_EMBED_BATCH_SIZE=256 \
  -e PITLANE_EMBED_TIMEOUT=120
```

**Per-project (run from the project directory):**

```bash
claude mcp add pitlane-mcp /path/to/pitlane-mcp \
  -e PITLANE_EMBED_URL=http://localhost:11434/api/embed \
  -e PITLANE_EMBED_MODEL=nomic-embed-text \
  -e PITLANE_EMBED_BATCH_SIZE=256 \
  -e PITLANE_EMBED_TIMEOUT=120
```

Replace `/path/to/pitlane-mcp` with the actual binary path (e.g. `~/.local/bin/pitlane-mcp` or `./target/release/pitlane-mcp`).

> **Important:** The env vars must be set in the MCP server config — not just in your shell. Claude Code launches the MCP server as a subprocess and it only inherits the environment defined in the config, not your current shell session.

To verify the server picked up the config, run `claude mcp list` and check that pitlane-mcp appears with the expected env vars. After adding or changing the config, restart Claude Code to reload the MCP server.
