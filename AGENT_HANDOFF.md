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

## Files changed
- `src/knowledge/mod.rs` (new)
- `src/knowledge/document.rs` (new)
- `src/index/knowledge_bm25.rs` (new)
- `src/index/mod.rs` (declare module)
- `src/index/format.rs` (add `knowledge_dir()`)
- `src/index/bm25.rs` (pub(crate): register_tokenizer, TOKENIZER_NAME, escape_query)
- `src/lib.rs` (add `pub mod knowledge;`)

## Tests / results
- `cargo test --lib` → 714 passed, 0 failed. Clippy clean.
- Commits: 4ea9d62 (unit 1), bfccd44 (unit 2), a5b82bf (unit 3).

## Unresolved issues
- None blocking. Notes: bincode cannot deserialize serde_json::Value → front matter
  persisted as JSON string (accessor `front_matter_value()`). Setext headings fold
  the whole preceding paragraph into the heading (CommonMark) — parser handles it.

## Next actions (in order, each atomic + tested)
1. Embedding integration — knowledge section embedding docs + separate store under
   `knowledge/embeddings.bin`, reuse embed client; wire into index_project flow so
   initial code indexing also indexes knowledge (BM25 ensure + embeddings when
   configured).
2. `src/tools/search_knowledge.rs` — MCP tool (query, optional tag/path filters,
   limit) with lexical BM25 + semantic hybrid when embeddings exist; register in
   main.rs; update README/docs.
