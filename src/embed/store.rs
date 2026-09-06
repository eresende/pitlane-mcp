use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::indexer::language::SymbolId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbedStoreMetadata {
    pub format_version: u32,
    pub model: String,
    pub dimension: usize,
    pub document_fingerprint: String,
    pub endpoint_fingerprint: String,
}

impl EmbedStoreMetadata {
    pub fn path(store_path: &Path) -> std::path::PathBuf {
        store_path.with_extension("meta.json")
    }

    pub fn load(store_path: &Path) -> anyhow::Result<Option<Self>> {
        let path = Self::path(store_path);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
    }

    pub fn save(&self, store_path: &Path) -> anyhow::Result<()> {
        let path = Self::path(store_path);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn is_compatible(
        &self,
        model: &str,
        fingerprint: &str,
        endpoint_fingerprint: &str,
    ) -> bool {
        self.format_version == crate::embed::document::DOCUMENT_FORMAT_VERSION
            && self.model == model
            && self.document_fingerprint == fingerprint
            && self.endpoint_fingerprint == endpoint_fingerprint
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct EmbedStore {
    pub vectors: HashMap<SymbolId, Vec<f32>>,
    /// blake3 (first 8 bytes, LE) of the embedding document each vector was
    /// computed from. A stable symbol ID does NOT prove the vector is current:
    /// when the document changes (docs, body) the vector must be regenerated.
    #[serde(default)]
    pub hashes: HashMap<SymbolId, u64>,
}

impl EmbedStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let bytes = std::fs::read(path)?;
        match bincode::serde::decode_from_slice::<Self, _>(&bytes, bincode::config::standard()) {
            Ok((store, _)) => Ok(store),
            Err(e) => {
                tracing::warn!("EmbedStore: corrupt cache at {}: {e}", path.display());
                Ok(Self::new())
            }
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let bytes = bincode::serde::encode_to_vec(self, bincode::config::standard())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn update(&mut self, id: SymbolId, vec: Vec<f32>) {
        self.vectors.insert(id, vec);
    }

    pub fn remove_ids(&mut self, ids: &HashSet<SymbolId>) {
        self.vectors.retain(|k, _| !ids.contains(k));
        self.hashes.retain(|k, _| !ids.contains(k));
    }

    /// Keep only entries whose ID is in `keep`; drops vectors and document
    /// hashes for symbols that no longer exist in the index.
    pub fn retain_ids(&mut self, keep: &HashSet<SymbolId>) {
        self.vectors.retain(|k, _| keep.contains(k));
        self.hashes.retain(|k, _| keep.contains(k));
    }

    pub fn dimension(&self) -> Option<usize> {
        self.vectors.values().next().map(|v| v.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::collection::{hash_map, vec};
    use proptest::prelude::*;

    // --- Unit tests for EmbedStore edge cases (Requirements 4a.3, 6.2) ---

    #[test]
    fn load_corrupt_file_returns_empty_store() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"this is not valid bincode garbage \xff\xfe").unwrap();
        let store = EmbedStore::load(tmp.path()).unwrap();
        assert!(store.vectors.is_empty());
    }

    #[test]
    fn remove_ids_removes_specified_and_leaves_others() {
        let mut store = EmbedStore::new();
        store.update("a".to_string(), vec![1.0]);
        store.update("b".to_string(), vec![2.0]);
        store.update("c".to_string(), vec![3.0]);

        let to_remove: HashSet<SymbolId> = ["a".to_string(), "c".to_string()].into();
        store.remove_ids(&to_remove);

        assert!(!store.vectors.contains_key("a"));
        assert!(!store.vectors.contains_key("c"));
        assert!(store.vectors.contains_key("b"));
        assert_eq!(store.vectors.len(), 1);
    }

    #[test]
    fn dimension_returns_none_for_empty_store() {
        let store = EmbedStore::new();
        assert_eq!(store.dimension(), None);
    }

    #[test]
    fn dimension_returns_correct_dim_for_non_empty_store() {
        let mut store = EmbedStore::new();
        store.update("sym".to_string(), vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(store.dimension(), Some(4));
    }

    #[test]
    fn retain_ids_drops_vectors_and_hashes_for_deleted_symbols() {
        let mut store = EmbedStore::new();
        store.update("a".to_string(), vec![1.0]);
        store.update("b".to_string(), vec![2.0]);
        store.hashes.insert("a".to_string(), 10);
        store.hashes.insert("b".to_string(), 20);

        let keep: HashSet<SymbolId> = ["b".to_string()].into();
        store.retain_ids(&keep);

        assert!(!store.vectors.contains_key("a"));
        assert!(!store.hashes.contains_key("a"));
        assert!(store.vectors.contains_key("b"));
        assert_eq!(store.hashes.get("b"), Some(&20));
    }

    #[test]
    fn hashes_survive_serialisation_round_trip() {
        let mut store = EmbedStore::new();
        store.update("a".to_string(), vec![1.0]);
        store.hashes.insert("a".to_string(), 0xDEADBEEF);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        store.save(tmp.path()).unwrap();
        let loaded = EmbedStore::load(tmp.path()).unwrap();

        assert_eq!(loaded.hashes.get("a"), Some(&0xDEADBEEF));
    }

    #[test]
    fn remove_ids_also_removes_document_hashes() {
        let mut store = EmbedStore::new();
        store.update("a".to_string(), vec![1.0]);
        store.hashes.insert("a".to_string(), 7);

        let remove: HashSet<SymbolId> = ["a".to_string()].into();
        store.remove_ids(&remove);

        assert!(store.vectors.is_empty());
        assert!(store.hashes.is_empty());
    }

    #[test]
    fn metadata_rejects_model_or_document_changes() {
        let metadata = EmbedStoreMetadata {
            format_version: crate::embed::document::DOCUMENT_FORMAT_VERSION,
            model: "model-a".into(),
            dimension: 768,
            document_fingerprint: "profile-a".into(),
            endpoint_fingerprint: "endpoint-a".into(),
        };
        assert!(metadata.is_compatible("model-a", "profile-a", "endpoint-a"));
        assert!(!metadata.is_compatible("model-b", "profile-a", "endpoint-a"));
        assert!(!metadata.is_compatible("model-a", "profile-b", "endpoint-a"));
        assert!(!metadata.is_compatible("model-a", "profile-a", "endpoint-b"));
    }

    // --- Property tests ---

    // Feature: ollama-lmstudio-embeddings, Property 10: Watcher update removes deleted symbols from store
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_watcher_remove_ids_removes_deleted_and_preserves_others(
            vectors in hash_map(
                "[a-zA-Z0-9_:]{1,32}",
                vec(proptest::num::f32::NORMAL, 0..=8usize),
                0..=20usize,
            ),
            remove_indices in proptest::collection::hash_set(0..20usize, 0..=20usize),
        ) {
            // Validates: Requirements 7.2
            let keys: Vec<SymbolId> = vectors.keys().cloned().collect();

            let ids_to_remove: HashSet<SymbolId> = remove_indices
                .iter()
                .filter_map(|&i| keys.get(i).cloned())
                .collect();

            let mut store = EmbedStore { vectors: vectors.clone(), hashes: HashMap::new() };
            store.remove_ids(&ids_to_remove);

            // None of the removed IDs should remain
            for id in &ids_to_remove {
                prop_assert!(!store.vectors.contains_key(id),
                    "removed id {:?} still present in store", id);
            }

            // All IDs not in the remove set should be unchanged
            for (id, vec) in &vectors {
                if !ids_to_remove.contains(id) {
                    prop_assert_eq!(store.vectors.get(id), Some(vec),
                        "id {:?} was unexpectedly modified or removed", id);
                }
            }
        }
    }

    // Feature: ollama-lmstudio-embeddings, Property 2: EmbedStore serialisation round-trip
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_embed_store_serialisation_round_trip(
            vectors in hash_map(
                // SymbolId is a type alias for String — generate non-empty strings
                "[a-zA-Z0-9_:]{1,32}",
                vec(proptest::num::f32::NORMAL, 0..=16usize),
                0..=20usize,
            )
        ) {
            // Validates: Requirements 6.1, 6.2
            let store = EmbedStore { vectors: vectors.clone(), hashes: HashMap::new() };

            let tmp = tempfile::NamedTempFile::new().unwrap();
            let path = tmp.path();

            store.save(path).unwrap();
            let loaded = EmbedStore::load(path).unwrap();

            prop_assert_eq!(loaded.vectors, vectors);
        }
    }
}
