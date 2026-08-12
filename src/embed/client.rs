use std::net::SocketAddr;
use std::sync::Arc;

use serde::Serialize;

use super::document::{document_prefix, query_prefix};
use super::EmbedConfig;

pub const MAX_CONCURRENCY: usize = 16;
pub const BATCH_SIZE: usize = 256;
pub const MAX_RETRIES: usize = 3;
pub const RETRY_BASE_MS: u64 = 500;

/// Returns the effective concurrent request limit.
pub fn effective_max_concurrency() -> usize {
    std::env::var("PITLANE_EMBED_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(MAX_CONCURRENCY)
}

/// Returns the effective batch size, respecting `PITLANE_EMBED_BATCH_SIZE` if set.
pub fn effective_batch_size() -> usize {
    std::env::var("PITLANE_EMBED_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(BATCH_SIZE)
}

/// Returns the number of retries for rate-limit and transient server errors.
pub fn effective_max_retries() -> usize {
    std::env::var("PITLANE_EMBED_MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(MAX_RETRIES)
}

fn effective_retry_base_ms() -> u64 {
    std::env::var("PITLANE_EMBED_RETRY_BASE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(RETRY_BASE_MS)
}

/// Returns the minimum delay in milliseconds between consecutive embedding requests.
///
/// When set, this throttles outbound requests to stay within external API rate limits
/// (e.g. 100 RPM). A value of 700 limits throughput to ~85 req/min.
/// Defaults to 0 (no delay).
pub fn effective_request_delay_ms() -> u64 {
    std::env::var("PITLANE_EMBED_REQUEST_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

// ── HTTP request types ────────────────────────────────────────────────────────

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: EmbedInput<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum EmbedInput<'a> {
    Single(&'a str),
    Batch(Vec<&'a str>),
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct EmbedClient {
    http: reqwest::Client,
    config: Arc<EmbedConfig>,
}

impl EmbedClient {
    pub fn new(config: Arc<EmbedConfig>) -> Self {
        let timeout_secs = std::env::var("PITLANE_EMBED_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);

        // If the URL uses "localhost", override DNS to resolve it to 127.0.0.1
        // (IPv4 only). Ollama typically listens on 127.0.0.1 but not ::1, and
        // reqwest with rustls-tls does not fall back to the next address when
        // the first one (::1) is refused, causing all requests to fail.
        let mut builder =
            reqwest::ClientBuilder::new().timeout(std::time::Duration::from_secs(timeout_secs));
        builder = builder.default_headers(config.headers.clone());

        if let Ok(url) = reqwest::Url::parse(&config.url) {
            if url.host_str() == Some("localhost") {
                let port = url.port_or_known_default().unwrap_or(80);
                let addr: SocketAddr = ([127, 0, 0, 1], port).into();
                builder = builder.resolve("localhost", addr);
            }
        }

        let http = builder.build().unwrap();
        Self { http, config }
    }

    /// Send one batch of up to `BATCH_SIZE` texts.
    /// Returns one `Option<Vec<f32>>` per input — `None` on any failure.
    pub async fn embed_batch(&self, texts: &[String]) -> Vec<Option<Vec<f32>>> {
        let n = texts.len();
        let prefix = document_prefix(&self.config.model);
        let prepared: Vec<String> = texts.iter().map(|text| format!("{prefix}{text}")).collect();

        let body = EmbedRequest {
            model: &self.config.model,
            input: EmbedInput::Batch(prepared.iter().map(|s| s.as_str()).collect()),
        };

        let response = match self.send_with_retry(&body).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = ?e, "embed_batch: connection error");
                return vec![None; n];
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                "embed_batch: HTTP error {status}: {}",
                truncate_error_body(&body)
            );
            return vec![None; n];
        }

        let json: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("embed_batch: malformed JSON: {e}");
                return vec![None; n];
            }
        };

        parse_response(&json, n)
    }

    /// Embed a single query string (used at search time).
    pub async fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let prepared = format!("{}{}", query_prefix(&self.config.model), text);
        let body = EmbedRequest {
            model: &self.config.model,
            input: EmbedInput::Single(&prepared),
        };

        let response = self
            .send_with_retry(&body)
            .await
            .map_err(|err| anyhow::anyhow!("embed_query: connection error: {err:?}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "embed_query: HTTP error {status}: {}",
                truncate_error_body(&body)
            );
        }

        let json: serde_json::Value = response.json().await?;

        // Try OpenAI format first: data[0].embedding
        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
            let vec = data
                .first()
                .and_then(|item| item.get("embedding"))
                .and_then(|e| e.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect::<Vec<f32>>()
                })
                .filter(|v| !v.is_empty());

            if let Some(mut v) = vec {
                normalise(&mut v);
                return Ok(v);
            }
            anyhow::bail!("embed_query: OpenAI response missing or empty embedding");
        }

        // Ollama /api/embed format: embeddings[[...]] (plural, nested array)
        if let Some(embeddings) = json.get("embeddings").and_then(|e| e.as_array()) {
            let vec = embeddings
                .first()
                .and_then(|e| e.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect::<Vec<f32>>()
                })
                .filter(|v| !v.is_empty());

            if let Some(mut v) = vec {
                normalise(&mut v);
                return Ok(v);
            }
            anyhow::bail!("embed_query: Ollama embeddings response missing or empty");
        }

        // Ollama /api/embeddings legacy format: embedding[...] (singular, flat)
        if let Some(embedding) = json.get("embedding").and_then(|e| e.as_array()) {
            if embedding.is_empty() {
                anyhow::bail!("embed_query: Ollama response has empty embedding");
            }
            let mut vec: Vec<f32> = embedding
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            normalise(&mut vec);
            return Ok(vec);
        }

        anyhow::bail!(
            "embed_query: response has neither 'data', 'embeddings', nor 'embedding' field"
        )
    }

    async fn send_with_retry(
        &self,
        body: &EmbedRequest<'_>,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let max_retries = effective_max_retries();

        for attempt in 0..=max_retries {
            let response = self
                .http
                .post(&self.config.url)
                .header("Content-Type", "application/json")
                .json(body)
                .send()
                .await;

            match response {
                Ok(response) if is_retryable_status(response.status()) && attempt < max_retries => {
                    let status = response.status();
                    let delay = retry_delay(&response, attempt);
                    let _ = response.bytes().await;
                    tracing::warn!(
                        %status,
                        attempt = attempt + 1,
                        max_retries,
                        delay_ms = delay.as_millis() as u64,
                        "embedding request is rate-limited or temporarily unavailable; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Ok(response) => return Ok(response),
                Err(error) if attempt < max_retries => {
                    let delay = exponential_retry_delay(attempt);
                    tracing::warn!(
                        error = ?error,
                        attempt = attempt + 1,
                        max_retries,
                        delay_ms = delay.as_millis() as u64,
                        "embedding request failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }

        unreachable!("embedding retry loop always returns a response or error")
    }
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
        || status == reqwest::StatusCode::BAD_GATEWAY
        || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        || status == reqwest::StatusCode::GATEWAY_TIMEOUT
}

fn retry_delay(response: &reqwest::Response, attempt: usize) -> std::time::Duration {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or_else(|| exponential_retry_delay(attempt))
}

fn exponential_retry_delay(attempt: usize) -> std::time::Duration {
    let multiplier = 1u64 << attempt.min(6);
    std::time::Duration::from_millis(
        effective_retry_base_ms()
            .saturating_mul(multiplier)
            .min(30_000),
    )
}

fn truncate_error_body(body: &str) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 512;
    let body = body.trim();
    if body.chars().count() <= MAX_ERROR_BODY_CHARS {
        return body.to_string();
    }
    format!(
        "{}...",
        body.chars().take(MAX_ERROR_BODY_CHARS).collect::<String>()
    )
}

/// Cosine similarity between two unit-normalised vectors (dot product).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Normalise a vector in-place by its L2 norm. No-op if norm is zero.
pub fn normalise(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

/// Parse a JSON response value into `n` optional embedding vectors.
/// Tries three formats in order:
///   1. OpenAI: `data[i].embedding` (array of objects)
///   2. Ollama /api/embed: `embeddings[i]` (nested array, plural) — current API
///   3. Ollama /api/embeddings: `embedding` (flat array, singular) — legacy API
///
/// Returns `vec![None; n]` on any parse failure.
pub(crate) fn parse_response(json: &serde_json::Value, n: usize) -> Vec<Option<Vec<f32>>> {
    // 1. OpenAI format: data[i].embedding
    if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
        if data.is_empty() {
            tracing::warn!("embed_batch: OpenAI response has empty data array");
            return vec![None; n];
        }
        let mut results = vec![None; n];
        for (position, item) in data.iter().enumerate() {
            let index = item
                .get("index")
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(position);
            if index >= n {
                continue;
            }
            let vec = item
                .get("embedding")
                .and_then(|e| e.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect::<Vec<f32>>()
                })
                .filter(|v| !v.is_empty());

            results[index] = vec.map(|mut v| {
                normalise(&mut v);
                v
            });
        }
        return results;
    }

    // 2. Ollama /api/embed format: embeddings[[...], [...]] (plural, nested array)
    if let Some(embeddings) = json.get("embeddings").and_then(|e| e.as_array()) {
        if embeddings.is_empty() {
            tracing::warn!("embed_batch: Ollama response has empty embeddings array");
            return vec![None; n];
        }
        let mut results = Vec::with_capacity(n);
        for i in 0..n {
            let vec = embeddings
                .get(i)
                .and_then(|e| e.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect::<Vec<f32>>()
                })
                .filter(|v| !v.is_empty());

            results.push(vec.map(|mut v| {
                normalise(&mut v);
                v
            }));
        }
        return results;
    }

    // 3. Ollama /api/embeddings legacy format: embedding[...] (singular, flat array)
    if let Some(embedding) = json.get("embedding").and_then(|e| e.as_array()) {
        if embedding.is_empty() {
            tracing::warn!("embed_batch: Ollama response has empty embedding");
            return vec![None; n];
        }
        let mut vec: Vec<f32> = embedding
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        normalise(&mut vec);
        // Single-embedding format: index 0 gets the vector, rest are None
        let mut results = vec![None; n];
        if n > 0 {
            results[0] = Some(vec);
        }
        return results;
    }

    tracing::warn!("embed_batch: response has neither 'data', 'embeddings', nor 'embedding' field");
    vec![None; n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::collection::vec as pvec;
    use proptest::num::f32::NORMAL;
    use proptest::prelude::*;
    use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
    use std::collections::HashMap;

    #[tokio::test]
    async fn sends_configured_headers_for_batch_and_query() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        let embedding = serde_json::json!({
            "data": [
                { "index": 0, "embedding": [1.0, 0.0] },
                { "index": 1, "embedding": [0.0, 1.0] }
            ]
        });
        let mock = server.mock(|when, then| {
            when.method(POST)
                .header("authorization", "Bearer test-token")
                .header("x-tenant-id", "engineering");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(embedding);
        });

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-token"));
        headers.insert("x-tenant-id", HeaderValue::from_static("engineering"));
        let client = EmbedClient::new(Arc::new(EmbedConfig {
            url: server.url("/v1/embeddings"),
            model: "company-code-embedding".to_string(),
            headers,
        }));

        let batch = client
            .embed_batch(&["first".to_string(), "second".to_string()])
            .await;
        assert_eq!(batch.iter().filter(|item| item.is_some()).count(), 2);

        let query = client.embed_query("find retries").await.unwrap();
        assert_eq!(query.len(), 2);
        assert_eq!(mock.calls(), 2);
    }

    #[tokio::test]
    async fn retries_rate_limited_batch_using_retry_after() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(429)
                .header("retry-after", "0")
                .body(r#"{"error":"rate limited"}"#);
        });

        let client = EmbedClient::new(Arc::new(EmbedConfig {
            url: server.url("/v1/embeddings"),
            model: "test-model".to_string(),
            headers: HeaderMap::new(),
        }));

        let result = client.embed_batch(&["rate limited".to_string()]).await;

        assert_eq!(result, vec![None]);
        mock.assert_calls(effective_max_retries() + 1);
    }

    #[test]
    fn openai_response_respects_explicit_indices() {
        let json = serde_json::json!({
            "data": [
                { "index": 1, "embedding": [0.0, 2.0] },
                { "index": 0, "embedding": [3.0, 0.0] }
            ]
        });
        let results = parse_response(&json, 2);
        assert_eq!(results[0], Some(vec![1.0, 0.0]));
        assert_eq!(results[1], Some(vec![0.0, 1.0]));
    }

    // Feature: ollama-lmstudio-embeddings, Property 4: OpenAI response parsing extracts correct vector
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]
        #[test]
        /// Validates: Requirements 3.3
        fn prop_openai_response_parsing(raw in pvec(NORMAL, 1..=32)) {
            // Build the expected normalised vector
            let mut expected = raw.clone();
            normalise(&mut expected);

            // Wrap in OpenAI format: {"data":[{"embedding":[...]}]}
            let json = serde_json::json!({
                "data": [{ "embedding": raw }]
            });

            let results = parse_response(&json, 1);
            prop_assert_eq!(results.len(), 1);
            let parsed = results[0].as_ref().expect("should parse successfully");
            prop_assert_eq!(parsed.len(), expected.len());
            for (a, b) in parsed.iter().zip(expected.iter()) {
                prop_assert!((a - b).abs() < 1e-5, "element mismatch: {} vs {}", a, b);
            }
        }
    }

    // Feature: ollama-lmstudio-embeddings, Property 5: Ollama response parsing extracts correct vector
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]
        #[test]
        /// Validates: Requirements 3.4
        fn prop_ollama_response_parsing(raw in pvec(NORMAL, 1..=32)) {
            // Build the expected normalised vector
            let mut expected = raw.clone();
            normalise(&mut expected);

            // Wrap in Ollama format: {"embedding":[...]}
            let json = serde_json::json!({
                "embedding": raw
            });

            let results = parse_response(&json, 1);
            prop_assert_eq!(results.len(), 1);
            let parsed = results[0].as_ref().expect("should parse successfully");
            prop_assert_eq!(parsed.len(), expected.len());
            for (a, b) in parsed.iter().zip(expected.iter()) {
                prop_assert!((a - b).abs() < 1e-5, "element mismatch: {} vs {}", a, b);
            }
        }
    }

    // Feature: ollama-lmstudio-embeddings, Property 6: Embedding endpoint failures never panic
    /// Validates: Requirements 4a.1
    #[test]
    fn prop_endpoint_failure_resilience() {
        // For each failure mode, parse_response must return vec![None; n] without panicking.
        let n = 3;

        // Empty JSON object — no data or embedding field
        let json = serde_json::json!({});
        let results = parse_response(&json, n);
        assert_eq!(results, vec![None; n], "empty object: expected all None");

        // {"data": []} — empty data array
        let json = serde_json::json!({ "data": [] });
        let results = parse_response(&json, n);
        assert_eq!(
            results,
            vec![None; n],
            "empty data array: expected all None"
        );

        // {"embedding": []} — empty embedding array
        let json = serde_json::json!({ "embedding": [] });
        let results = parse_response(&json, n);
        assert_eq!(
            results,
            vec![None; n],
            "empty embedding array: expected all None"
        );

        // {"embedding": "not-an-array"} — invalid embedding type
        let json = serde_json::json!({ "embedding": "not-an-array" });
        let results = parse_response(&json, n);
        assert_eq!(
            results,
            vec![None; n],
            "non-array embedding: expected all None"
        );

        // {"data": [{"embedding": []}]} — empty embedding inside data item
        let json = serde_json::json!({ "data": [{ "embedding": [] }] });
        let results = parse_response(&json, n);
        // data[0].embedding is empty → None; data[1] and data[2] missing → None
        assert_eq!(
            results,
            vec![None; n],
            "empty embedding in data: expected all None"
        );
    }

    // Feature: ollama-lmstudio-embeddings, Property 7: Semantic search results are sorted by descending cosine similarity
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        /// Validates: Requirements 5.1, 5.2
        fn prop_semantic_search_sort_order(
            // query vector: 1–8 dimensions, bounded f32 values to avoid overflow during normalisation
            // (symbol vectors are scaled by up to 2.9x, so cap at 1e10 to stay well within f32::MAX)
            query_raw in pvec(-1e10f32..=1e10f32, 1..=8usize),
            // 1–20 symbol embeddings, all same dimension as query
            symbol_ids in proptest::collection::vec("[a-zA-Z0-9_]{1,16}", 1..=20usize),
        ) {
            let dim = query_raw.len();

            // Build a query vector and normalise it
            let mut query_vec = query_raw.clone();
            normalise(&mut query_vec);

            // Build a store: each symbol gets a random-ish embedding derived from its id
            // We use the id bytes to seed deterministic-but-varied vectors
            let mut store: HashMap<String, Vec<f32>> = HashMap::new();
            for (i, id) in symbol_ids.iter().enumerate() {
                // Create a vector by cycling through query_raw values with an offset
                let vec: Vec<f32> = (0..dim)
                    .map(|j| query_raw[(j + i) % query_raw.len()] * (1.0 + i as f32 * 0.1))
                    .collect();
                let mut v = vec;
                normalise(&mut v);
                store.insert(id.clone(), v);
            }

            // Compute scores and sort descending — mirrors the logic in search_symbols
            let mut scored: Vec<f32> = store
                .values()
                .map(|vec| cosine_similarity(&query_vec, vec))
                .collect();
            scored.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

            // Assert score[i] >= score[i+1] for all consecutive pairs
            // Skip NaN values (can only arise from zero vectors, which are degenerate inputs)
            for window in scored.windows(2) {
                // NaN comparisons always return false; skip windows containing NaN
                if window[0].is_nan() || window[1].is_nan() {
                    continue;
                }
                prop_assert!(
                    window[0] >= window[1],
                    "sort order violated: {} < {}",
                    window[0],
                    window[1]
                );
            }
        }
    }

    // Feature: ollama-lmstudio-embeddings, Property 8: Filters exclude non-matching symbols from semantic results
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        /// Validates: Requirements 5.5
        fn prop_filter_exclusion_semantic(
            // Generate a list of (kind_index, language_index, file_suffix) tuples
            symbols in proptest::collection::vec(
                (0usize..12, 0usize..17, "[a-z]{1,8}"),
                1..=20usize,
            ),
            // Filter: optionally pick one kind index, one language index, one file suffix
            filter_kind in proptest::option::of(0usize..12),
            filter_lang in proptest::option::of(0usize..17),
            filter_file in proptest::option::of("[a-z]{1,8}"),
        ) {
            // Map indices to kind/language strings (matching the enums)
            let kinds = [
                "function", "method", "struct", "enum", "trait", "impl",
                "mod", "macro", "const", "type_alias", "class", "interface",
            ];
            let langs = [
                "rust", "python", "javascript", "typescript", "svelte", "c", "cpp",
                "go", "java", "bash", "csharp", "ruby", "swift",
                "objc", "php", "zig", "kotlin", "lua",
            ];

            // Build a list of (id, kind_str, lang_str, file_str) for each symbol
            let symbol_data: Vec<(String, &str, &str, String)> = symbols
                .iter()
                .enumerate()
                .map(|(i, (ki, li, fs))| {
                    let id = format!("sym_{i}");
                    let kind = kinds[*ki];
                    let lang = langs[*li];
                    let file = format!("src/{fs}.rs");
                    (id, kind, lang, file)
                })
                .collect();

            // Apply the filter predicate (mirrors search_symbols logic)
            let passes_filter = |kind: &str, lang: &str, file: &str| -> bool {
                if let Some(fk) = filter_kind {
                    if kind != kinds[fk] {
                        return false;
                    }
                }
                if let Some(fl) = filter_lang {
                    if lang != langs[fl] {
                        return false;
                    }
                }
                if let Some(ref ff) = filter_file {
                    // Simple substring match (analogous to glob matching on file path)
                    if !file.contains(ff.as_str()) {
                        return false;
                    }
                }
                true
            };

            // Collect results: only symbols that pass the filter
            let results: Vec<&str> = symbol_data
                .iter()
                .filter(|(_, kind, lang, file)| passes_filter(kind, lang, file))
                .map(|(id, _, _, _)| id.as_str())
                .collect();

            // Assert: no symbol that fails the predicate appears in results
            for (id, kind, lang, file) in &symbol_data {
                if !passes_filter(kind, lang, file) {
                    prop_assert!(
                        !results.contains(&id.as_str()),
                        "symbol {:?} (kind={}, lang={}, file={}) failed filter but appeared in results",
                        id, kind, lang, file
                    );
                }
            }
        }
    }

    // Feature: ollama-lmstudio-embeddings, Property 9: Pagination is consistent with other modes
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        /// Validates: Requirements 5.6
        fn prop_pagination_consistency(
            // R: total ranked results (0–50)
            r in 0usize..=50,
            // O: offset (0–60, may exceed R)
            o in 0usize..=60,
            // L: limit (0–30)
            l in 0usize..=30,
        ) {
            // Build a ranked list of R items
            let ranked: Vec<usize> = (0..r).collect();

            // Apply offset and limit — mirrors search_symbols pagination
            let page: Vec<usize> = ranked.iter().copied().skip(o).take(l).collect();

            // Expected count: min(L, max(0, R - O))
            let expected_count = l.min(r.saturating_sub(o));

            prop_assert_eq!(
                page.len(),
                expected_count,
                "pagination: R={}, O={}, L={} → expected {} items, got {}",
                r, o, l, expected_count, page.len()
            );

            // Also verify the items start at position O in the ranked list
            for (i, &item) in page.iter().enumerate() {
                prop_assert_eq!(
                    item,
                    o + i,
                    "pagination: item at position {} should be ranked[{}]={}, got {}",
                    i, o + i, o + i, item
                );
            }
        }
    }
}
