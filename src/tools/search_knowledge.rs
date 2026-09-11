//! Knowledge-base search over indexed Markdown documents (Phase 1 of #118).
//!
//! Hybrid ranking: lexical BM25 over sections plus semantic cosine similarity
//! when an embedding store exists. Falls back to pure-semantic top-k across all
//! embedded sections when the query has no lexical matches at all.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use serde_json::{json, Value};

use crate::embed::client::{cosine_similarity, EmbedClient};
use crate::embed::store::{EmbedStore, EmbedStoreMetadata};
use crate::embed::EmbedConfig;
use crate::error::ToolError;
use crate::index::format::knowledge_dir;
use crate::knowledge::okf::{OkfMeta, TrustTier};
use crate::knowledge::{document, generate_knowledge_embeddings, okf, with_knowledge_index};
use crate::path_policy::resolve_project_path;
use crate::tools::steering::{attach_steering, build_steering};

const DEFAULT_LIMIT: usize = 8;
const MAX_LIMIT: usize = 50;
/// Lexical candidates fetched before hybrid re-ranking.
fn candidate_fetch(limit: usize) -> usize {
    (limit * 4).clamp(60, 300)
}
/// Snippet length in characters for search results.
const SNIPPET_CHARS: usize = 320;

pub struct SearchKnowledgeParams {
    pub project: String,
    /// Natural-language query describing the information need.
    pub query: String,
    /// Optional document-level tag filter (front matter `tags`/`categories`).
    pub tag: Option<String>,
    /// Optional substring filter on the relative file path.
    pub path_filter: Option<String>,
    /// Optional OKF concept-type filter (`type` front matter), case-insensitive.
    pub okf_type: Option<String>,
    /// Optional OKF lifecycle filter: `draft`, `stable`, or `deprecated`.
    pub status: Option<String>,
    /// Optional minimum OKF trust tier: `unverified`, `machine-confirmed`,
    /// or `human-reviewed`.
    pub min_trust: Option<String>,
    pub limit: Option<usize>,
    pub embed_config: Option<Arc<EmbedConfig>>,
}

/// Document-level filters resolved once per call.
struct DocFilter<'a> {
    tag: Option<&'a str>,
    okf_type: Option<&'a str>,
    status: Option<&'a str>,
    min_trust: Option<TrustTier>,
}

/// One ranked result section with its score breakdown.
struct Candidate {
    full_section_id: String,
    file_path: String,
    doc_title: String,
    tags: Vec<String>,
    heading: String,
    line_start: usize,
    line_end: usize,
    snippet: String,
    bm25_score: Option<f32>,
    /// OKF metadata when the document is an OKF concept.
    okf: Option<OkfMeta>,
    /// Outgoing links to other knowledge documents.
    links: Vec<crate::knowledge::links::KnowledgeLink>,
    /// True when the document's `stale_after` instant has passed.
    stale: bool,
    /// Score adjustment from OKF trust/lifecycle signals.
    metadata_adjustment: f32,
}

pub async fn search_knowledge(params: SearchKnowledgeParams) -> anyhow::Result<Value> {
    if params.query.trim().is_empty() {
        return Err(ToolError::InvalidArgument {
            param: "query".to_string(),
            message: "query must not be empty".to_string(),
        }
        .into());
    }

    let canonical = resolve_project_path(&params.project)?;
    // Validate filter values up front so typos fail fast instead of silently
    // returning nothing.
    let doc_filter = DocFilter {
        tag: params
            .tag
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty()),
        okf_type: params
            .okf_type
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty()),
        status: params
            .status
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        min_trust: params.min_trust.as_deref().and_then(TrustTier::parse),
    };
    if let Some(min_trust) = params
        .min_trust
        .as_deref()
        .filter(|_| doc_filter.min_trust.is_none())
    {
        return Err(ToolError::InvalidArgument {
            param: "min_trust".to_string(),
            message: format!(
                "invalid trust tier {min_trust:?}: use unverified, machine-confirmed, or human-reviewed"
            ),
        }
        .into());
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let fetch = candidate_fetch(limit);
    // Incremental indexing may walk many files — keep it off the async runtime.
    let root_for_blocking = canonical.clone();
    let query = params.query.clone();
    let (changed, index, hits) = tokio::task::spawn_blocking(move || {
        with_knowledge_index(&root_for_blocking, |changed, index| {
            let hits = crate::index::knowledge_bm25::search(
                &query,
                &root_for_blocking,
                &knowledge_dir(&root_for_blocking)?.join("tantivy"),
                fetch,
            )?;
            Ok((changed, index, hits))
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!("knowledge indexing task failed: {e}"))??;

    if changed && !index.documents.is_empty() {
        tracing::info!(
            docs = index.documents.len(),
            "search_knowledge: knowledge documents updated"
        );
    }

    // Kick off background embedding generation when configured and the store
    // is missing or stale. Deduped per project so concurrent searches do not
    // double-spawn; steady-state requests with a fresh store never spawn.
    maybe_spawn_knowledge_embeds(&canonical, &index, changed, params.embed_config.clone());

    let kdir = knowledge_dir(&canonical)?;

    // ── Lexical candidates (BM25 over sections) ────────────────────────────
    let mut candidates: Vec<Candidate> = Vec::new();
    for hit in &hits {
        if let Some(mut cand) = resolve_candidate(&index, hit.section_id.as_str(), &doc_filter)
            .filter(|c| path_matches(params.path_filter.as_deref(), &c.file_path))
        {
            cand.bm25_score = Some(hit.score);
            candidates.push(cand);
        }
    }

    // ── Semantic availability (non-fatal: lexical still works) ─────────────
    let store_path = kdir.join("embeddings.bin");
    let mut query_vec: Option<Vec<f32>> = None;
    let mut store: Option<EmbedStore> = None;
    if let Some(cfg) = params.embed_config.clone() {
        if let Some(compatible_store) = load_compatible_store(&store_path, &cfg) {
            match tokio::time::timeout(
                std::time::Duration::from_millis(query_timeout_ms()),
                EmbedClient::new(Arc::clone(&cfg)).embed_query(&params.query),
            )
            .await
            {
                Ok(Ok(vec)) => {
                    query_vec = Some(vec);
                    store = Some(compatible_store);
                }
                Err(_) => {
                    tracing::warn!(
                        "search_knowledge: semantic scoring disabled (query embedding timed out)"
                    );
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "search_knowledge: semantic scoring disabled ({e})");
                }
            }
        }
    }

    // ── Scoring ─────────────────────────────────────────────────────────────
    if candidates.is_empty() {
        // Pure-semantic fallback: rank every embedded section.
        let Some(scan_store) = store.as_ref() else {
            return Ok(empty_response(&index, &params.query));
        };
        for id in scan_store.vectors.keys() {
            if let Some(cand) = resolve_candidate(&index, id, &doc_filter)
                .filter(|c| path_matches(params.path_filter.as_deref(), &c.file_path))
            {
                candidates.push(cand);
            }
        }
    }

    let sem_weight = semantic_weight();
    // Hybrid: weighted blend of cosine similarity and max-normalized BM25,
    // plus an OKF trust/lifecycle adjustment. Without a query vector we keep
    // the lexical ranking as-is (still adjusted).
    let adjusted = |cand: &Candidate, base: f32| (base + cand.metadata_adjustment).max(0.0);
    let scored: Vec<(f64, Candidate)> = if let Some(qv) = query_vec.as_ref() {
        let max_bm25 = candidates
            .iter()
            .filter_map(|c| c.bm25_score)
            .fold(0.0_f32, f32::max);
        let mut out: Vec<(f64, Candidate)> = Vec::new();
        for cand in std::mem::take(&mut candidates) {
            let sim = store
                .as_ref()
                .and_then(|s| s.vectors.get(&cand.full_section_id))
                .filter(|v| v.len() == qv.len())
                .map(|v| cosine_similarity(qv, v));
            let lexical = cand
                .bm25_score
                .map(|s| max_bm25.max(f32::EPSILON).recip() * s);
            let score: f32 = match (sim, lexical) {
                (Some(s), Some(l)) => sem_weight * s + (1.0 - sem_weight) * l,
                (Some(s), None) => s,
                (_, Some(l)) => l,
                (None, None) => 0.0,
            };
            out.push((round_frac3(f64::from(adjusted(&cand, score))), cand));
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(limit);
        out
    } else {
        // No semantic signal: BM25 order adjusted by metadata, then re-sorted
        // because the adjustment can reorder near-equal lexical scores.
        let mut out: Vec<(f64, Candidate)> = std::mem::take(&mut candidates)
            .into_iter()
            .map(|c| {
                let score = adjusted(&c, c.bm25_score.unwrap_or(0.0));
                (round_frac3(f64::from(score)), c)
            })
            .collect();
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(limit);
        out
    };

    // ── Response ────────────────────────────────────────────────────────────
    let total_sections: usize = index.documents.values().map(|d| d.sections.len()).sum();
    let embeddings_used =
        query_vec.is_some() && store.as_ref().is_some_and(|s| !s.vectors.is_empty());

    let results: Vec<Value> = scored
        .into_iter()
        .map(|(score, c)| {
            let okf = c.okf.as_ref();
            json!({
                "section_id": c.full_section_id,
                "file_path": c.file_path,
                "doc_title": c.doc_title,
                "heading": c.heading,
                "line_start": c.line_start,
                "line_end": c.line_end,
                "tags": if c.tags.is_empty() { Value::Null } else { json!(c.tags) },
                "snippet": c.snippet,
                "score": score,
                "okf_type": okf.and_then(|m| (!m.doc_type.is_empty()).then(|| json!(m.doc_type))),
                "description": okf.and_then(|m| m.description.as_deref().map(|d| json!(d))),
                "resource": okf.and_then(|m| m.resource.as_deref().map(|r| json!(r))),
                "status": okf.and_then(|m| m.status.as_deref().map(|st| json!(st))),
                "trust_tier": okf.map(|m| json!(m.trust_tier.as_str())),
                "stale": c.stale,
                "metadata_adjustment": round_frac3(f64::from(c.metadata_adjustment)),
                "related_docs": if c.links.is_empty() {
                    Value::Null
                } else {
                    json!(c.links.iter().map(|l| json!({
                        "target": l.target,
                        "text": l.text,
                    })).collect::<Vec<_>>())
                },
            })
        })
        .collect();

    let mut response = json!({
        "query": params.query,
        "total_sections": total_sections,
        "results_count": results.len(),
        "embeddings_used": embeddings_used,
        "results": results,
        "guidance": {
            "next_step": if results.is_empty() {
                "No knowledge sections matched. Try a broader query or check that Markdown files exist in the project (they must not be gitignored)."
            } else {
                "Use read_knowledge_document with file_path to open the full document, or pass section_id to fetch only that section."
            },
            "avoid": "Avoid loading whole documentation trees into context; fetch sections on demand.",
        },
    });

    let top = results.first();
    let steering = build_steering(
        if results.is_empty() { 0.2 } else { 0.85 },
        if results.is_empty() {
            "No knowledge section matched the query.".to_string()
        } else {
            format!(
                "Top result scores {} on the combined lexical/semantic ranking.",
                top.as_ref()
                    .and_then(|r| r["score"].as_f64())
                    .unwrap_or(0.0)
            )
        },
        if results.is_empty() {
            "search_content"
        } else {
            "read_knowledge_document"
        },
        match &top {
            Some(r) => json!({
                "document": r["file_path"],
                "section": r["section_id"],
            }),
            None => json!(params.project),
        },
        Vec::new(),
    );
    attach_steering(&mut response, steering);
    Ok(response)
}

/// Resolve a `knowledge:<path>#<slug>` ID to its section payload.
fn resolve_candidate(
    index: &crate::knowledge::KnowledgeIndex,
    full_id: &str,
    filter: &DocFilter<'_>,
) -> Option<Candidate> {
    let (doc_id, slug) = full_id.split_once('#')?;
    let doc = index.documents.get(doc_id)?;
    if let Some(tag) = filter.tag {
        if !matches_tag(&doc.tags, tag) {
            return None;
        }
    }
    let okf = doc.okf.clone();
    if let Some(meta) = &okf {
        if let Some(okf_type) = filter.okf_type {
            if !meta.doc_type.eq_ignore_ascii_case(okf_type) {
                return None;
            }
        }
        if let Some(status) = filter.status {
            match meta.status.as_deref().unwrap_or("stable") {
                s if s.eq_ignore_ascii_case(status) => {}
                _ => return None,
            }
        }
        if let Some(min_trust) = filter.min_trust {
            if meta.trust_tier < min_trust {
                return None;
            }
        }
    } else if filter.okf_type.is_some() || filter.status.is_some() || filter.min_trust.is_some() {
        // OKF filters never match plain-Markdown documents.
        return None;
    }
    let section = doc.sections.iter().find(|s| s.section_id == slug)?.clone();
    let stale = okf
        .as_ref()
        .is_some_and(|m| okf::is_stale(m, okf::now_secs()));
    Some(Candidate {
        full_section_id: full_id.to_string(),
        file_path: doc.file_path.clone(),
        doc_title: doc.title.clone(),
        tags: if doc.tags.is_empty() {
            Vec::new()
        } else {
            doc.tags.clone()
        },
        heading: section_heading(&section),
        line_start: section.line_start,
        line_end: section.line_end,
        snippet: make_snippet(&section.content),
        bm25_score: None,
        metadata_adjustment: metadata_adjustment(okf.as_ref(), stale),
        stale,
        okf,
        links: doc.links.clone(),
    })
}

/// Score adjustment from OKF trust and lifecycle signals. Advisory nudges, in
/// the same units as the blended hybrid score: verified concepts gain a little,
/// stale, deprecated, and draft concepts lose a little. Plain-Markdown
/// documents are unaffected.
fn metadata_adjustment(meta: Option<&OkfMeta>, stale: bool) -> f32 {
    let Some(meta) = meta else {
        return 0.0;
    };
    let mut adjustment: f32 = match meta.trust_tier {
        TrustTier::HumanReviewed => 0.05,
        TrustTier::MachineConfirmed => 0.02,
        TrustTier::Unverified => 0.0,
    };
    if stale {
        adjustment -= 0.10;
    }
    match meta.status.as_deref().unwrap_or("stable") {
        "deprecated" => adjustment -= 0.10,
        "draft" => adjustment -= 0.02,
        _ => {}
    }
    adjustment.clamp(-0.25, 0.25)
}

fn matches_tag(tags: &[String], tag: &str) -> bool {
    tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
}

/// Substring match on the relative file path; `None` filter passes everything.
fn path_matches(filter: Option<&str>, path: &str) -> bool {
    filter.is_none_or(|f| f.trim().is_empty() || path.contains(f))
}

fn section_heading(section: &document::KnowledgeSection) -> String {
    if section.hierarchy.is_empty() {
        "(preamble)".to_string()
    } else {
        section.hierarchy.join(" > ")
    }
}

/// Compact one-paragraph preview of a section body.
fn make_snippet(content: &str) -> String {
    let collapsed = content
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>();
    let joined = if collapsed.is_empty() {
        String::new()
    } else {
        // Skip leading Markdown heading lines; keep body text.
        collapsed
            .iter()
            .skip_while(|line| line.starts_with('#'))
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let joined = joined.trim().to_string();
    if joined.len() <= SNIPPET_CHARS {
        return joined;
    }
    // Cut on a word boundary at or before the limit.
    let mut end = SNIPPET_CHARS.min(joined.len());
    while !joined.is_char_boundary(end) {
        if end == 0 {
            break;
        }
        end -= 1;
    }
    // Prefer a word boundary within the limit.
    let cut_end = joined[..end].trim_end().len();
    format!("{}…", &joined[..cut_end])
}

/// Response shape for "nothing matched at all" (no lexical hits and no
/// usable semantic store).
fn empty_response(index: &crate::knowledge::KnowledgeIndex, query: &str) -> Value {
    let total_sections: usize = index.documents.values().map(|d| d.sections.len()).sum();
    let mut response = json!({
        "query": query,
        "total_sections": total_sections,
        "results_count": 0usize,
        "embeddings_used": false,
        "results": [],
        "guidance": {
            "next_step": if total_sections == 0 {
                "No Markdown documents are indexed for this project yet. Add .md files (not gitignored) and re-run ensure_project_ready, or use search_content to find text directly in source files."
            } else {
                "No knowledge sections matched. Try a broader query with fewer terms; headings match strongly, so naming the likely heading helps."
            },
            "avoid": "Avoid loading whole documentation trees into context; fetch sections on demand.",
        },
    });
    let steering = build_steering(
        0.2,
        if total_sections == 0 {
            "The project has no indexed knowledge documents.".to_string()
        } else {
            "No knowledge section matched the query".to_string()
        },
        "search_content",
        json!({}),
        Vec::new(),
    );
    attach_steering(&mut response, steering);
    response
}

// Projects whose background knowledge-embedding task is currently running.
static EMBED_INFLIGHT: LazyLock<RwLock<HashSet<PathBuf>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Spawn a detached task that (re)generates section embeddings when the store
/// is missing, incompatible with the configured model/endpoint, or dirty.
pub(crate) fn maybe_spawn_knowledge_embeds(
    canonical: &Path,
    index: &crate::knowledge::KnowledgeIndex,
    dirty: bool,
    embed_config: Option<Arc<EmbedConfig>>,
) {
    let Some(cfg) = embed_config else { return };
    let expected = index
        .documents
        .values()
        .flat_map(|d| d.sections.iter())
        .filter(|s| !s.is_empty())
        .count();
    if expected == 0 {
        return;
    }

    let store_path = match knowledge_dir(canonical) {
        Ok(kdir) => kdir.join("embeddings.bin"),
        Err(_) => return,
    };
    let fresh = load_compatible_store(&store_path, &cfg)
        .is_some_and(|store| knowledge_embeddings_complete(index, &store));
    if !dirty && fresh {
        return;
    }

    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let key = canonical.to_path_buf();
    {
        let mut inflight = crate::sync_utils::rw_write(&EMBED_INFLIGHT);
        if !inflight.insert(key.clone()) {
            return; // already running for this project
        }
    }

    // Clone only on the (rare) spawn path — steady-state requests skip here.
    let docs_owned: Vec<document::KnowledgeDocument> = index.documents.values().cloned().collect();
    tokio::spawn(async move {
        let result = generate_knowledge_embeddings(&docs_owned, &cfg, &store_path).await;
        crate::sync_utils::rw_write(&EMBED_INFLIGHT).remove(&key);
        if let Some(err) = result.error {
            tracing::error!(project = %key.display(), error = %err, "knowledge-embed: background generation failed");
        } else {
            tracing::info!(stored = result.stored, skipped = result.skipped, elapsed_ms = result.elapsed_ms, project = %key.display(), "knowledge-embed: background generation complete");
        }
    });
}

/// True when the on-disk embedding store is compatible with `cfg` and covers
/// every non-empty section of `index`. Used by the one-shot CLI to decide
/// whether embeddings must be regenerated synchronously before a search (the
/// MCP server instead refreshes them via `maybe_spawn_knowledge_embeds`).
pub fn knowledge_store_fresh(
    canonical: &Path,
    index: &crate::knowledge::KnowledgeIndex,
    cfg: &EmbedConfig,
) -> bool {
    match knowledge_dir(canonical) {
        Ok(kdir) => load_compatible_store(&kdir.join("embeddings.bin"), cfg)
            .is_some_and(|store| knowledge_embeddings_complete(index, &store)),
        Err(_) => false,
    }
}

fn knowledge_embeddings_complete(
    index: &crate::knowledge::KnowledgeIndex,
    store: &EmbedStore,
) -> bool {
    index.documents.values().all(|doc| {
        doc.sections
            .iter()
            .filter(|s| !s.is_empty())
            .all(|section| {
                let id = section.full_id(&doc.doc_id);
                store.vectors.get(&id).is_some_and(|v| !v.is_empty())
                    && store.hashes.get(&id)
                        == Some(&crate::embed::document::document_hash(
                            &section.embedding_text(doc),
                        ))
            })
    })
}

/// Load the embedding store only when its metadata is compatible with `cfg`.
fn load_compatible_store(store_path: &std::path::Path, cfg: &EmbedConfig) -> Option<EmbedStore> {
    let store = EmbedStore::load(store_path).ok()?;
    if store.vectors.is_empty() {
        return None;
    }
    let meta = EmbedStoreMetadata::load(store_path).ok().flatten()?;
    meta.is_compatible(
        &cfg.model,
        &crate::embed::document::document_fingerprint(&cfg.model),
        &crate::embed::endpoint_fingerprint(&cfg.url, &cfg.headers),
    )
    .then_some(store)
}

fn semantic_weight() -> f32 {
    std::env::var("PITLANE_KNOWLEDGE_SEMANTIC_WEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|w: &f32| w.is_finite() && *w >= 0.0 && *w <= 1.0)
        .unwrap_or(0.7)
}

fn query_timeout_ms() -> u64 {
    std::env::var("PITLANE_SEMANTIC_QUERY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|ms: &u64| *ms > 0)
        .unwrap_or(15_000)
}

fn round_frac3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap;
    use tempfile::TempDir;

    #[test]
    fn snippet_collapses_and_truncates_on_word_boundary() {
        let text = "# Title\nFirst line.\nSecond line.";
        assert_eq!(make_snippet(text), "First line. Second line.");

        let long = "word ".repeat(100);
        let out = make_snippet(&long);
        // Content ≤ limit bytes plus the ellipsis (3 UTF-8 bytes).
        let content_len = out.chars().count();
        assert!(content_len <= SNIPPET_CHARS + 1, "{content_len}");
        assert!(out.ends_with('…'));

        // Empty content → empty snippet.
        assert_eq!(make_snippet(""), "");
    }

    #[test]
    fn tag_and_path_filters_behave_as_documented() {
        let mut index = crate::knowledge::KnowledgeIndex::default();
        let doc = document::parse_markdown(
            "docs/a.md",
            "---\ntags: [ops]\n---\n# A\nBody here.\n## Sub\nMore body.\n",
        );
        index.documents.insert(doc.doc_id.clone(), doc);

        let no_filter = DocFilter {
            tag: None,
            okf_type: None,
            status: None,
            min_trust: None,
        };
        let tag_filter = DocFilter {
            tag: Some("OPS"),
            ..no_filter
        };
        assert!(resolve_candidate(&index, "knowledge:docs/a.md#a-sub", &tag_filter).is_some());
        let dev_filter = DocFilter {
            tag: Some("dev"),
            ..no_filter
        };
        assert!(resolve_candidate(&index, "knowledge:docs/a.md#a-sub", &dev_filter).is_none());
    }

    #[test]
    fn okf_filters_exclude_plain_markdown_and_match_metadata() {
        let mut index = crate::knowledge::KnowledgeIndex::default();
        let okf_doc = document::parse_markdown(
            "kb/revenue.md",
            "---\ntype: Attested Computation\nstatus: stable\nverified: { by: human:ana, at: 2026-06-25T09:00:00Z }\n---\n# Computation\nSELECT 1\n",
        );
        let plain_doc = document::parse_markdown("notes.md", "Plain notes.\n");
        index
            .documents
            .insert(okf_doc.doc_id.clone(), okf_doc.clone());
        index.documents.insert(plain_doc.doc_id.clone(), plain_doc);

        let base = DocFilter {
            tag: None,
            okf_type: None,
            status: None,
            min_trust: None,
        };
        let section_id = "knowledge:kb/revenue.md#computation";

        let type_filter = DocFilter {
            okf_type: Some("attested computation"),
            ..base
        };
        assert!(resolve_candidate(&index, section_id, &type_filter).is_some());
        // Plain Markdown documents never match OKF filters.
        assert!(resolve_candidate(&index, "knowledge:notes.md#preamble", &type_filter).is_none());

        let stale_filter = DocFilter {
            status: Some("deprecated"),
            ..base
        };
        assert!(resolve_candidate(&index, section_id, &stale_filter).is_none());

        let human_only = DocFilter {
            min_trust: Some(TrustTier::HumanReviewed),
            ..base
        };
        assert!(resolve_candidate(&index, section_id, &human_only).is_some());
        let machine_floor = DocFilter {
            min_trust: Some(TrustTier::MachineConfirmed),
            ..base
        };
        // Machine-confirmed floor also passes human-reviewed docs.
        assert!(resolve_candidate(&index, section_id, &machine_floor).is_some());
    }

    #[test]
    fn metadata_adjustment_follows_trust_and_lifecycle() {
        assert_eq!(metadata_adjustment(None, false), 0.0);

        let mut meta = OkfMeta {
            doc_type: "Metric".into(),
            ..Default::default()
        };
        assert_eq!(metadata_adjustment(Some(&meta), false), 0.0);

        meta.trust_tier = TrustTier::HumanReviewed;
        assert_eq!(metadata_adjustment(Some(&meta), false), 0.05);
        // Stale verified doc: +0.05 − 0.10.
        assert_eq!(metadata_adjustment(Some(&meta), true), -0.05);

        meta.status = Some("deprecated".into());
        assert_eq!(metadata_adjustment(Some(&meta), false), -0.05);

        meta.status = Some("draft".into());
        assert!((metadata_adjustment(Some(&meta), false) - 0.03).abs() < 1e-6);
    }

    #[test]
    fn stale_metadata_is_reported_and_penalized() {
        let mut index = crate::knowledge::KnowledgeIndex::default();
        // stale_after far in the past.
        let doc = document::parse_markdown(
            "kb/old.md",
            "---\ntype: Metric\nstale_after: 2000-01-01T00:00:00Z\n---\n# Metric\nBody.\n",
        );
        index.documents.insert(doc.doc_id.clone(), doc);
        let base = DocFilter {
            tag: None,
            okf_type: None,
            status: None,
            min_trust: None,
        };
        let cand = resolve_candidate(&index, "knowledge:kb/old.md#metric", &base).unwrap();
        assert!(cand.stale);
        assert_eq!(cand.metadata_adjustment, -0.10);
    }

    #[tokio::test]
    async fn background_embeds_spawn_when_dirty_and_dedupe() {
        use httpmock::prelude::*;

        let root = TempDir::new().unwrap();
        let canonical = root.path().canonicalize().unwrap();

        // One document with one non-empty section.
        let doc = document::parse_markdown("a.md", "# A\nBody.\n");
        let mut index = crate::knowledge::KnowledgeIndex::default();
        index.documents.insert(doc.doc_id.clone(), doc);

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .body(json!({"data": [{"index": 0, "embedding": [1.0]}]}).to_string());
        });

        let cfg = Arc::new(EmbedConfig {
            url: server.url("/"),
            model: "test".into(),
            headers: HeaderMap::new(),
        });

        maybe_spawn_knowledge_embeds(&canonical, &index, true, Some(Arc::clone(&cfg)));

        // Poll until the background task persisted a vector (≤10 s).
        let store_path = crate::index::format::knowledge_dir(&canonical)
            .unwrap()
            .join("embeddings.bin");
        for _ in 0..1_000 {
            if EmbedStore::load(&store_path)
                .ok()
                .is_some_and(|s| !s.vectors.is_empty())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let store = EmbedStore::load(&store_path).expect("embeddings.bin written");
        assert!(!store.vectors.is_empty());

        // A follow-up call with the store now fresh and !dirty must be a no-op
        // (no new task); it still returns cleanly either way.
        maybe_spawn_knowledge_embeds(&canonical, &index, false, Some(cfg));
    }

    #[tokio::test]
    async fn search_knowledge_ranks_lexical_hits() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join("docs/retry.md"),
            "# Retry policy\nThe client retries failed requests with exponential backoff.\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "# Project readme\nThis project is a test fixture for knowledge search.\n",
        )
        .unwrap();

        let root = dir.path().canonicalize().unwrap();
        let params = SearchKnowledgeParams {
            project: root.to_string_lossy().to_string(),
            query: "retry backoff policy".into(),
            tag: None,
            path_filter: Some("docs/".to_string()),
            limit: Some(5),
            okf_type: None,
            status: None,
            min_trust: None,
            embed_config: None,
        };
        let response = search_knowledge(params).await.unwrap();

        assert_eq!(response["results_count"], 1);
        assert_eq!(response["results"][0]["file_path"], "docs/retry.md");
        // README is excluded by the path filter even though it matches lexically.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_searches_respect_limits_after_edits() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("docs.md");
        for version in ["original", "modified"] {
            std::fs::write(
                &path,
                (0..70)
                    .map(|i| format!("# Retry {i}\nRetry {version} body.\n"))
                    .collect::<String>(),
            )
            .unwrap();
            let mut tasks = Vec::new();
            for limit in [0, 1, 8, 50, 100, 1, 8, 50] {
                let project = dir.path().to_string_lossy().to_string();
                tasks.push(tokio::spawn(async move {
                    let response = search_knowledge(SearchKnowledgeParams {
                        project,
                        query: "retry".into(),
                        tag: None,
                        path_filter: None,
                        limit: Some(limit),
                        okf_type: None,
                        status: None,
                        min_trust: None,
                        embed_config: None,
                    })
                    .await
                    .unwrap();
                    assert_eq!(response["results_count"], limit.min(50));
                    for result in response["results"].as_array().unwrap() {
                        assert!(result["snippet"].as_str().unwrap().contains(version));
                    }
                }));
            }
            for task in tasks {
                task.await.unwrap();
            }
        }
    }

    #[test]
    fn embedding_completeness_detects_partial_and_outdated_stores() {
        let doc = document::parse_markdown("a.md", "# A\nOne.\n## B\nTwo.\n");
        let mut index = crate::knowledge::KnowledgeIndex::default();
        index.documents.insert(doc.doc_id.clone(), doc.clone());
        let mut store = EmbedStore::new();
        for section in &doc.sections {
            assert!(!knowledge_embeddings_complete(&index, &store));
            let id = section.full_id(&doc.doc_id);
            store.update(id.clone(), vec![1.0]);
            store.hashes.insert(
                id,
                crate::embed::document::document_hash(&section.embedding_text(&doc)),
            );
        }
        assert!(knowledge_embeddings_complete(&index, &store));
        index.documents.get_mut(&doc.doc_id).unwrap().sections[0].content = "New body".into();
        assert!(!knowledge_embeddings_complete(&index, &store));
    }
}
