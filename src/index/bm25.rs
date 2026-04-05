use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, RwLock},
};

use anyhow::Context;
use tantivy::{
    collector::TopDocs,
    directory::MmapDirectory,
    query::{BooleanQuery, Occur, QueryParser, TermQuery},
    schema::{IndexRecordOption, Schema, Value, STORED, STRING, TEXT},
    Index, IndexReader, ReloadPolicy, TantivyDocument, Term,
};

use crate::indexer::language::{Language, Symbol, SymbolId, SymbolKind};

struct ReaderEntry {
    index: Index,
    reader: IndexReader,
}

static READER_CACHE: LazyLock<RwLock<HashMap<PathBuf, Arc<ReaderEntry>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn build_schema() -> Schema {
    let mut b = Schema::builder();
    // Only symbol_id needs STORED — all other fields are retrieved from index.bin.
    // Omitting STORED from the search fields eliminates the tantivy store files,
    // which are the main driver of disk usage on large repos.
    b.add_text_field("symbol_id", STRING | STORED);
    b.add_text_field("name", TEXT);
    b.add_text_field("qualified", TEXT);
    b.add_text_field("signature", TEXT);
    b.add_text_field("doc", TEXT);
    b.add_text_field("kind", STRING);
    b.add_text_field("language", STRING);
    b.add_text_field("file_path", STRING);
    b.build()
}

/// Build the tantivy BM25 index from scratch into `tantivy_dir`.
/// Writes a `.ready` sentinel on success. Any partial write is cleaned up
/// by removing the directory first.
pub fn build(symbols: &HashMap<SymbolId, Symbol>, tantivy_dir: &Path) -> anyhow::Result<()> {
    if tantivy_dir.exists() {
        std::fs::remove_dir_all(tantivy_dir)
            .with_context(|| format!("removing {}", tantivy_dir.display()))?;
    }
    std::fs::create_dir_all(tantivy_dir)
        .with_context(|| format!("creating {}", tantivy_dir.display()))?;

    let schema = build_schema();
    let dir = MmapDirectory::open(tantivy_dir)?;
    let index = Index::open_or_create(dir, schema.clone())?;
    let mut writer = index.writer(50_000_000)?;

    let symbol_id_f = schema.get_field("symbol_id").unwrap();
    let name_f = schema.get_field("name").unwrap();
    let qualified_f = schema.get_field("qualified").unwrap();
    let signature_f = schema.get_field("signature").unwrap();
    let doc_f = schema.get_field("doc").unwrap();
    let kind_f = schema.get_field("kind").unwrap();
    let language_f = schema.get_field("language").unwrap();
    let file_path_f = schema.get_field("file_path").unwrap();

    for sym in symbols.values() {
        let mut document = TantivyDocument::default();
        document.add_text(symbol_id_f, &sym.id);
        document.add_text(name_f, &sym.name);
        document.add_text(qualified_f, &sym.qualified);
        if let Some(ref sig) = sym.signature {
            document.add_text(signature_f, sig);
        }
        if let Some(ref d) = sym.doc {
            document.add_text(doc_f, d);
        }
        document.add_text(kind_f, sym.kind.to_string());
        document.add_text(language_f, sym.language.to_string());
        document.add_text(file_path_f, sym.file.to_string_lossy());
        writer.add_document(document)?;
    }

    writer.commit()?;
    // Sentinel written last — a missing or partial build is retried next time.
    std::fs::write(tantivy_dir.join(".ready"), b"")?;
    Ok(())
}

/// Builds the tantivy index only if the `.ready` sentinel is absent, i.e. on
/// first use after an upgrade or after a failed previous build.
pub fn ensure(symbols: &HashMap<SymbolId, Symbol>, tantivy_dir: &Path) -> anyhow::Result<()> {
    if tantivy_dir.join(".ready").exists() {
        return Ok(());
    }
    build(symbols, tantivy_dir)
}

fn get_or_open_reader(project: &Path, tantivy_dir: &Path) -> anyhow::Result<Arc<ReaderEntry>> {
    {
        let cache = READER_CACHE.read().unwrap();
        if let Some(entry) = cache.get(project) {
            return Ok(Arc::clone(entry));
        }
    }

    let dir = MmapDirectory::open(tantivy_dir)
        .with_context(|| format!("opening tantivy dir {}", tantivy_dir.display()))?;
    let index = Index::open(dir)?;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let entry = Arc::new(ReaderEntry { index, reader });

    READER_CACHE
        .write()
        .unwrap()
        .insert(project.to_path_buf(), Arc::clone(&entry));
    Ok(entry)
}

/// Evict the cached reader for `project`. Call before rebuilding the tantivy
/// index so the next search opens a fresh reader.
pub fn invalidate(project: &Path) {
    READER_CACHE.write().unwrap().remove(project);
}

/// Search the BM25 index, returning symbol IDs in relevance order.
///
/// `fetch` is the number of results to pull from tantivy. Callers should add
/// slack when a file-glob post-filter will be applied.
pub fn search(
    query_str: &str,
    project: &Path,
    tantivy_dir: &Path,
    kind_filter: Option<&SymbolKind>,
    lang_filter: Option<&Language>,
    fetch: usize,
) -> anyhow::Result<Vec<SymbolId>> {
    if fetch == 0 {
        return Ok(Vec::new());
    }

    let entry = get_or_open_reader(project, tantivy_dir)?;
    let searcher = entry.reader.searcher();
    let schema = searcher.schema();

    let name_f = schema.get_field("name").unwrap();
    let qualified_f = schema.get_field("qualified").unwrap();
    let signature_f = schema.get_field("signature").unwrap();
    let doc_f = schema.get_field("doc").unwrap();
    let kind_f = schema.get_field("kind").unwrap();
    let language_f = schema.get_field("language").unwrap();
    let symbol_id_f = schema.get_field("symbol_id").unwrap();

    let mut parser =
        QueryParser::for_index(&entry.index, vec![name_f, qualified_f, signature_f, doc_f]);
    // AND semantics: all terms must be present, reducing noise for code queries.
    parser.set_conjunction_by_default();

    let escaped = escape_query(query_str);
    let text_query = parser
        .parse_query(&escaped)
        .or_else(|_| parser.parse_query(query_str))
        .with_context(|| format!("failed to parse BM25 query: {:?}", query_str))?;

    let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = vec![(Occur::Must, text_query)];

    if let Some(kind) = kind_filter {
        let term = Term::from_field_text(kind_f, &kind.to_string());
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }
    if let Some(lang) = lang_filter {
        let term = Term::from_field_text(language_f, &lang.to_string());
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }

    let query = BooleanQuery::new(clauses);
    let top_docs = searcher.search(&query, &TopDocs::with_limit(fetch))?;

    let mut ids = Vec::with_capacity(top_docs.len());
    for (_score, doc_address) in top_docs {
        let doc: TantivyDocument = searcher.doc(doc_address)?;
        if let Some(v) = doc.get_first(symbol_id_f) {
            if let Some(id) = v.as_str() {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

fn escape_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if "+-&|!(){}[]^\"~*?:\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
