# AGENT HANDOFF — Knowledge-Base Indexing (issue #118), Phase 2

Branch: `feat/knowledge-indexing-phase1`

## Objective
Phase 2 of issue #118: OKF-aware indexing. OKF = Open Knowledge Format v0.2
(spec: <https://github.com/GoogleCloudPlatform/open-knowledge-format> — note the
spec moved out of GoogleCloudPlatform/knowledge-catalog; the `okf/` dir there is
a frozen snapshot). Full YAML front matter parsing, typed OKF metadata
(provenance/trust/lifecycle), metadata-aware filtering/ranking, cross-document
link preservation, and a representative OKF bundle integration test.

## Completed work
- [x] `src/knowledge/okf.rs` (new) — `OkfMeta`, `TrustTier`, `OkfSource`,
      `VerifiedEvent`; `extract_okf` over full YAML front matter; `is_stale`
      (absolute `stale_after` comparison) via `chrono` (RFC 3339).
- [x] `src/knowledge/document.rs` — front matter now parsed with `serde_yaml`
      (Phase 1 used a minimal YAML subset). Unknown keys still preserved in the
      `front_matter` JSON bag (converted YAML→JSON). `KnowledgeDocument` gained
      `okf: Option<OkfMeta>` and `links: Vec<KnowledgeLink>`.
- [x] `src/knowledge/links.rs` (new) — pulldown_cmark link extraction,
      bundle-relative (`/x.md`) and relative (`./x.md`) resolution to
      project-relative normalized `.md` targets; external/anchor/code targets
      ignored; duplicates collapse by target (first text wins).
- [x] `src/tools/search_knowledge.rs` + `src/main.rs` — new filters
      `okf_type`, `status`, `min_trust` (OKF filters never match plain
      Markdown documents; invalid `min_trust` fails fast). Results expose
      `okf_type`, `description`, `resource`, `status`, `trust_tier`, `stale`,
      `metadata_adjustment`, `related_docs`. Ranking adjustment: +0.05
      human-reviewed, +0.02 machine-confirmed, −0.10 stale, −0.10 deprecated,
      −0.02 draft (clamped ±0.25), applied to the blended/lexical score in
      BOTH paths — the lexical-only path re-sorts after adjustment.
- [x] `tests/knowledge_okf.rs` (new) — representative OKF v0.2 bundle modeled
      on spec Appendix A: metrics, attested computations in different
      trust/staleness states, cross-links, `index.md`/`log.md` reserved files,
      a plain-Markdown doc. 8 end-to-end tests.
- [x] Docs: `docs/tools.md`, `README.md`, `AGENTS.md`.

## Important decisions
- Full YAML via `serde_yaml 0.9` (deprecated upstream but stable/fine); the
  Phase 1 minimal parser is deleted. Malformed or unterminated front matter is
  treated as plain Markdown body (spec-strict bundles would fail, we stay
  lenient per §11).
- `KNOWLEDGE_META_VERSION` bumped 1 → 2 (persisted layout gained `okf`/`links`
  fields; bincode cannot decode old payloads anyway).
- Trust tier derivation follows spec §5.3 (bare `verified` mapping = one-event
  list); `verified_at` = max of event timestamps. Staleness compares now >=
  `stale_after` in Unix seconds; malformed timestamps are never stale.
- Metadata adjustments are advisory nudges in blended-score units, not hard
  filters; env-tunable knobs intentionally deferred (Phase 3 can revisit).
- Computation-family fields (`runtime`, `executor`, `attester`, `parameters`)
  are NOT modeled — they stay in the generic front-matter bag. Add typed
  support only when a tool actually needs them.

## Tests / results
- `cargo test --lib` → 740 passed. `cargo test --test knowledge_okf` → 8 passed.
- Clippy `-D warnings` clean, fmt clean.
- Commits: b5874c3 (units 1+2), 7ed455f (unit 3), 2f8540a (unit 4 tests/docs),
  7b737b0 (lexical re-sort fix).

## Phase 3 outlook (from the issue)
- Hybrid/unified retrieval across code + knowledge; full-document retrieval
  after search; unified MCP search experience. The per-result `related_docs`
  and OKF metadata in place make a graph-viewer or follow-links tool easy.
