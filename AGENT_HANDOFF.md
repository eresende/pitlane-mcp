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
- `src/knowledge/mod.rs` (new, stub)
- `src/knowledge/document.rs` (new)
- `src/lib.rs` (add `pub mod knowledge;`)

## Tests / results
- `cargo test -p pitlane-mcp --lib knowledge::` → (fill in after run)

## Unresolved issues
- None yet. Watch for: pulldown_cmark 0.13 event API details; setext headings.

## Next actions (in order, each atomic + tested)
1. `src/index/knowledge_bm25.rs` — tantivy schema/build/ensure/search over sections
   (fields: section_id [STRING|STORED], name, hierarchy, content, tags, file_path,
   doc_title; reuse the "code" tokenizer approach or a plain text one), tests.
2. `src/knowledge/mod.rs` — `ContentSource` trait + `FilesystemMarkdownSource`
   (walk .md/.markdown, skip symlinks/binary/>1MiB, honor excludes),
   `KnowledgeIndex` load/save/incremental (mtime+size+blake3 per file in meta.json),
   tests.
3. Embedding integration — knowledge section embedding docs + separate store under
   `knowledge/`, reuse embed client; wire into index_project flow.
4. `src/tools/search_knowledge.rs` — MCP tool (query, optional tag/path filters,
   limit) with lexical BM25 + semantic hybrid when embeddings exist; register in
   main.rs; update README/docs.
