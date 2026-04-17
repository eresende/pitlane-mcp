use serde_json::{json, Value};
use std::path::Path;

use crate::error::ToolError;
use crate::graph::{collect_direct_references, read_symbol_source};
use crate::indexer::language::SymbolKind;
use crate::path_policy::resolve_project_path;
use crate::session;
use crate::tools::index_project::load_project_index;
use crate::tools::steering::{attach_steering, build_steering, take_fallback_candidates};

const MAX_REFERENCES: usize = 25;

pub struct GetSymbolParams {
    pub project: String,
    pub symbol_id: String,
    pub include_context: Option<bool>,
    pub signature_only: Option<bool>,
}

pub async fn get_symbol(params: GetSymbolParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let canonical_project = resolve_project_path(&params.project)?;
    let canonical_project = canonical_project.to_string_lossy().to_string();

    let sym = index
        .symbols
        .get(&params.symbol_id)
        .ok_or_else(|| ToolError::SymbolNotFound {
            symbol_id: params.symbol_id.clone(),
        })?;

    // signature_only defaults to true for container kinds (struct, class,
    // interface, trait) because agents almost always want the shape, not the
    // full method bodies.  Pass signature_only=false explicitly to override.
    let class_like = matches!(
        sym.kind,
        SymbolKind::Struct | SymbolKind::Class | SymbolKind::Interface | SymbolKind::Trait
    );
    let use_signature_only = params.signature_only.unwrap_or(class_like);
    let full_source_bytes = (sym.byte_end - sym.byte_start) as u64;

    if use_signature_only {
        let returned_bytes = sym.signature.as_deref().unwrap_or("").len() as u64
            + sym.doc.as_deref().unwrap_or("").len() as u64;
        let content = format!(
            "{}\n{}",
            sym.signature.as_deref().unwrap_or(""),
            sym.doc.as_deref().unwrap_or("")
        );
        let observation =
            session::observe_content(Path::new(&canonical_project), "symbol", &sym.id, &content);
        let mut response = json!({
            "id": sym.id,
            "name": sym.name,
            "qualified": sym.qualified,
            "kind": sym.kind.to_string(),
            "language": sym.language.to_string(),
            "file": sym.file.to_string_lossy().replace('\\', "/"),
            "line_start": sym.line_start,
            "line_end": sym.line_end,
            "signature": sym.signature,
            "doc": sym.doc,
            "content_seen": observation.content_seen,
            "target_seen": observation.target_seen,
            "content_changed": observation.changed_since_last_read,
        });
        attach_steering(
            &mut response,
            build_steering(
                if observation.content_seen {
                    0.72
                } else if observation.changed_since_last_read {
                    0.8
                } else {
                    0.84
                },
                if observation.content_seen {
                    "The symbol shape and documentation were already returned in this session, so this is a repeat read."
                        .to_string()
                } else if observation.changed_since_last_read {
                    "The symbol shape was reread and changed since the previous read in this session."
                        .to_string()
                } else {
                    "The symbol shape and documentation were returned without reading the full body."
                        .to_string()
                },
                "find_usages",
                json!({
                    "symbol_id": sym.id,
                    "name": sym.name,
                    "file": sym.file.to_string_lossy().replace('\\', "/"),
                }),
                Vec::new(),
            ),
        );
        crate::stats::record_get_symbol(
            &canonical_project,
            true,
            full_source_bytes,
            returned_bytes,
        );
        session::record_symbol(
            Path::new(&canonical_project),
            &sym.id,
            Some(sym.file.as_ref()),
        );
        return Ok(response);
    }

    let include_context = params.include_context.unwrap_or(false);
    let source_text = read_symbol_source(sym, include_context)?;

    crate::stats::record_get_symbol(
        &canonical_project,
        false,
        full_source_bytes,
        full_source_bytes,
    );

    let mut refs: Vec<Value> = collect_direct_references(&index, sym, Some(&source_text))
        .into_iter()
        .map(|reference| {
            json!({
                "id": reference.id,
                "name": reference.name,
                "kind": reference.kind,
                "file": reference.file,
                "line_start": reference.line_start,
                "evidence": reference.evidence,
                "confidence": reference.confidence,
            })
        })
        .collect();
    let references_truncated = refs.len() > MAX_REFERENCES;
    refs.truncate(MAX_REFERENCES);
    let steering_refs = refs.clone();
    let has_references = !steering_refs.is_empty();
    let observation = session::observe_content(
        Path::new(&canonical_project),
        "symbol",
        &sym.id,
        &source_text,
    );
    let mut response = json!({
        "id": sym.id,
        "name": sym.name,
        "qualified": sym.qualified,
        "kind": sym.kind.to_string(),
        "language": sym.language.to_string(),
        "file": sym.file.to_string_lossy().replace('\\', "/"),
        "line_start": sym.line_start,
        "line_end": sym.line_end,
        "source": source_text,
        "signature": sym.signature,
        "doc": sym.doc,
        "content_seen": observation.content_seen,
        "target_seen": observation.target_seen,
        "content_changed": observation.changed_since_last_read,
        "references": refs,
        "references_truncated": references_truncated,
    });
    let content_seen = response["content_seen"].as_bool().unwrap_or(false);
    let content_changed = response["content_changed"].as_bool().unwrap_or(false);
    attach_steering(
        &mut response,
        build_steering(
            if content_seen {
                0.76
            } else if content_changed {
                0.82
            } else if references_truncated {
                0.86
            } else if has_references {
                0.9
            } else {
                0.72
            },
            if content_seen {
                "The symbol body was already returned in this session, so this read is a repeat."
                    .to_string()
            } else if content_changed {
                "The symbol body changed since the previous read in this session, so this reread contains new content."
                    .to_string()
            } else {
                "The symbol body was read and direct identifier references were extracted for local expansion."
                    .to_string()
            },
            "find_usages",
            json!({
                "symbol_id": sym.id,
                "name": sym.name,
                "file": sym.file.to_string_lossy().replace('\\', "/"),
            }),
            take_fallback_candidates(&steering_refs),
        ),
    );
    session::record_symbol(
        Path::new(&canonical_project),
        &sym.id,
        Some(sym.file.as_ref()),
    );

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::extract_identifiers;
    use crate::index::format::{index_dir, save_index};
    use crate::indexer::{registry, Indexer};
    use tempfile::TempDir;

    /// Index a temp project to disk and return its path string.
    async fn setup_project(dir: &TempDir) -> String {
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        dir.path().to_string_lossy().to_string()
    }

    fn first_symbol_id(project: &str) -> String {
        let index = load_project_index(project).unwrap();
        index.symbols.keys().next().unwrap().clone()
    }

    fn symbol_id_by_name(project: &str, name: &str) -> String {
        let index = load_project_index(project).unwrap();
        index
            .symbols
            .values()
            .find(|symbol| symbol.name == name)
            .map(|symbol| symbol.id.clone())
            .unwrap_or_else(|| panic!("symbol {name} not found"))
    }

    /// signature_only returns the signature field and no source body.
    #[tokio::test]
    async fn test_signature_only_omits_source() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}").unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = first_symbol_id(&project);

        let result = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: Some(true),
        })
        .await
        .unwrap();

        assert!(
            result.get("source").is_none(),
            "source should not be present"
        );
        assert_eq!(result["signature"].as_str().unwrap(), "pub fn hello() {}");
    }

    /// Without signature_only the full source body is returned.
    #[tokio::test]
    async fn test_default_returns_source() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}").unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = first_symbol_id(&project);

        let result = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();

        assert!(result.get("source").is_some(), "source should be present");
        assert_eq!(result["source"].as_str().unwrap(), "pub fn hello() {}");
    }

    /// Repeating the same symbol read should mark the content as seen.
    #[tokio::test]
    async fn test_repeated_symbol_read_marks_content_seen() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}").unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = first_symbol_id(&project);

        let first = get_symbol(GetSymbolParams {
            project: project.clone(),
            symbol_id: symbol_id.clone(),
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();
        let second = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();

        assert_eq!(first["content_seen"].as_bool(), Some(false));
        assert_eq!(second["content_seen"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn test_full_symbol_read_marks_changed_content_for_same_target() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"pub fn hello() { old(); }\npub fn old() {}").unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = symbol_id_by_name(&project, "hello");

        let first = get_symbol(GetSymbolParams {
            project: project.clone(),
            symbol_id: symbol_id.clone(),
            include_context: None,
            signature_only: Some(false),
        })
        .await
        .unwrap();

        std::fs::write(&file, b"pub fn hello() { newer(); }\npub fn newer() {}").unwrap();
        let project = setup_project(&dir).await;
        let second = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: Some(false),
        })
        .await
        .unwrap();

        assert_eq!(first["content_changed"].as_bool(), Some(false));
        assert_eq!(second["target_seen"].as_bool(), Some(true));
        assert_eq!(second["content_changed"].as_bool(), Some(true));
        assert!(second["steering"]["why_this_matched"]
            .as_str()
            .unwrap()
            .contains("changed since the previous read"));
    }

    /// signature_only captures the doc comment stored in the index.
    #[tokio::test]
    async fn test_signature_only_includes_doc() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"/// Greets the world\npub fn hello() {}",
        )
        .unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = first_symbol_id(&project);

        let result = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: Some(true),
        })
        .await
        .unwrap();

        let doc = result["doc"].as_str().unwrap_or("");
        assert!(
            doc.contains("Greets the world"),
            "doc should contain the doc comment, got: {doc:?}"
        );
    }

    /// signature_only succeeds even after the source file is deleted,
    /// confirming it performs no file I/O.
    #[tokio::test]
    async fn test_signature_only_no_file_io() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"pub fn transient() {}").unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = first_symbol_id(&project);

        // Remove the source file — signature_only must still work.
        std::fs::remove_file(&file).unwrap();

        let result = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: Some(true),
        })
        .await;

        assert!(result.is_ok(), "signature_only should not read the file");
        assert_eq!(
            result.unwrap()["signature"].as_str().unwrap(),
            "pub fn transient() {}"
        );
    }

    #[tokio::test]
    async fn test_unknown_symbol_id_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}").unwrap();
        let project = setup_project(&dir).await;

        let err = get_symbol(GetSymbolParams {
            project,
            symbol_id: "nonexistent::symbol#function".to_string(),
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<crate::error::ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "SYMBOL_NOT_FOUND");
        assert!(tool_err
            .to_string()
            .contains("nonexistent::symbol#function"));
    }

    /// struct without explicit signature_only defaults to signature-only mode.
    #[tokio::test]
    async fn test_struct_defaults_to_signature_only() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub struct Foo { pub x: i32, pub y: i32 }",
        )
        .unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = first_symbol_id(&project);

        let result = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();

        assert!(
            result.get("source").is_none(),
            "struct should default to signature-only (no source)"
        );
        assert!(result["signature"].as_str().is_some());
    }

    /// passing signature_only=false on a struct overrides the default and returns source.
    #[tokio::test]
    async fn test_struct_signature_only_false_returns_source() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub struct Foo { pub x: i32 }").unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = first_symbol_id(&project);

        let result = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: Some(false),
        })
        .await
        .unwrap();

        assert!(
            result.get("source").is_some(),
            "explicit signature_only=false should return full source"
        );
    }

    /// Full-source response includes a references list.
    #[tokio::test]
    async fn test_full_source_includes_references() {
        let dir = TempDir::new().unwrap();
        // helper() is called inside caller() — should appear in references.
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn helper() {}\npub fn caller() { helper(); }",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        // Find the caller symbol id.
        let index = load_project_index(&project).unwrap();
        let caller_id = index
            .symbols
            .values()
            .find(|s| s.name == "caller")
            .map(|s| s.id.clone())
            .expect("caller symbol must exist");

        let result = get_symbol(GetSymbolParams {
            project,
            symbol_id: caller_id,
            include_context: None,
            signature_only: Some(false),
        })
        .await
        .unwrap();

        let refs = result["references"]
            .as_array()
            .expect("references must be an array");
        let ref_names: Vec<&str> = refs.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert!(
            ref_names.contains(&"helper"),
            "helper should appear in references, got: {ref_names:?}"
        );
    }

    /// Signature-only response does NOT include a references field.
    #[tokio::test]
    async fn test_signature_only_has_no_references() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn helper() {}\npub fn caller() { helper(); }",
        )
        .unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = first_symbol_id(&project);

        let result = get_symbol(GetSymbolParams {
            project,
            symbol_id,
            include_context: None,
            signature_only: Some(true),
        })
        .await
        .unwrap();

        assert!(
            result.get("references").is_none(),
            "signature_only response should not include references"
        );
    }

    /// extract_identifiers unit tests.
    #[test]
    fn test_extract_identifiers_basic() {
        let ids = extract_identifiers("let foo = bar(baz);");
        assert!(ids.contains("foo"));
        assert!(ids.contains("bar"));
        assert!(ids.contains("baz"));
    }

    #[test]
    fn test_extract_identifiers_skips_short_tokens() {
        let ids = extract_identifiers("if x > 0 { foo(); }");
        assert!(!ids.contains("if"));
        assert!(!ids.contains("x"));
        assert!(ids.contains("foo"));
    }

    #[test]
    fn test_extract_identifiers_skips_numbers() {
        let ids = extract_identifiers("let x = 123 + foo;");
        assert!(!ids.contains("123"));
        assert!(ids.contains("foo"));
    }
}
