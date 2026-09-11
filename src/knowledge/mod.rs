//! Knowledge-base indexing (issue #118, Phase 1).
//!
//! A generic content-source abstraction with a filesystem Markdown source.
//! Documents are parsed into heading-aware sections (see [`document`]) and
//! persisted per project under `<index_dir>/knowledge/` alongside the code
//! index. Code and knowledge indexing evolve independently: this module owns
//! its own walk, persistence format, and BM25 schema.

pub mod document;

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::Path;
use std::time::SystemTime;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::indexer::extra_excluded_dir_names;
use crate::path_policy::{read_regular_file, regular_file_metadata};

/// Files larger than this are skipped (same limit as code indexing).
const MAX_KNOWLEDGE_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

/// Bump when the persisted knowledge layout changes incompatibly.
pub const KNOWLEDGE_META_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Content sources
// ---------------------------------------------------------------------------

/// A source of knowledge content. Phase 1 ships [`MarkdownSource`]; later
/// phases can add OKF/Confluence-backed sources without touching the indexing
/// core.
pub trait ContentSource {
    /// Stable name, e.g. `"markdown"`.
    fn name(&self) -> &'static str;

    /// File extensions this source handles (lowercase, no dot).
    fn extensions(&self) -> &[&str];

    /// Parse raw file text into a knowledge document. `relative_path` uses
    /// forward slashes and is relative to the project root.
    fn parse(&self, relative_path: &str, text: &str)
        -> anyhow::Result<document::KnowledgeDocument>;
}

/// Filesystem Markdown source (`.md` / `.markdown`).
#[derive(Default)]
pub struct MarkdownSource;

impl ContentSource for MarkdownSource {
    fn name(&self) -> &'static str {
        "markdown"
    }

    fn extensions(&self) -> &[&str] {
        &["md", "markdown"]
    }

    fn parse(
        &self,
        relative_path: &str,
        text: &str,
    ) -> anyhow::Result<document::KnowledgeDocument> {
        Ok(document::parse_markdown(relative_path, text))
    }
}

// ---------------------------------------------------------------------------
// Persisted state
// ---------------------------------------------------------------------------

/// Per-file state used for incremental re-indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    /// mtime in seconds since the Unix epoch (0 when unavailable).
    mtime: u64,
    size: u64,
    /// blake3 (first 8 bytes, LE) of the file content — catches edits that
    /// preserve mtime/size.
    hash: u64,
}

/// Persisted knowledge index metadata (`<knowledge_dir>/meta.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeIndexMeta {
    pub format_version: u32,
    /// Per-file state keyed by project-relative path (forward slashes).
    #[serde(default)]
    pub files: HashMap<String, FileState>,
}

impl KnowledgeIndexMeta {
    fn new() -> Self {
        Self {
            format_version: KNOWLEDGE_META_VERSION,
            files: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// In-memory index
// ---------------------------------------------------------------------------

/// In-memory knowledge index for one project.
#[derive(Debug, Clone)]
pub struct KnowledgeIndex {
    /// Documents keyed by `doc_id` (`knowledge:<relative/path>`).
    pub documents: HashMap<String, document::KnowledgeDocument>,
    pub meta: KnowledgeIndexMeta,
}

impl Default for KnowledgeIndex {
    fn default() -> Self {
        Self {
            documents: HashMap::new(),
            meta: KnowledgeIndexMeta::new(),
        }
    }
}

/// Outcome of an incremental indexing run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexResult {
    /// Documents (re)parsed this run.
    pub parsed: usize,
    /// Documents dropped because their file is gone or no longer eligible.
    pub removed: usize,
}

impl IndexResult {
    pub fn anything_changed(&self) -> bool {
        self.parsed > 0 || self.removed > 0
    }
}

impl KnowledgeIndex {
    /// Load documents and metadata from `knowledge_dir` (empty when absent or
    /// corrupt). A version mismatch starts fresh.
    pub fn load(knowledge_dir: &Path) -> anyhow::Result<Self> {
        let mut index = Self::default();

        let docs_path = knowledge_dir.join("documents.bin");
        if docs_path.exists() {
            let bytes = std::fs::read(&docs_path)?;
            match bincode::serde::decode_from_slice::<HashMap<String, document::KnowledgeDocument>, _>(
                &bytes,
                bincode::config::standard(),
            ) {
                Ok((documents, _)) => index.documents = documents,
                Err(e) => tracing::warn!(
                    path = %docs_path.display(),
                    error = %e,
                    "knowledge documents.bin corrupt — starting empty"
                ),
            }
        }

        let meta_path = knowledge_dir.join("meta.json");
        if meta_path.exists() {
            let bytes = std::fs::read(&meta_path)?;
            match serde_json::from_slice::<KnowledgeIndexMeta>(&bytes) {
                Ok(meta) if meta.format_version == KNOWLEDGE_META_VERSION => index.meta = meta,
                Ok(_) => tracing::warn!(
                    path = %meta_path.display(),
                    "knowledge meta.json version mismatch — starting fresh"
                ),
                Err(e) => tracing::warn!(
                    path = %meta_path.display(),
                    error = %e,
                    "knowledge meta.json corrupt — starting fresh"
                ),
            }
        }

        Ok(index)
    }

    /// Persist documents and metadata (atomic tmp+rename).
    pub fn save(&self, knowledge_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(knowledge_dir)?;
        let bytes = bincode::serde::encode_to_vec(&self.documents, bincode::config::standard())?;
        atomic_write(&knowledge_dir.join("documents.bin"), &bytes)?;
        let meta = serde_json::to_vec_pretty(&self.meta)?;
        atomic_write(&knowledge_dir.join("meta.json"), &meta)?;
        Ok(())
    }

    /// Incrementally index all files handled by `source` under `root`.
    ///
    /// Files whose mtime and size match the persisted state are not re-read;
    /// a content hash catches edits that preserve both. Unchanged documents
    /// are kept in memory from the previous load.
    pub fn index_project(
        &mut self,
        root: &Path,
        source: &dyn ContentSource,
    ) -> anyhow::Result<IndexResult> {
        let exclude_set = build_knowledge_exclude_set(root)?;
        let extra_dirs = extra_excluded_dir_names();
        let extensions: HashSet<&str> = source.extensions().iter().copied().collect();

        let mut current_files: HashMap<String, FileState> = HashMap::new();
        let mut parsed = 0usize;

        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| should_descend(root, &exclude_set, &extra_dirs, entry))
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    crate::indexer::warn_walkdir_error(root, &err, "knowledge indexing");
                    continue;
                }
            };
            let path = entry.path();
            if !entry.file_type().is_file() {
                continue;
            }
            // regular_file_metadata rejects symlinks and non-regular files.
            let Some(metadata) = regular_file_metadata(path)? else {
                continue;
            };
            if metadata.len() > MAX_KNOWLEDGE_FILE_BYTES {
                tracing::warn!(file = %path.display(), limit = MAX_KNOWLEDGE_FILE_BYTES, "skipping oversized knowledge file");
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !extensions.contains(ext) {
                continue;
            }
            let rel = path.strip_prefix(root).unwrap_or(path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if exclude_set.is_match(rel_str.as_str()) || exclude_set.is_match(path) {
                continue;
            }

            let mtime = mtime_secs(metadata.modified().ok());
            let size = metadata.len();

            // Fast path: mtime + size unchanged → keep the existing document.
            if let Some(state) = self.meta.files.get(&rel_str) {
                if state.mtime == mtime && state.size == size {
                    current_files.insert(rel_str, state.clone());
                    continue;
                }
            }

            let bytes = read_regular_file(path)?;
            let Ok(text) = std::str::from_utf8(&bytes) else {
                tracing::warn!(file = %rel_str, "skipping non-UTF-8 knowledge file");
                continue;
            };
            let hash = crate::embed::document::document_hash(text);

            // Slow path: mtime/size drifted — the hash decides.
            if let Some(state) = self.meta.files.get(&rel_str) {
                if state.hash == hash {
                    current_files.insert(rel_str, FileState { mtime, size, hash });
                    continue;
                }
            }

            match source.parse(&rel_str, text) {
                Ok(doc) => {
                    parsed += 1;
                    self.documents.insert(doc.doc_id.clone(), doc);
                }
                Err(e) => {
                    tracing::warn!(file = %rel_str, error = %e, "skipping unparseable knowledge file")
                }
            }
            current_files.insert(rel_str, FileState { mtime, size, hash });
        }

        // Drop documents whose files are gone or no longer eligible.
        let mut removed = 0usize;
        self.documents.retain(|doc_id, _| {
            let rel = doc_id.strip_prefix("knowledge:").unwrap_or(doc_id);
            if current_files.contains_key(rel) {
                return true;
            }
            removed += 1;
            false
        });

        self.meta.files = current_files;
        Ok(IndexResult { parsed, removed })
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn mtime_secs(modified: Option<SystemTime>) -> u64 {
    modified
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Exclude set for knowledge walks: default patterns + `.gitignore` + saved
/// per-project excludes (same policy as code search, owned here so the two
/// indexers can evolve independently).
fn build_knowledge_exclude_set(root: &Path) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in crate::indexer::default_exclude_patterns()
        .into_iter()
        .chain(crate::indexer::load_gitignore_patterns(root))
        .chain(
            crate::index::format::load_project_meta(root)
                .map(|meta| meta.effective_excludes)
                .unwrap_or_default(),
        )
    {
        builder.add(Glob::new(&pattern)?);
    }
    Ok(builder.build()?)
}

/// Mirror of the code-index walk's descent filter: skip directories matched by
/// the exclude set or named in the excluded-directory list.
fn should_descend(
    root: &Path,
    exclude_set: &GlobSet,
    extra_excluded_dirs: &HashSet<String>,
    entry: &walkdir::DirEntry,
) -> bool {
    let path = entry.path();
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return true,
    };
    if rel == Path::new("") {
        return true;
    }
    let rel_str = rel.to_string_lossy();
    if exclude_set.is_match(rel_str.as_ref()) {
        return false;
    }
    if rel.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|name| {
            crate::indexer::is_excluded_dir_name_with_custom(name, extra_excluded_dirs)
        })
    }) {
        return false;
    }
    if entry.file_type().is_dir() && exclude_set.is_match(format!("{rel_str}/").as_str()) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::document::parse_markdown;
    use std::fs::File;
    use std::path::PathBuf;

    fn temp_project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        for (file, source) in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let canonical = dir.path().canonicalize().unwrap();
        (dir, canonical)
    }

    #[test]
    fn markdown_source_implements_trait() {
        let source = MarkdownSource;
        assert_eq!(source.name(), "markdown");
        assert_eq!(source.extensions(), &["md", "markdown"]);
        let doc = source.parse("a.md", "# T\nBody.\n").unwrap();
        assert_eq!(doc.doc_id, "knowledge:a.md");
        assert_eq!(parse_markdown("a.md", "# T\nBody.\n"), doc);
    }

    #[test]
    fn index_project_discovers_and_filters_files() {
        let (_dir, root) = temp_project(&[
            ("docs/keep.md", "# Keep\nContent.\n"),
            ("notes.markdown", "Preamble only.\n"),
            ("node_modules/skip.md", "# Skip\n"),
            ("large.md", &format!("# Large\n{}", "x".repeat(1024 * 1024))),
            ("binary.md", ""),
        ]);
        std::fs::write(root.join("binary.md"), [0xff, 0xfe]).unwrap();

        let mut index = KnowledgeIndex::default();
        let result = index.index_project(&root, &MarkdownSource).unwrap();
        assert_eq!(result.parsed, 2);
        assert_eq!(result.removed, 0);
        assert!(index.documents.contains_key("knowledge:docs/keep.md"));
        assert!(index.documents.contains_key("knowledge:notes.markdown"));
        assert!(!index.documents.keys().any(|k| k.contains("skip")));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let (_dir, root) = temp_project(&[("keep.md", "# Keep\n")]);
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("outside.md"), "# Outside\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("outside.md"), root.join("link.md"))
            .unwrap();

        let mut index = KnowledgeIndex::default();
        let result = index.index_project(&root, &MarkdownSource).unwrap();
        assert_eq!(result.parsed, 1);
        assert!(!index.documents.keys().any(|k| k.contains("link")));
    }

    #[test]
    fn incremental_reindex_skips_unchanged_files() {
        let (_dir, root) = temp_project(&[("a.md", "# A\nOne.\n"), ("b.md", "# B\nTwo.\n")]);

        let mut index = KnowledgeIndex::default();
        assert_eq!(
            index.index_project(&root, &MarkdownSource).unwrap(),
            IndexResult {
                parsed: 2,
                removed: 0
            }
        );

        // Second run: nothing changed.
        let result = index.index_project(&root, &MarkdownSource).unwrap();
        assert_eq!(
            result,
            IndexResult {
                parsed: 0,
                removed: 0
            }
        );

        // Modify one file → only it is reparsed.
        std::fs::write(root.join("a.md"), "# A\nOne, updated.\n").unwrap();
        let result = index.index_project(&root, &MarkdownSource).unwrap();
        assert_eq!(result.parsed, 1);
        assert!(index.documents["knowledge:a.md"]
            .sections
            .iter()
            .any(|s| s.content.contains("updated")));

        // Delete a file → its document is removed.
        std::fs::remove_file(root.join("b.md")).unwrap();
        let result = index.index_project(&root, &MarkdownSource).unwrap();
        assert_eq!(result.removed, 1);
        assert!(!index.documents.contains_key("knowledge:b.md"));
    }

    #[test]
    fn same_mtime_edit_is_caught_by_hash() {
        let (_dir, root) = temp_project(&[("a.md", "# A\nOriginal.\n")]);

        let mut index = KnowledgeIndex::default();
        index.index_project(&root, &MarkdownSource).unwrap();

        // Edit content, then restore the original mtime so only the hash can
        // detect the change.
        let path = root.join("a.md");
        let old_mtime = File::open(&path)
            .unwrap()
            .metadata()
            .unwrap()
            .modified()
            .unwrap();
        std::fs::write(&path, "# A\nEdited.\n").unwrap();
        File::open(&path).unwrap().set_modified(old_mtime).unwrap();

        let result = index.index_project(&root, &MarkdownSource).unwrap();
        assert_eq!(result.parsed, 1);
        assert!(index.documents["knowledge:a.md"]
            .sections
            .iter()
            .any(|s| s.content.contains("Edited")));
    }

    #[test]
    fn save_and_load_round_trip() {
        let (_dir, root) = temp_project(&[("docs/a.md", "# A\nBody.\n")]);
        let knowledge_dir = tempfile::tempdir().unwrap();

        let mut index = KnowledgeIndex::default();
        index.index_project(&root, &MarkdownSource).unwrap();
        index.save(knowledge_dir.path()).unwrap();

        let loaded = KnowledgeIndex::load(knowledge_dir.path()).unwrap();
        assert_eq!(loaded.documents, index.documents);
        assert_eq!(loaded.meta.files.len(), 1);
    }

    #[test]
    fn load_missing_or_corrupt_dir_returns_empty() {
        let missing = tempfile::tempdir().unwrap();
        let index = KnowledgeIndex::load(missing.path()).unwrap();
        assert!(index.documents.is_empty());

        let corrupt = tempfile::tempdir().unwrap();
        std::fs::write(corrupt.path().join("documents.bin"), b"not bincode").unwrap();
        std::fs::write(corrupt.path().join("meta.json"), b"{no json").unwrap();
        let index = KnowledgeIndex::load(corrupt.path()).unwrap();
        assert!(index.documents.is_empty());
        assert_eq!(index.meta.format_version, KNOWLEDGE_META_VERSION);
    }

    #[test]
    fn gitignore_and_saved_excludes_are_respected() {
        let (dir, root) = temp_project(&[
            ("visible.md", "# Visible\n"),
            ("hidden/secret.md", "# Secret\n"),
            ("skipped.md", "# Skipped\n"),
        ]);
        std::fs::write(dir.path().join(".gitignore"), "hidden/\n").unwrap();

        let mut meta = crate::index::format::IndexMeta::new(&root);
        meta.effective_excludes = vec!["skipped.md".into()];
        let idx_dir = crate::index::format::index_dir(&root).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        crate::index::format::save_meta(&meta, &idx_dir.join("meta.json")).unwrap();

        let mut index = KnowledgeIndex::default();
        let result = index.index_project(&root, &MarkdownSource).unwrap();
        assert_eq!(result.parsed, 1);
        assert!(index.documents.contains_key("knowledge:visible.md"));
    }
}
