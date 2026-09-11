# AGENT HANDOFF — Knowledge-Base Indexing (issue #118), Phase 1

Branch: `feat/knowledge-indexing-phase1`

## Objective
Phase 1 of issue #118: generic knowledge-source abstraction + filesystem Markdown
source, heading-aware section chunking with hierarchy metadata, reuse of existing
BM25/embedding infrastructure, incremental re-indexing, and an MCP search tool.
OKF-specific logic stays out of core; front matter is parsed into a generic
metadata bag now so Phase 2 (OKF fields/relationships) can build on it.

## Completed work
- [x] Unit 1: `src/knowledge/document.rs` — `KnowledgeDocument`, `KnowledgeSection`,
      heading-aware Markdown sectioning (pulldown_cmark), minimal YAML front-matter
      subset parser, slug-based stable section IDs, embedding-text helper. Unit tests.
- [x] Unit 2: `src/index/knowledge_bm25.rs` — tantivy schema/build/ensure/search over
      sections (OR semantics, field boosts, shared "code" tokenizer + escape_query).
- [x] Unit 3: `src/knowledge/mod.rs` — `ContentSource` trait + `MarkdownSource`,
      `KnowledgeIndex` with incremental indexing (mtime+size fast path, blake3 hash
      fallback), documents.bin/meta.json persistence under <index_dir>/knowledge/,
      code-index exclusion policy (defaults + .gitignore + saved excludes).
- [x] Unit 4: `generate_knowledge_embeddings` in `src/knowledge/mod.rs` — section
      embeddings via shared EmbedClient/EmbedStore infra into
      `<index_dir>/knowledge/embeddings.bin`; hash-based skip; separate store file.
- [x] Unit 5: `ensure_knowledge_index` orchestrator (incremental index → BM25
      rebuild → persist; returns `(changed, KnowledgeIndex)`), `is_ready()` helper
      in knowledge_bm25, and `src/tools/search_knowledge.rs` MCP tool with hybrid
      lexical (BM25)/semantic (cosine) ranking, tag + path filters, pure-semantic
      fallback scan, and lazy background embedding generation (deduped per
      project). Registered in main.rs (project_field! + #[tool]) + docs.

## Important decisions
- Separate storage under existing per-project index dir:
  `~/.pitlane/indexes/<hash>/knowledge/` → `documents.bin`, `meta.json`,
  `tantivy/` (sentinel `.ready.v1`), `embeddings.bin` (+`.meta.json`).
  Code and knowledge indexes evolve independently; embedding *client/batch* infra
  is reused but the store file is separate (different document profile).
- IDs: doc_id = `knowledge:<relative/path.md>`; section key = `<doc_id>#<slug>`,
  slug = slugified heading hierarchy, collision-suffixed (`-2`). Preamble sections
  (text before first H1) use slug `preamble`. SymbolId is already a String alias,
  so string IDs work with the existing EmbedStore type.
- Section content = markdown from after its heading to the NEXT heading at ANY level
  (leaf chunking, standard RAG style). Empty-content sections are kept for
  navigation/line ranges but skipped for embeddings; BM25 indexes their name only.
- Front matter: minimal YAML subset (top-level `key: value`, inline `[a, b]` lists,
  block `- item` lists). Full YAML is a Phase 2 upgrade. Unknown keys preserved in
  `front_matter` JSON bag for OKF extensibility.
- Title resolution: front matter `title` > first H1 > file stem.
- Tags from front matter `tags` and/or `categories`.
- `search_knowledge` builds the knowledge index lazily + incrementally on first use
  (no code-index dependency). When `PITLANE_EMBED_*` are set, section embedding
  generation is triggered from a background task after doc changes or when the
  store/embed model is stale; a per-project `EMBED_INFLIGHT` set (RwLock) dedupes
  concurrent spawns.

## Files changed
- `src/knowledge/mod.rs` (new)
- `src/knowledge/document.rs` (new)
- `src/index/knowledge_bm25.rs` (new)
- `src/index/mod.rs` (declare module)
- `src/index/format.rs` (add `knowledge_dir()`)
- `src/index/bm25.rs` (pub(crate): register_tokenizer, TOKENIZER_NAME, escape_query)
- `src/lib.rs` (add `pub mod knowledge;`)
- `src/tools/search_knowledge.rs` (new MCP tool)
- `src/tools/mod.rs` (declare module)
- `src/main.rs` (SearchKnowledgeRequest + project_field! registration + #[tool] handler)
- `AGENTS.md`, `README.md`, `docs/tools.md` (tool documentation)

## Tests / results
- `cargo test --lib` → 720 passed, 0 failed. Clippy clean, fmt clean.
- Commits: 4ea9d62 (unit 1), bfccd44 (unit 2), a5b82bf (unit 3),
  3f2c68f (unit 4), TBD (unit 5).

## Unresolved issues
- None blocking. Notes: bincode cannot deserialize serde_json::Value → front matter
  persisted as JSON string (accessor `front_matter_value()`). Setext headings fold
  the whole preceding paragraph into the heading (CommonMark) — parser handles it.
- Embedding dimension mismatch between a stale store and a freshly embedded section
  is handled by skipping at save time (warn log) — a `force`/rebuild knob for the
  knowledge store is not exposed yet (Phase 2 could add one via doctor/ensure).

## Next actions
1. (PR 1) Commit unit 5 as `closes #118`-style knowledge search feature branch work:
   verify full suite after final fmt, commit with a message describing
   `search_knowledge` tool, push branch, open PR (towards #118).
2. Phase 2 (separate PR(s)): OKF metadata fields/relationships on the generic
   front-matter bag; possibly a `force` embed rebuild knob and richer hybrid
   weights documented.