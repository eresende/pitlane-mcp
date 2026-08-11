# Semantic search

## Pipeline

Pitlane parses files with the language-specific tree-sitter indexers and stores
`Symbol` records in `SymbolIndex`. A symbol carries its stable ID, name,
qualified name, kind, language, file and byte/line span, signature, and optional
documentation.

`generate_embeddings` turns each symbol into a bounded document, batches those
documents, calls the configured OpenAI/Ollama-compatible embedding endpoint,
L2-normalizes every returned vector, validates dimensions, and stores vectors by
stable symbol ID in `embeddings.bin`. `embeddings.meta.json` records the model,
endpoint identity, dimension, document format, size settings, and
query/document prefixes. A change to any of these invalidates the cache and
causes a rebuild. Credential header values are deliberately excluded from cache
identity, so rotating credentials does not rebuild vectors. Non-secret routing
headers, such as tenant identifiers, are included.

The default `metadata_code` document includes relative file path, name,
qualified name, kind, language, signature, documentation, identifiers extracted
from the complete symbol, and a bounded head/tail source excerpt. The
`metadata` profile omits identifiers and source; `legacy` reproduces the old
name + qualified name + signature + documentation representation.

At query time Pitlane applies the configured query task prefix, normalizes the
query vector, checks its dimension and cache identity, and computes cosine
similarity as a dot product over normalized vectors. Hybrid ranking adds bounded
identifier/metadata overlap, BM25 rank, callable-kind preference, and soft path
penalties for tests, examples, vendored, and third-party code. Test penalties
are disabled when the query asks for tests or examples. Cross-query session
boosting is disabled by default because it made previously viewed symbols sticky
for unrelated queries.

`semantic_debug` returns raw cosine similarity, final score, BM25 rank, and every
adjustment. `locate_code` preserves this final semantic order; `investigate`
uses the same search path before reading source.

## Configuration

- `PITLANE_EMBED_DOCUMENT_PROFILE=metadata_code|metadata|legacy`
- `PITLANE_EMBED_MAX_CHARS` (default `6000`)
- `PITLANE_EMBED_BODY_CHARS` (default `3000`)
- `PITLANE_EMBED_MAX_IDENTIFIERS` (default `64`)
- `PITLANE_EMBED_TASK_PREFIX_MODE=auto|none|nomic`
- `PITLANE_EMBED_DOCUMENT_PREFIX` and `PITLANE_EMBED_QUERY_PREFIX` override task prefixes
- `PITLANE_EMBED_API_KEY` adds an `Authorization: Bearer ...` header
- `PITLANE_EMBED_HEADERS` adds arbitrary request headers from a JSON object of string values
- `PITLANE_EMBED_MAX_CONCURRENCY` limits concurrent endpoint requests (default `16`)
- `PITLANE_EMBED_MAX_RETRIES` retries `429` and transient `5xx` responses (default `3`)
- `PITLANE_EMBED_RETRY_BASE_MS` sets exponential backoff when `Retry-After` is absent (default `500`)
- `PITLANE_SEMANTIC_LEXICAL_WEIGHT` (default `0.10`)
- `PITLANE_SEMANTIC_BM25_WEIGHT` (default `0.03`)
- `PITLANE_SEMANTIC_TEST_PENALTY` (default `0.12`)
- `PITLANE_SEMANTIC_AUXILIARY_PENALTY` (default `0.03`)
- `PITLANE_SEMANTIC_KIND_WEIGHT` (default `0.01`)
- `PITLANE_SEMANTIC_SESSION_WEIGHT` (default `0`)

`PITLANE_EMBED_URL` can target any reachable OpenAI-compatible embedding endpoint,
including company gateways. The endpoint must accept `model` and `input` fields
and return `data[i].embedding`. Credentials are sent through environment-backed
headers. Credential values are never included in embedding cache identity.

HTTPS requests trust both public WebPKI roots and certificates installed in the
operating system's native trust store, including company-internal CAs.

## llama.cpp benchmark

Build and index with the desired configuration, then run:

```sh
cargo build --bin pitlane
python3 bench/semantic_retrieval.py /path/to/llama.cpp \
  --output /tmp/semantic-results.json
```

The harness calls only `search --mode semantic_debug`; no LLM or fallback code
search can mask retrieval failures. It reports exact top-ten rankings, raw and
final scores, hit@1/3/5, and mean reciprocal rank.
