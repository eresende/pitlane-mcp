pub mod client;
pub mod progress;
pub mod store;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;

use crate::index::SymbolIndex;
use crate::indexer::language::{Symbol, SymbolId};

use client::{effective_batch_size, EmbedClient, MAX_CONCURRENCY};
use store::EmbedStore;

/// Result returned by `generate_embeddings` and `update_embeddings_for_files`.
pub struct EmbedResult {
    pub stored: usize,
    pub skipped: usize,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

/// Build the text representation of a symbol used as input to the embedding model.
///
/// Always includes `name` and `qualified`; appends `signature` and `doc` when present.
/// Parts are joined with `\n`.
/// Maximum character length of the text sent to the embedding model.
///
/// `nomic-embed-text` has an 8192-token context window (~4 chars/token on average).
/// We cap at 6000 chars to stay well within that limit regardless of tokeniser
/// differences, and to leave headroom when batching.
/// Override with `PITLANE_EMBED_MAX_CHARS` if your model has a different limit.
const DEFAULT_EMBED_MAX_CHARS: usize = 6000;

fn embed_max_chars() -> usize {
    std::env::var("PITLANE_EMBED_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_EMBED_MAX_CHARS)
}

pub fn symbol_text(sym: &Symbol) -> String {
    let mut parts = vec![sym.name.as_str(), sym.qualified.as_str()];
    let sig_owned;
    let doc_owned;
    if let Some(ref s) = sym.signature {
        sig_owned = s.as_str();
        parts.push(sig_owned);
    }
    if let Some(ref d) = sym.doc {
        doc_owned = d.as_str();
        parts.push(doc_owned);
    }
    let text = parts.join("\n");
    let max = embed_max_chars();
    if text.len() <= max {
        text
    } else {
        // Truncate on a char boundary to avoid splitting a multi-byte sequence.
        let truncated = text
            .char_indices()
            .take_while(|(i, _)| *i < max)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max);
        text[..truncated].to_string()
    }
}

/// Configuration for the embedding endpoint, read from environment variables
/// exactly once at startup.
///
/// Both `PITLANE_EMBED_URL` and `PITLANE_EMBED_MODEL` must be set to non-empty
/// strings for embeddings to be enabled. When either is absent or empty,
/// `from_env()` returns `None` and no embedding-related code paths are executed.
pub struct EmbedConfig {
    pub url: String,
    pub model: String,
}

impl EmbedConfig {
    /// Returns `Some(config)` when both `PITLANE_EMBED_URL` and
    /// `PITLANE_EMBED_MODEL` are set to non-empty strings, `None` otherwise.
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("PITLANE_EMBED_URL").ok()?;
        if url.is_empty() {
            return None;
        }
        let model = std::env::var("PITLANE_EMBED_MODEL").ok()?;
        if model.is_empty() {
            return None;
        }
        Some(Self { url, model })
    }
}

/// Generate embeddings for all symbols in `index` and save to `store_path`.
///
/// When `force` is true, clears any existing store before embedding.
/// Symbols already present in the store are skipped (unless `force`).
/// `progress_cb` is called after each batch with `(completed, total)` — pass `None`
/// to disable progress reporting.
/// Returns an `EmbedResult` summarising what happened.
pub async fn generate_embeddings(
    index: &SymbolIndex,
    config: &EmbedConfig,
    store_path: &Path,
    force: bool,
    progress_cb: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    project_path: Option<&PathBuf>,
) -> EmbedResult {
    let start = std::time::Instant::now();

    // 1. Load existing store (or empty if absent/corrupt)
    let mut store = EmbedStore::load(store_path).unwrap_or_default();

    // 2. If force, clear the store
    if force {
        store = EmbedStore::new();
    }

    // 3. Collect symbols to embed: those NOT already in store
    let symbols_to_embed: Vec<&Symbol> = index
        .symbols
        .values()
        .filter(|sym| force || !store.vectors.contains_key(&sym.id))
        .collect();

    let total = symbols_to_embed.len();
    let mut stored = 0usize;
    let mut skipped = 0usize;

    if total > 0 {
        tracing::info!(total, "embed: starting embedding generation");

        // Record start timestamp in the progress registry.
        if let Some(proj) = project_path {
            progress::start(proj, total);
        }

        let client = Arc::new(EmbedClient::new(Arc::new(EmbedConfig {
            url: config.url.clone(),
            model: config.model.clone(),
        })));

        // 4. Chunk into batch slices (size configurable via PITLANE_EMBED_BATCH_SIZE)
        let batch_size = effective_batch_size();
        let chunks: Vec<Vec<(String, String)>> = symbols_to_embed
            .chunks(batch_size)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|sym| (sym.id.clone(), symbol_text(sym)))
                    .collect()
            })
            .collect();

        let total_chunks = chunks.len();

        // 5. Dispatch concurrently via buffer_unordered, collecting results with
        //    chunk index so we can report progress in order.
        let chunk_futures = chunks.into_iter().enumerate().map(|(i, chunk)| {
            let client = Arc::clone(&client);
            async move {
                let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
                let ids: Vec<String> = chunk.iter().map(|(id, _)| id.clone()).collect();
                let results = client.embed_batch(&texts).await;
                (i, ids.into_iter().zip(results).collect::<Vec<_>>())
            }
        });

        let mut completed_symbols = 0usize;
        let mut stream = futures::stream::iter(chunk_futures).buffer_unordered(MAX_CONCURRENCY);

        while let Some((chunk_idx, batch)) = stream.next().await {
            let batch_size = batch.len();
            for (id, maybe_vec) in batch {
                match maybe_vec {
                    Some(vec) => {
                        // Check dimension consistency
                        if let Some(existing_dim) = store.dimension() {
                            if vec.len() != existing_dim {
                                tracing::warn!(
                                    "embed: dimension mismatch for symbol {}: got {} expected {}; skipping",
                                    id, vec.len(), existing_dim
                                );
                                skipped += 1;
                                continue;
                            }
                        }
                        store.update(id, vec);
                        stored += 1;
                    }
                    None => {
                        skipped += 1;
                    }
                }
            }
            completed_symbols += batch_size;

            // Update in-memory progress registry so wait_for_embeddings sees live numbers.
            if let Some(proj) = project_path {
                progress::set(proj, stored, total);
            }

            // Log progress every 10 batches or on the last batch
            let is_last = chunk_idx + 1 == total_chunks;
            if chunk_idx % 10 == 0 || is_last {
                let pct = (completed_symbols as f64 * 100.0 / total.max(1) as f64 * 100.0).round()
                    / 100.0;
                tracing::info!(
                    completed = completed_symbols,
                    total,
                    pct,
                    stored,
                    skipped,
                    "embed: progress"
                );
                if let Some(cb) = progress_cb {
                    cb(completed_symbols, total);
                }
            }
        }
    }

    // 7. Save store once after all batches
    let error = match store.save(store_path) {
        Ok(()) => None,
        Err(e) => {
            let msg = format!("failed to write embeddings store: {e}");
            tracing::error!("{msg}");
            Some(msg)
        }
    };

    // Mark progress as complete, recording the finish timestamp.
    if let Some(proj) = project_path {
        progress::finish(proj);
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;

    EmbedResult {
        stored,
        skipped,
        elapsed_ms,
        error,
    }
}

/// Called by the watcher after `reindex_batch`.
///
/// Re-embeds symbols belonging to `changed_files`, removes vectors for
/// symbols that no longer exist (`removed_ids`), and saves the updated store.
/// All errors are non-fatal — failures are logged via `tracing::warn!`.
pub async fn update_embeddings_for_files(
    index: &SymbolIndex,
    changed_files: &HashSet<PathBuf>,
    removed_ids: &[SymbolId],
    config: &EmbedConfig,
    store_path: &Path,
) {
    // 1. Load existing store (empty if absent or corrupt)
    let mut store = EmbedStore::load(store_path).unwrap_or_default();

    // 2. Remove vectors for deleted symbols
    let removed_set: HashSet<SymbolId> = removed_ids.iter().cloned().collect();
    store.remove_ids(&removed_set);

    // 3. Collect symbols whose file is in changed_files
    let symbols_to_embed: Vec<&Symbol> = index
        .symbols
        .values()
        .filter(|sym| changed_files.contains(sym.file.as_ref()))
        .collect();

    if !symbols_to_embed.is_empty() {
        let client = Arc::new(EmbedClient::new(Arc::new(EmbedConfig {
            url: config.url.clone(),
            model: config.model.clone(),
        })));

        // 4. Chunk into batch slices and dispatch via buffer_unordered
        let chunks: Vec<Vec<(String, String)>> = symbols_to_embed
            .chunks(effective_batch_size())
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|sym| (sym.id.clone(), symbol_text(sym)))
                    .collect()
            })
            .collect();

        let chunk_futures = chunks.into_iter().map(|chunk| {
            let client = Arc::clone(&client);
            async move {
                let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
                let ids: Vec<String> = chunk.iter().map(|(id, _)| id.clone()).collect();
                let results = client.embed_batch(&texts).await;
                ids.into_iter().zip(results).collect::<Vec<_>>()
            }
        });

        let results: Vec<Vec<(String, Option<Vec<f32>>)>> = futures::stream::iter(chunk_futures)
            .buffer_unordered(MAX_CONCURRENCY)
            .collect()
            .await;

        // 5. Update store: Some → update, None → log warn and skip
        for batch in results {
            for (id, maybe_vec) in batch {
                match maybe_vec {
                    Some(vec) => {
                        store.update(id, vec);
                    }
                    None => {
                        tracing::warn!("update_embeddings_for_files: failed to embed symbol {id}");
                    }
                }
            }
        }
    }

    // 6. Save updated store; log warn on failure (non-fatal)
    if let Err(e) = store.save(store_path) {
        tracing::warn!("update_embeddings_for_files: failed to save store: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SymbolIndex;
    use crate::indexer::language::{Language, SymbolKind};
    use proptest::option;
    use proptest::prelude::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    // ── Task 11.1: End-to-end integration test: index → embed → semantic search ──
    // Requirements: 2.1, 2.3, 5.1, 5.2
    #[tokio::test]
    async fn test_integration_index_embed_semantic_search() {
        use crate::embed::store::EmbedStore;
        use crate::index::format::index_dir;
        use crate::tools::index_project::{index_project, IndexProjectParams};
        use crate::tools::search_symbols::{search_symbols, SearchSymbolsParams};
        use httpmock::prelude::*;
        use tempfile::TempDir;

        // Create a temp project with a Rust source file
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn hello() {}\npub fn world() {}\npub struct Foo {}\n",
        )
        .unwrap();

        // Build a mock server that returns valid OpenAI-format embedding responses
        let embedding: Vec<f32> = vec![1.0_f32, 0.0_f32, 0.0_f32];
        let data_items: Vec<serde_json::Value> = (0..crate::embed::client::BATCH_SIZE)
            .map(|_| serde_json::json!({ "embedding": embedding }))
            .collect();
        let response_body = serde_json::json!({ "data": data_items }).to_string();

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let embed_config = Some(Arc::new(EmbedConfig {
            url: server.url("/"),
            model: "test".to_string(),
        }));

        // Call index_project with embeddings enabled
        let params = IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: embed_config.clone(),
        };

        let response = index_project(params).await.unwrap();

        // Embeddings run in the background — response says "running".
        assert_eq!(
            response["embeddings"],
            serde_json::json!("running"),
            "expected embeddings=running; response={response}"
        );

        // Let the background task finish.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Verify embeddings.bin was written to disk
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        let store_path = idx_dir.join("embeddings.bin");
        assert!(
            store_path.exists(),
            "embeddings.bin should exist at {store_path:?}"
        );

        // Verify the store is non-empty
        let store = EmbedStore::load(&store_path).unwrap();
        assert!(
            !store.vectors.is_empty(),
            "embeddings store should have vectors"
        );

        // Call search_symbols with mode="semantic"
        let search_params = SearchSymbolsParams {
            project: dir.path().to_string_lossy().to_string(),
            query: "hello".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: Some(10),
            offset: None,
            mode: Some("semantic".to_string()),
            embed_config,
        };

        let search_response = search_symbols(search_params).await.unwrap();

        // Verify ranked results are returned (no error, count >= 0)
        assert!(
            search_response.get("results").is_some(),
            "search response should have 'results' field"
        );
        assert!(
            search_response.get("count").is_some(),
            "search response should have 'count' field"
        );
        let count = search_response["count"].as_u64().unwrap_or(0);
        // Results may be 0 if no symbols matched the store, but no error should occur
        let _ = count;
    }

    // ── Task 11.2: Incremental skip integration test ──────────────────────────
    // Requirements: 2.6
    #[tokio::test]
    async fn test_integration_incremental_skip() {
        use crate::tools::index_project::{index_project, IndexProjectParams};
        use httpmock::prelude::*;
        use tempfile::TempDir;

        // Create a temp project
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn alpha() {}\npub fn beta() {}\n",
        )
        .unwrap();

        // Build a mock server that counts requests
        let embedding: Vec<f32> = vec![1.0_f32, 0.0_f32, 0.0_f32];
        let data_items: Vec<serde_json::Value> = (0..crate::embed::client::BATCH_SIZE)
            .map(|_| serde_json::json!({ "embedding": embedding }))
            .collect();
        let response_body = serde_json::json!({ "data": data_items }).to_string();

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let embed_config = Some(Arc::new(EmbedConfig {
            url: server.url("/"),
            model: "test".to_string(),
        }));

        // First call: force=true — must embed all symbols
        let params1 = IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: embed_config.clone(),
        };
        let resp1 = index_project(params1).await.unwrap();
        assert_eq!(resp1["embeddings"], serde_json::json!("running"));

        // Let the background task finish so the store is written.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let hits_after_first = mock.calls();
        assert!(
            hits_after_first > 0,
            "first call should issue HTTP requests"
        );

        // Second call: force=false (default) — all symbols already in store, zero HTTP requests
        let params2 = IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(false),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: embed_config.clone(),
        };
        let resp2 = index_project(params2).await.unwrap();

        // The second call may return "cached" (index up-to-date) or "indexed" with embeddings skipped.
        // Either way, zero new HTTP requests should have been issued.
        let hits_after_second = mock.calls();
        assert_eq!(
            hits_after_second, hits_after_first,
            "second call (no force) should issue zero HTTP requests; \
             hits before={hits_after_first}, hits after={hits_after_second}; \
             resp2={resp2}"
        );
    }

    // ── Task 11.3: Watcher integration test ──────────────────────────────────
    // Requirements: 7.1, 7.2
    //
    // Tests update_embeddings_for_files directly:
    //   - Creates an index with some symbols
    //   - Creates an embeddings store with those symbols
    //   - Calls update_embeddings_for_files with a changed file and some removed IDs
    //   - Asserts the store was updated correctly
    #[tokio::test]
    async fn test_integration_watcher_update_embeddings_for_files() {
        use crate::embed::store::EmbedStore;
        use crate::index::SymbolIndex;
        use crate::indexer::language::{Language, Symbol, SymbolKind};
        use httpmock::prelude::*;
        use std::collections::HashSet;
        use std::path::PathBuf;
        use std::sync::Arc;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("embeddings.bin");

        // Build a SymbolIndex with two symbols in different files
        let file_a = Arc::new(PathBuf::from("/project/a.rs"));
        let file_b = Arc::new(PathBuf::from("/project/b.rs"));

        let sym_a = Symbol {
            id: "sym_a".to_string(),
            name: "func_a".to_string(),
            qualified: "mod::func_a".to_string(),
            kind: SymbolKind::Function,
            language: Language::Rust,
            file: Arc::clone(&file_a),
            byte_start: 0,
            byte_end: 10,
            line_start: 1,
            line_end: 1,
            signature: None,
            doc: None,
        };
        let sym_b = Symbol {
            id: "sym_b".to_string(),
            name: "func_b".to_string(),
            qualified: "mod::func_b".to_string(),
            kind: SymbolKind::Function,
            language: Language::Rust,
            file: Arc::clone(&file_b),
            byte_start: 0,
            byte_end: 10,
            line_start: 1,
            line_end: 1,
            signature: None,
            doc: None,
        };

        // Build index: sym_a and sym_b are current; sym_removed is gone (not in index)
        let mut index = SymbolIndex::new();
        index.symbols.insert(sym_a.id.clone(), sym_a.clone());
        index.symbols.insert(sym_b.id.clone(), sym_b.clone());

        // Pre-populate the store with all three symbols (including the removed one)
        let old_vec = vec![0.5_f32, 0.5_f32, 0.0_f32];
        let mut store = EmbedStore::new();
        store.update("sym_a".to_string(), old_vec.clone());
        store.update("sym_b".to_string(), old_vec.clone());
        store.update("sym_removed".to_string(), old_vec.clone());
        store.save(&store_path).unwrap();

        // Mock server returns a new embedding vector for sym_a (file_a changed)
        let new_vec: Vec<f32> = vec![0.0_f32, 1.0_f32, 0.0_f32];
        let data_items: Vec<serde_json::Value> = (0..crate::embed::client::BATCH_SIZE)
            .map(|_| serde_json::json!({ "embedding": new_vec }))
            .collect();
        let response_body = serde_json::json!({ "data": data_items }).to_string();

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let config = EmbedConfig {
            url: server.url("/"),
            model: "test".to_string(),
        };

        // changed_files = {file_a}, removed_ids = ["sym_removed"]
        let mut changed_files: HashSet<PathBuf> = HashSet::new();
        changed_files.insert((*file_a).clone());

        let removed_ids: Vec<String> = vec!["sym_removed".to_string()];

        // Call update_embeddings_for_files
        update_embeddings_for_files(&index, &changed_files, &removed_ids, &config, &store_path)
            .await;

        // Load the updated store and verify
        let updated_store = EmbedStore::load(&store_path).unwrap();

        // sym_removed should be gone (Requirements 7.2)
        assert!(
            !updated_store.vectors.contains_key("sym_removed"),
            "sym_removed should have been removed from the store"
        );

        // sym_a should have been re-embedded with the new vector (Requirements 7.1)
        let sym_a_vec = updated_store
            .vectors
            .get("sym_a")
            .expect("sym_a should still be in the store");
        // The new vector [0, 1, 0] is already unit-normalised, so after normalise() it stays [0, 1, 0]
        assert!(
            (sym_a_vec[1] - 1.0_f32).abs() < 1e-5,
            "sym_a should have been re-embedded with the new vector; got {sym_a_vec:?}"
        );

        // sym_b should be unchanged (file_b was not in changed_files)
        let sym_b_vec = updated_store
            .vectors
            .get("sym_b")
            .expect("sym_b should still be in the store");
        // sym_b was inserted directly into the store (not via embed_batch), so it keeps
        // the raw old_vec = [0.5, 0.5, 0.0] without normalisation.
        assert!(
            (sym_b_vec[0] - 0.5_f32).abs() < 1e-5 && (sym_b_vec[1] - 0.5_f32).abs() < 1e-5,
            "sym_b should be unchanged; got {sym_b_vec:?}"
        );
    }

    // Feature: ollama-lmstudio-embeddings, Property 1: Symbol_Text contains all present fields
    // Validates: Requirements 2.2
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]
        #[test]
        fn symbol_text_contains_all_present_fields(
            name in "\\PC+",
            qualified in "\\PC+",
            signature in option::of("\\PC+"),
            doc in option::of("\\PC+"),
        ) {
            let sym = crate::indexer::language::Symbol {
                id: "test-id".to_string(),
                name: name.clone(),
                qualified: qualified.clone(),
                kind: SymbolKind::Function,
                language: Language::Rust,
                file: Arc::new(PathBuf::from("test.rs")),
                byte_start: 0,
                byte_end: 0,
                line_start: 1,
                line_end: 1,
                signature: signature.clone(),
                doc: doc.clone(),
            };

            let result = symbol_text(&sym);

            // name and qualified are always present
            prop_assert!(result.contains(name.as_str()));
            prop_assert!(result.contains(qualified.as_str()));

            // optional fields appear only when Some
            if let Some(ref s) = signature {
                prop_assert!(result.contains(s.as_str()));
            }
            if let Some(ref d) = doc {
                prop_assert!(result.contains(d.as_str()));
            }

            // parts are separated by \n
            let parts: Vec<&str> = result.split('\n').collect();
            prop_assert!(parts.len() >= 2);
            prop_assert_eq!(parts[0], name.as_str());
            prop_assert_eq!(parts[1], qualified.as_str());
        }
    }

    // Serialize all env-var tests to avoid races between parallel test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: run a closure with specific env vars set, restoring state after.
    /// Holds `ENV_LOCK` for the duration so tests don't race on shared env vars.
    fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        let _guard = ENV_LOCK.lock().unwrap();

        // Save originals
        let saved: Vec<(&str, Option<String>)> = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var(k).ok()))
            .collect();

        // Apply
        for (k, v) in vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }

        f();

        // Restore
        for (k, v) in &saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }

    #[test]
    fn returns_none_when_url_absent() {
        with_env(
            &[
                ("PITLANE_EMBED_URL", None),
                ("PITLANE_EMBED_MODEL", Some("nomic-embed-text")),
            ],
            || {
                assert!(EmbedConfig::from_env().is_none());
            },
        );
    }

    #[test]
    fn returns_none_when_model_absent() {
        with_env(
            &[
                (
                    "PITLANE_EMBED_URL",
                    Some("http://localhost:11434/api/embeddings"),
                ),
                ("PITLANE_EMBED_MODEL", None),
            ],
            || {
                assert!(EmbedConfig::from_env().is_none());
            },
        );
    }

    #[test]
    fn returns_none_when_url_empty() {
        with_env(
            &[
                ("PITLANE_EMBED_URL", Some("")),
                ("PITLANE_EMBED_MODEL", Some("nomic-embed-text")),
            ],
            || {
                assert!(EmbedConfig::from_env().is_none());
            },
        );
    }

    #[test]
    fn returns_none_when_model_empty() {
        with_env(
            &[
                (
                    "PITLANE_EMBED_URL",
                    Some("http://localhost:11434/api/embeddings"),
                ),
                ("PITLANE_EMBED_MODEL", Some("")),
            ],
            || {
                assert!(EmbedConfig::from_env().is_none());
            },
        );
    }

    #[test]
    fn returns_some_when_both_set() {
        with_env(
            &[
                (
                    "PITLANE_EMBED_URL",
                    Some("http://localhost:11434/api/embeddings"),
                ),
                ("PITLANE_EMBED_MODEL", Some("nomic-embed-text")),
            ],
            || {
                let cfg = EmbedConfig::from_env().expect("should be Some");
                assert_eq!(cfg.url, "http://localhost:11434/api/embeddings");
                assert_eq!(cfg.model, "nomic-embed-text");
            },
        );
    }

    #[test]
    fn reads_exactly_the_correct_env_vars() {
        // Ensure it reads PITLANE_EMBED_URL / PITLANE_EMBED_MODEL specifically
        with_env(
            &[
                (
                    "PITLANE_EMBED_URL",
                    Some("http://localhost:1234/v1/embeddings"),
                ),
                ("PITLANE_EMBED_MODEL", Some("all-minilm")),
            ],
            || {
                let cfg = EmbedConfig::from_env().expect("should be Some");
                // Values come from the correct vars
                assert_eq!(cfg.url, "http://localhost:1234/v1/embeddings");
                assert_eq!(cfg.model, "all-minilm");
            },
        );
    }

    // Feature: ollama-lmstudio-embeddings, Property 3: Batch count equals ceil(N / BATCH_SIZE)
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]
        #[test]
        /// Validates: Requirements 2a.3
        fn prop_batch_count(n in 1usize..=200usize) {
            use httpmock::prelude::*;
            use tempfile::tempdir;

            let batch_size = client::effective_batch_size();

            // Build a fixed OpenAI-format response body with enough embeddings
            // for the current effective batch size.
            let embedding: Vec<f32> = vec![1.0_f32, 0.0_f32, 0.0_f32];
            let data_items: Vec<serde_json::Value> = (0..batch_size)
                .map(|_| serde_json::json!({ "embedding": embedding }))
                .collect();
            let response_body = serde_json::json!({ "data": data_items }).to_string();

            // Start mock server and register a catch-all POST mock.
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(POST);
                then.status(200)
                    .header("content-type", "application/json")
                    .body(response_body.clone());
            });

            // Build a SymbolIndex with exactly N symbols.
            let mut index = SymbolIndex::new();
            for i in 0..n {
                let sym = crate::indexer::language::Symbol {
                    id: format!("sym-{i}"),
                    name: format!("sym{i}"),
                    qualified: format!("mod::sym{i}"),
                    kind: SymbolKind::Function,
                    language: Language::Rust,
                    file: Arc::new(PathBuf::from("test.rs")),
                    byte_start: 0,
                    byte_end: 0,
                    line_start: 1,
                    line_end: 1,
                    signature: None,
                    doc: None,
                };
                index.symbols.insert(sym.id.clone(), sym);
            }

            let config = EmbedConfig {
                url: server.url("/"),
                model: "test-model".to_string(),
            };

            let dir = tempdir().expect("tempdir");
            let store_path = dir.path().join("embeddings.bin");

            // Run generate_embeddings synchronously via a Tokio runtime.
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(async {
                generate_embeddings(&index, &config, &store_path, true, None, None).await;
            });

            // Assert exactly ceil(N / effective batch size) HTTP requests were issued.
            let expected_batches = n.div_ceil(batch_size);
            let actual_hits = mock.calls();
            prop_assert_eq!(
                actual_hits,
                expected_batches,
                "N={}: expected {} batch requests, got {}",
                n,
                expected_batches,
                actual_hits
            );
        }
    }
}
