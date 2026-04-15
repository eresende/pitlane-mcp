use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::index::repo_profile::{build_repo_profile, RepoProfile};
use crate::index::SymbolIndex;
use crate::indexer::language::{Symbol, SymbolId};

/// Serializable form of index - only contains the symbols map
#[derive(Serialize, Deserialize)]
struct IndexOnDisk {
    symbols: HashMap<SymbolId, Symbol>,
}

/// Metadata stored alongside the index
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IndexMeta {
    pub project_path: String,
    pub version: u32,
    pub indexed_at: String,
    pub file_mtimes: HashMap<String, u64>,
    pub repo_profile: RepoProfile,
}

impl IndexMeta {
    pub fn new(project_path: &Path) -> Self {
        Self {
            project_path: project_path.display().to_string(),
            version: 2,
            indexed_at: chrono_now(),
            file_mtimes: HashMap::new(),
            repo_profile: RepoProfile::default(),
        }
    }
}

pub fn build_index_meta(project_path: &Path, index: &SymbolIndex) -> IndexMeta {
    let mut meta = IndexMeta::new(project_path);
    meta.repo_profile = build_repo_profile(project_path, index);
    meta
}

pub fn load_project_meta(project_path: &Path) -> anyhow::Result<IndexMeta> {
    let idx_dir = index_dir(project_path)?;
    let meta_path = idx_dir.join("meta.json");
    load_meta(&meta_path)
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple ISO-like timestamp
    format!("{}", secs)
}

pub fn save_index(index: &SymbolIndex, index_path: &Path) -> anyhow::Result<()> {
    let on_disk = IndexOnDisk {
        symbols: index.symbols.clone(),
    };

    let encoded = bincode::serde::encode_to_vec(&on_disk, bincode::config::standard())
        .context("Failed to encode index")?;

    // Atomic write: write to temp file then rename
    let tmp_path = index_path.with_extension("bin.tmp");
    std::fs::write(&tmp_path, &encoded)?;
    std::fs::rename(&tmp_path, index_path)?;

    Ok(())
}

pub fn load_index(index_path: &Path) -> anyhow::Result<SymbolIndex> {
    let bytes = std::fs::read(index_path).context("Failed to read index file")?;

    let (on_disk, _): (IndexOnDisk, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .context("Failed to decode index")?;

    let mut index = SymbolIndex {
        symbols: on_disk.symbols,
        ..Default::default()
    };
    index.rebuild_secondary_indexes();

    Ok(index)
}

pub fn save_meta(meta: &IndexMeta, meta_path: &Path) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(meta)?;
    let tmp_path = meta_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    std::fs::rename(&tmp_path, meta_path)?;
    Ok(())
}

pub fn load_meta(meta_path: &Path) -> anyhow::Result<IndexMeta> {
    let bytes = std::fs::read(meta_path)?;
    let meta: IndexMeta = serde_json::from_slice(&bytes)?;
    Ok(meta)
}

pub fn project_hash(canonical_path: &Path) -> String {
    let path_bytes = canonical_path.to_string_lossy();
    let hash = blake3::hash(path_bytes.as_bytes());
    hash.to_hex().to_string()
}

pub fn index_dir(project_path: &Path) -> anyhow::Result<std::path::PathBuf> {
    let canonical = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.to_path_buf());
    let hash = project_hash(&canonical);
    let home = dirs_home()?;
    Ok(home.join(".pitlane").join("indexes").join(hash))
}

fn dirs_home() -> anyhow::Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("USERPROFILE").map(std::path::PathBuf::from))
        .context("Cannot determine home directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SymbolIndex;
    use crate::indexer::language::{make_symbol_id, Language, Symbol, SymbolKind};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_test_symbol(name: &str) -> Symbol {
        let path = std::path::PathBuf::from("src/foo.rs");
        let id = make_symbol_id(&path, name, &SymbolKind::Function);
        Symbol {
            id,
            name: name.to_string(),
            qualified: name.to_string(),
            kind: SymbolKind::Function,
            language: Language::Rust,
            file: Arc::new(path),
            byte_start: 0,
            byte_end: 10,
            line_start: 1,
            line_end: 2,
            signature: Some(format!("fn {name}()")),
            doc: None,
        }
    }

    #[test]
    fn test_save_load_index_roundtrip() {
        let dir = TempDir::new().unwrap();
        let index_path = dir.path().join("index.bin");

        let mut index = SymbolIndex::new();
        let sym1 = make_test_symbol("hello");
        let sym2 = make_test_symbol("world");
        let id1 = sym1.id.clone();
        let id2 = sym2.id.clone();
        index.insert(sym1);
        index.insert(sym2);

        save_index(&index, &index_path).unwrap();
        let loaded = load_index(&index_path).unwrap();

        assert_eq!(loaded.symbol_count(), 2);
        assert!(loaded.symbols.contains_key(&id1));
        assert!(loaded.symbols.contains_key(&id2));
    }

    #[test]
    fn test_load_index_rebuilds_secondary_indexes() {
        let dir = TempDir::new().unwrap();
        let index_path = dir.path().join("index.bin");

        let mut index = SymbolIndex::new();
        index.insert(make_test_symbol("foo"));

        save_index(&index, &index_path).unwrap();
        let loaded = load_index(&index_path).unwrap();

        assert_eq!(loaded.file_count(), 1);
        assert_eq!(loaded.by_kind[&SymbolKind::Function].len(), 1);
    }

    #[test]
    fn test_save_load_meta_roundtrip() {
        let dir = TempDir::new().unwrap();
        let meta_path = dir.path().join("meta.json");

        let mut meta = IndexMeta::new(dir.path());
        meta.repo_profile.archetype = crate::index::repo_profile::RepoArchetype::Cli;
        meta.file_mtimes
            .insert("src/foo.rs".to_string(), 1_700_000_000);

        save_meta(&meta, &meta_path).unwrap();
        let loaded = load_meta(&meta_path).unwrap();

        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.file_mtimes["src/foo.rs"], 1_700_000_000);
    }

    #[test]
    fn test_project_hash_is_deterministic() {
        let path = Path::new("/some/project/path");
        let h1 = project_hash(path);
        let h2 = project_hash(path);
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn test_project_hash_differs_by_path() {
        let h1 = project_hash(Path::new("/project/alpha"));
        let h2 = project_hash(Path::new("/project/beta"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_load_index_missing_file_errors() {
        let dir = TempDir::new().unwrap();
        let result = load_index(&dir.path().join("nonexistent.bin"));
        assert!(result.is_err());
    }
}
