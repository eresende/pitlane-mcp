//! Tantivy BM25 index over knowledge sections (issue #118, Phase 1).
//!
//! Mirrors [`crate::index::bm25`] for code symbols but indexes heading-aware
//! document sections instead. The two indexes are built and searched
//! independently so code and knowledge retrieval can evolve separately; both
//! reuse the shared `"code"` tokenizer, which also handles identifiers that
//! appear in technical documentation.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, RwLock},
};

use anyhow::Context;
use tantivy::{
    collector::TopDocs,
    directory::MmapDirectory,
    query::QueryParser,
    schema::{
        Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
    },
    Index, IndexReader, ReloadPolicy, TantivyDocument,
};

use crate::knowledge::document::KnowledgeDocument;
use crate::sync_utils::{rw_read, rw_write};

const READY_SENTINEL: &str = ".ready.v1";

struct ReaderEntry {
    index: Index,
    reader: IndexReader,
}

struct KnowledgeFields {
    section_id: Field,
    name: Field,
    hierarchy: Field,
    content: Field,
    tags: Field,
    file_path: Field,
    doc_title: Field,
}

impl KnowledgeFields {
    fn load(schema: &Schema) -> anyhow::Result<Self> {
        Ok(Self {
            section_id: schema.get_field("section_id")?,
            name: schema.get_field("name")?,
            hierarchy: schema.get_field("hierarchy")?,
            content: schema.get_field("content")?,
            tags: schema.get_field("tags")?,
            file_path: schema.get_field("file_path")?,
            doc_title: schema.get_field("doc_title")?,
        })
    }
}

static READER_CACHE: LazyLock<RwLock<HashMap<PathBuf, Arc<ReaderEntry>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn build_schema() -> Schema {
    let mut b = Schema::builder();
    // Only section_id is STORED; search fields are looked up from the
    // in-memory document map after ranking (same trade-off as the code index).
    b.add_text_field("section_id", STRING | STORED);
    b.add_text_field("name", text());
    b.add_text_field("hierarchy", text());
    b.add_text_field("content", text());
    b.add_text_field("tags", STRING);
    b.add_text_field("file_path", STRING);
    b.add_text_field("doc_title", text());
    b.build()
}

/// TEXT field options using the shared `"code"` tokenizer.
fn text() -> TextOptions {
    TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(crate::index::bm25::TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    )
}

/// Build the knowledge tantivy index from scratch into `dir`. Writes a ready
/// sentinel on success; any partial write is cleaned up first.
pub fn build(docs: &[KnowledgeDocument], dir: &Path) -> anyhow::Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let schema = build_schema();
    let directory = MmapDirectory::open(dir)?;
    let index = Index::open_or_create(directory, schema.clone())?;
    crate::index::bm25::register_tokenizer(&index);
    let mut writer = index.writer(50_000_000)?;
    let fields = KnowledgeFields::load(&schema)?;

    for doc in docs {
        for section in &doc.sections {
            let mut document = TantivyDocument::default();
            document.add_text(fields.section_id, section.full_id(&doc.doc_id));
            // The preamble has no heading of its own — the document title is
            // its best name.
            let name = if section.heading.is_empty() {
                doc.title.as_str()
            } else {
                section.heading.as_str()
            };
            document.add_text(fields.name, name);
            document.add_text(fields.hierarchy, section.hierarchy_path());
            document.add_text(fields.content, &section.content);
            for tag in &doc.tags {
                document.add_text(fields.tags, tag);
            }
            document.add_text(fields.file_path, &doc.file_path);
            document.add_text(fields.doc_title, &doc.title);
            writer.add_document(document)?;
        }
    }

    writer.commit()?;
    std::fs::write(dir.join(READY_SENTINEL), b"")?;
    Ok(())
}

/// True when a previously built BM25 index exists for `dir` (sentinel present).
pub fn is_ready(dir: &Path) -> bool {
    dir.join(READY_SENTINEL).exists()
}

/// Build only when the `.ready.v1` sentinel is absent.
pub fn ensure(docs: &[KnowledgeDocument], dir: &Path) -> anyhow::Result<()> {
    if dir.join(READY_SENTINEL).exists() {
        return Ok(());
    }
    build(docs, dir)
}

/// Mark the on-disk knowledge BM25 index stale so the next `ensure` rebuilds.
pub fn mark_stale(dir: &Path) -> anyhow::Result<()> {
    let ready_path = dir.join(READY_SENTINEL);
    match std::fs::remove_file(&ready_path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn get_or_open_reader(project: &Path, dir: &Path) -> anyhow::Result<Arc<ReaderEntry>> {
    {
        let cache = rw_read(&READER_CACHE);
        if let Some(entry) = cache.get(project) {
            return Ok(Arc::clone(entry));
        }
    }

    let directory = MmapDirectory::open(dir)
        .with_context(|| format!("opening knowledge tantivy dir {}", dir.display()))?;
    let index = Index::open(directory)?;
    crate::index::bm25::register_tokenizer(&index);
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let entry = Arc::new(ReaderEntry { index, reader });

    rw_write(&READER_CACHE).insert(project.to_path_buf(), Arc::clone(&entry));
    Ok(entry)
}

/// Evict the cached reader for `project` before rebuilding the index.
pub fn invalidate(project: &Path) {
    rw_write(&READER_CACHE).remove(project);
}

/// One ranked knowledge search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeHit {
    /// Full section ID: `knowledge:<path>#<slug>`.
    pub section_id: String,
    /// BM25 score (higher is better; not comparable across queries).
    pub score: f32,
}

/// Search the knowledge index, returning sections in relevance order.
///
/// Uses OR semantics (any query term may match) — natural-language queries
/// are less likely to have every term in a single section than code queries.
pub fn search(
    query_str: &str,
    project: &Path,
    dir: &Path,
    fetch: usize,
) -> anyhow::Result<Vec<KnowledgeHit>> {
    if fetch == 0 || query_str.trim().is_empty() {
        return Ok(Vec::new());
    }

    let entry = get_or_open_reader(project, dir)?;
    let searcher = entry.reader.searcher();
    let fields = KnowledgeFields::load(searcher.schema())?;

    let mut parser = QueryParser::for_index(
        &entry.index,
        vec![
            fields.name,
            fields.hierarchy,
            fields.content,
            fields.doc_title,
        ],
    );
    // Name and hierarchy are the strongest signals for a section.
    parser.set_field_boost(fields.name, 2.0);
    parser.set_field_boost(fields.hierarchy, 1.5);
    parser.set_field_boost(fields.doc_title, 1.2);

    // Escape first (natural-language queries often contain apostrophes and
    // colons); fall back to the raw query if parsing still fails.
    let escaped = crate::index::bm25::escape_query(query_str);
    let query = parser
        .parse_query(&escaped)
        .or_else(|_| parser.parse_query(query_str))
        .with_context(|| format!("failed to parse knowledge BM25 query: {:?}", query_str))?;
    let top_docs = searcher.search(&query, &TopDocs::with_limit(fetch).order_by_score())?;
    let mut hits = Vec::with_capacity(top_docs.len());
    for (score, doc_address) in top_docs {
        let stored: TantivyDocument = searcher.doc(doc_address)?;
        if let Some(v) = stored.get_first(fields.section_id) {
            if let Some(section_id) = v.as_str() {
                hits.push(KnowledgeHit {
                    section_id: section_id.to_string(),
                    score,
                });
            }
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::document::parse_markdown;

    fn sample_docs() -> Vec<KnowledgeDocument> {
        vec![
            parse_markdown(
                "docs/setup.md",
                "---\ntitle: Setup Guide\ntags: [ops]\n---\n# Setup\nInstall the server.\n## Retry policy\nConfigure `retry_count` for transient failures.\n",
            ),
            parse_markdown("notes.md", "Random notes about billing.\n"),
        ]
    }

    #[test]
    fn build_and_search_rank_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tantivy");
        let docs = sample_docs();
        build(&docs, &dir).unwrap();
        assert!(dir.join(READY_SENTINEL).exists());

        // Rebuild idempotency via ensure: sentinel present → no-op.
        ensure(&docs, &dir).unwrap();

        let project = tmp.path();
        let hits = search("retry_count", project, &dir, 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(
            hits[0].section_id,
            "knowledge:docs/setup.md#setup-retry-policy"
        );

        let hits = search("billing", project, &dir, 5).unwrap();
        assert_eq!(hits[0].section_id, "knowledge:notes.md#preamble");

        // OR semantics: one matching term is enough.
        let hits = search("nonexistentterm billing", project, &dir, 5).unwrap();
        assert!(!hits.is_empty());

        invalidate(project);
    }

    #[test]
    fn mark_stale_forces_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tantivy");
        let docs = sample_docs();
        build(&docs, &dir).unwrap();
        mark_stale(&dir).unwrap();
        assert!(!dir.join(READY_SENTINEL).exists());
        ensure(&docs, &dir).unwrap();
        assert!(dir.join(READY_SENTINEL).exists());
    }

    #[test]
    fn search_empty_query_returns_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tantivy");
        build(&sample_docs(), &dir).unwrap();
        assert!(search("   ", tmp.path(), &dir, 5).unwrap().is_empty());
        assert!(search("", tmp.path(), &dir, 0).unwrap().is_empty());
    }
}
