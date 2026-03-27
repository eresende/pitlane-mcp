use std::path::Path;
use std::collections::HashMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};

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
}

impl IndexMeta {
    pub fn new(project_path: &Path) -> Self {
        Self {
            project_path: project_path.display().to_string(),
            version: 1,
            indexed_at: chrono_now(),
            file_mtimes: HashMap::new(),
        }
    }
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
    let bytes = std::fs::read(index_path)
        .context("Failed to read index file")?;

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
    let canonical = project_path.canonicalize()
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
