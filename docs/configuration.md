# Configuration

Pitlane is configured through MCP or CLI arguments and the environment variables
listed below. This page is the canonical environment-variable reference; feature
pages link here instead of maintaining separate inventories.

Unless noted otherwise, an unset, empty, or invalid numeric value falls back to
the documented default. Restart the `pitlane-mcp` process after changing values
that are read at startup.

## Server and filesystem

| Variable | Default | Description |
|---|---:|---|
| `PITLANE_MCP_TOOL_TIER` | default tier | Set to `all` (case-insensitive) to expose advanced primitive tools through `tools/list`. Every other value selects the default public tier. Read at startup. |
| `PITLANE_ALLOWED_ROOTS` | unrestricted | Platform-native path list of roots the process may access. For example, use `:` between paths on Unix and `;` on Windows. Every configured root must exist and be canonicalizable. An unset or empty value disables this additional confinement. |
| `PITLANE_EXCLUDE_DIRS` | empty | Comma-separated directory basenames to add to Pitlane's built-in exclusions. Matching is case-insensitive at any depth. Values are trimmed; trailing `/` or `\` is accepted, but entries containing an internal path separator are ignored. This setting applies process-wide to indexing, watching, and direct file/content discovery. |

`PITLANE_EXCLUDE_DIRS` accepts names, not paths or glob patterns:

```sh
export PITLANE_EXCLUDE_DIRS="vendor,generated,tmp"
```

Tool-level `exclude` arguments remain available for project-specific glob
patterns. They are combined with the built-in and process-wide exclusions.

## Embedding endpoint and transport

Embeddings are enabled only when both `PITLANE_EMBED_URL` and
`PITLANE_EMBED_MODEL` are set to non-empty values.

| Variable | Default | Description |
|---|---:|---|
| `PITLANE_EMBED_URL` | disabled | Full URL of an OpenAI/Ollama-compatible embedding endpoint. Read at startup. |
| `PITLANE_EMBED_MODEL` | disabled | Model identifier sent to the endpoint. Read at startup. |
| `PITLANE_EMBED_API_KEY` | empty | Bearer token added as the `Authorization` header. It cannot be combined with an `Authorization` entry in `PITLANE_EMBED_HEADERS`. Read at startup. |
| `PITLANE_EMBED_HEADERS` | empty | JSON object containing additional string-valued HTTP headers. Invalid JSON, header names, or header values prevent embedding configuration from loading. Read at startup. |
| `PITLANE_EMBED_BATCH_SIZE` | `256` | Positive number of documents sent per embedding request. |
| `PITLANE_EMBED_TIMEOUT` | `120` | Per-request HTTP timeout in seconds. |
| `PITLANE_EMBED_MAX_CONCURRENCY` | `16` | Positive maximum number of concurrent embedding requests. |
| `PITLANE_EMBED_MAX_RETRIES` | `3` | Retry count for rate limits and transient server errors. Zero disables retries. |
| `PITLANE_EMBED_RETRY_BASE_MS` | `500` | Initial exponential-backoff delay in milliseconds when `Retry-After` is absent. |
| `PITLANE_EMBED_REQUEST_DELAY_MS` | `0` | Minimum delay in milliseconds between consecutive requests. Zero disables throttling. |

## Embedding documents

Changing these settings changes the embedding document fingerprint and may cause
stored vectors to be rebuilt.

| Variable | Default | Description |
|---|---:|---|
| `PITLANE_EMBED_DOCUMENT_PROFILE` | `metadata_code` | Document shape: `metadata_code`, `metadata`, or `legacy`. Values are case-insensitive; an unknown value selects `metadata_code`. |
| `PITLANE_EMBED_MAX_CHARS` | `6000` | Positive maximum length of a complete embedding document. |
| `PITLANE_EMBED_BODY_CHARS` | `3000` | Positive maximum source-code excerpt length in the `metadata_code` profile. |
| `PITLANE_EMBED_MAX_IDENTIFIERS` | `64` | Positive maximum number of source identifiers in the `metadata_code` profile. |
| `PITLANE_EMBED_TASK_PREFIX_MODE` | `auto` | Task-prefix policy: `auto`, `none`, or `nomic`. `auto` selects Nomic prefixes when the model name contains `nomic`; `none` emits no automatic prefix. |
| `PITLANE_EMBED_DOCUMENT_PREFIX` | automatic | Exact document prefix override. When set, including to an empty string, it replaces the task-prefix policy. |
| `PITLANE_EMBED_QUERY_PREFIX` | automatic | Exact query prefix override. When set, including to an empty string, it replaces the task-prefix policy. |

## Semantic ranking and queries

All ranking weights must be finite, non-negative numbers. Invalid values use the
listed defaults. `PITLANE_KNOWLEDGE_SEMANTIC_WEIGHT` is additionally constrained
to the range `0.0` through `1.0`.

| Variable | Default | Description |
|---|---:|---|
| `PITLANE_SEMANTIC_LEXICAL_WEIGHT` | `0.10` | Identifier and metadata overlap contribution to symbol ranking. |
| `PITLANE_SEMANTIC_BM25_WEIGHT` | `0.03` | BM25 contribution to symbol ranking. |
| `PITLANE_SEMANTIC_TEST_PENALTY` | `0.12` | Soft path penalty for test and example symbols when the query does not request them. |
| `PITLANE_SEMANTIC_AUXILIARY_PENALTY` | `0.03` | Soft path penalty for auxiliary, vendored, or third-party symbols. |
| `PITLANE_SEMANTIC_KIND_WEIGHT` | `0.01` | Callable and implementation-kind preference in symbol ranking. |
| `PITLANE_SEMANTIC_SESSION_WEIGHT` | `0` | Cross-query session-history contribution. Zero keeps it disabled. |
| `PITLANE_KNOWLEDGE_SEMANTIC_WEIGHT` | `0.7` | Semantic contribution to hybrid Markdown knowledge ranking; the remaining contribution is lexical. |
| `PITLANE_SEMANTIC_QUERY_TIMEOUT_MS` | `10000` / `15000` | Query-embedding timeout in milliseconds: `10000` for symbol search and `15000` for knowledge search. Use a positive value for consistent behavior across both paths. |

## Standard environment variables

| Variable | Default | Description |
|---|---:|---|
| `RUST_LOG` | `pitlane_mcp=info` | Logging filter understood by `tracing_subscriber`. Pitlane adds its built-in `pitlane_mcp=info` directive. Logs are written to stderr so MCP stdout remains protocol-safe. |
| `HOME` | platform value | Preferred base directory for `.pitlane/indexes` and `.pitlane/stats.json`. |
| `USERPROFILE` | platform value | Used as the home directory when `HOME` is unavailable, primarily on Windows. |

Pitlane currently stores state under `.pitlane` in the resolved home directory;
it does not consult `XDG_CACHE_HOME`.
