//! Secret-safe process diagnostics captured once at server startup.

use pitlane_mcp::embed::document::{document_fingerprint, DOCUMENT_FORMAT_VERSION};
use pitlane_mcp::embed::{endpoint_fingerprint, EmbedConfig};
use pitlane_mcp::index::format::{index_cache_root, INDEX_SCHEMA_VERSION};
use serde_json::{json, Value};

pub fn snapshot(
    config: Option<&EmbedConfig>,
    error: Option<&str>,
    api_key_set: bool,
    tool_tier: &str,
) -> Value {
    let embeddings = match config {
        Some(config) => {
            // Exclude userinfo, path, query, and fragment, which may contain credentials.
            let host = reqwest::Url::parse(&config.url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned));
            json!({
                "enabled": true,
                "endpointHost": host,
                "model": config.model,
                "apiKey": if api_key_set { "set" } else { "unset" },
                "authorization": if config.headers.contains_key("authorization") { "set" } else { "unset" },
                "validation": if host.is_some() { "valid" } else { "endpoint URL has no valid host" },
                "endpointFingerprint": endpoint_fingerprint(&config.url, &config.headers),
                "documentFingerprint": document_fingerprint(&config.model),
            })
        }
        None => json!({
            "enabled": false,
            "apiKey": if api_key_set { "set" } else { "unset" },
            "validation": error.unwrap_or(
                "PITLANE_EMBED_URL and PITLANE_EMBED_MODEL must both be set and non-empty"
            ),
        }),
    };
    json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "toolExposureTier": tool_tier,
        "indexCacheDirectory": index_cache_root().ok().map(|path| path.display().to_string()),
        "indexSchemaVersion": INDEX_SCHEMA_VERSION,
        "embeddingDocumentFormatVersion": DOCUMENT_FORMAT_VERSION,
        "embeddings": embeddings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

    #[test]
    fn diagnostics_redact_credentials_and_preserve_cache_identity() {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer header-secret"),
        );
        headers.insert("x-api-key", HeaderValue::from_static("custom-secret"));
        let config = EmbedConfig {
            url: "https://user:password@example.com/secret-path?token=query-secret#fragment".into(),
            model: "test-model".into(),
            headers,
        };
        let result = snapshot(Some(&config), None, true, "all");
        let text = result.to_string();
        for secret in [
            "user:",
            "password",
            "secret-path",
            "query-secret",
            "fragment",
            "header-secret",
            "custom-secret",
        ] {
            assert!(!text.contains(secret), "diagnostics leaked {secret}");
        }
        assert_eq!(result["embeddings"]["endpointHost"], "example.com");
        assert_eq!(result["embeddings"]["apiKey"], "set");
        assert_eq!(result["toolExposureTier"], "all");
        assert_eq!(
            result["embeddings"]["endpointFingerprint"],
            endpoint_fingerprint(&config.url, &config.headers)
        );
    }

    #[test]
    fn invalid_config_errors_do_not_echo_supplied_data() {
        let error: anyhow::Error =
            pitlane_mcp::embed::EmbedConfigError::HeaderValueNotString.into();
        let result = snapshot(
            None,
            Some(pitlane_mcp::embed::config_error_message(&error)),
            false,
            "default",
        );
        assert_eq!(result["embeddings"]["enabled"], false);
        assert!(!result.to_string().contains("secret-value"));
        assert!(result["embeddings"]["validation"]
            .as_str()
            .unwrap()
            .contains("PITLANE_EMBED_HEADERS"));
    }

    #[test]
    fn unconfigured_embeddings_explain_required_variables() {
        let result = snapshot(None, None, false, "default");
        assert_eq!(result["embeddings"]["enabled"], false);
        assert_eq!(result["embeddings"]["apiKey"], "unset");
        assert!(result["embeddings"]["validation"]
            .as_str()
            .unwrap()
            .contains("PITLANE_EMBED_MODEL"));
    }
}
