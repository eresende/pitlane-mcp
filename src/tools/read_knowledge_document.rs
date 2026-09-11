//! Full-document and section retrieval over the indexed knowledge base
//! (Phase 3 of issue #118).
//!
//! `search_knowledge` returns short snippets; this tool serves the
//! authoritative Markdown source afterwards — the whole file, or one
//! section's exact line range — plus document metadata, the section outline,
//! and the cross-document link graph. The raw read happens under the same
//! project lock as the incremental re-index, so the served text always
//! matches the parsed section coordinates.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::error::ToolError;
use crate::knowledge::document::{KnowledgeDocument, KnowledgeSection};
use crate::knowledge::{okf, with_knowledge_index};
use crate::path_policy::{read_regular_file, resolve_project_file, resolve_project_path};
use crate::tools::steering::{attach_steering, build_steering};

pub struct ReadKnowledgeDocumentParams {
    pub project: String,
    /// Project-relative Markdown path, as returned by `search_knowledge`
    /// (`file_path`).
    pub document: String,
    /// Optional section to read instead of the whole document: a
    /// `sections[].section_id` slug (e.g. `retry-policy`), `preamble`, or a
    /// full section id (`knowledge:docs/x.md#retry-policy`).
    pub section: Option<String>,
}

pub async fn read_knowledge_document(params: ReadKnowledgeDocumentParams) -> anyhow::Result<Value> {
    let document_path = params.document.trim().replace('\\', "/");
    if document_path.is_empty() {
        return Err(ToolError::InvalidArgument {
            param: "document".to_string(),
            message: "document must not be empty (use the project-relative path returned by search_knowledge)"
                .to_string(),
        }
        .into());
    }
    // `knowledge:path#slug` and bare slugs both resolve to the slug.
    let section = params
        .section
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.rsplit('#').next().unwrap_or(s).to_string());

    let canonical = resolve_project_path(&params.project)?;

    // Index refresh + raw file read in one blocking task; the lock inside
    // `with_knowledge_index` keeps the two consistent.
    let root = canonical;
    let payload = tokio::task::spawn_blocking(move || {
        with_knowledge_index(&root, |_changed, index| {
            let resolved = resolve_project_file(&root, &document_path)?;
            let rel = resolved
                .strip_prefix(&root)
                .map_err(|_| ToolError::AccessDenied {
                    path: document_path.clone(),
                })?
                .to_string_lossy()
                .replace('\\', "/");

            let doc_id = KnowledgeDocument::doc_id_for(&rel);
            let doc = index.documents.get(&doc_id).ok_or_else(|| {
                ToolError::DocumentNotFound {
                    path: rel.clone(),
                }
            })?;

            let source = String::from_utf8(read_regular_file(&resolved)?).map_err(|_| {
                ToolError::Internal {
                    message: format!("knowledge document '{rel}' is not valid UTF-8"),
                }
            })?;

            let stale = doc
                .okf
                .as_ref()
                .is_some_and(|m| okf::is_stale(m, okf::now_secs()));

            let mut response = Map::new();
            response.insert("document".into(), json!(doc.file_path));
            response.insert("doc_id".into(), json!(doc.doc_id));
            response.insert("title".into(), json!(doc.title));
            response.insert(
                "tags".into(),
                if doc.tags.is_empty() {
                    Value::Null
                } else {
                    json!(doc.tags)
                },
            );
            insert_okf_fields(doc, stale, &mut response);
            response.insert("front_matter".into(), doc.front_matter_value());
            response.insert(
                "related_docs".into(),
                if doc.links.is_empty() {
                    Value::Null
                } else {
                    json!(doc
                        .links
                        .iter()
                        .map(|l| json!({"target": l.target, "text": l.text}))
                        .collect::<Vec<_>>())
                },
            );
            response.insert("referenced_by".into(), referenced_by(&index, &rel));
            response.insert("section_count".into(), json!(doc.sections.len()));
            response.insert(
                "sections".into(),
                json!(doc.sections.iter().map(section_outline).collect::<Vec<_>>()),
            );

            let steering = match section.as_deref() {
                Some(slug) => {
                    let sec = doc
                        .sections
                        .iter()
                        .find(|s| s.section_id == slug)
                        .ok_or_else(|| ToolError::InvalidArgument {
                            param: "section".to_string(),
                            message: format!(
                                "section '{slug}' not found in '{rel}'; available: {}",
                                section_id_list(doc)
                            ),
                        })?;
                    response.insert(
                        "section".into(),
                        json!({
                            "section_id": sec.section_id,
                            "heading": outline_heading(sec),
                            "hierarchy": sec.hierarchy,
                            "level": sec.level,
                            "line_start": sec.line_start,
                            "line_end": sec.line_end,
                            "content": slice_lines(&source, sec.line_start, sec.line_end),
                        }),
                    );
                    build_steering(
                        0.95,
                        served_reason(doc, stale, &format!(
                            "section '{slug}' (lines {}–{})",
                            sec.line_start, sec.line_end
                        )),
                        follow_up_tool(doc),
                        follow_up_target(doc, &root),
                        Vec::new(),
                    )
                }
                None => {
                    response.insert("content".into(), json!(source));
                    response.insert("line_count".into(), json!(source.lines().count()));
                    build_steering(
                        0.95,
                        served_reason(doc, stale, &format!("full document ({rel})")),
                        follow_up_tool(doc),
                        follow_up_target(doc, &root),
                        Vec::new(),
                    )
                }
            };

            response.insert(
                "guidance".into(),
                json!({
                    "next_step": if section.is_some() {
                        "Navigate sibling sections via `sections` coordinates, or pass a `related_docs` target as `document` to follow the link."
                    } else {
                        "Pass `section` with one of the `sections[].section_id` values to fetch only that part of the document."
                    },
                    "avoid": if section.is_some() {
                        "Avoid loading whole documents when one section already answers the question."
                    } else {
                        "Avoid re-reading the full document when a single section suffices."
                    },
                }),
            );

            let mut response = Value::Object(response);
            attach_steering(&mut response, steering);
            Ok(response)
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!("knowledge indexing task failed: {e}"))??;
    Ok(payload)
}

/// Mirror of the `search_knowledge` result metadata: OKF fields are `null`
/// for plain-Markdown documents.
fn insert_okf_fields(doc: &KnowledgeDocument, stale: bool, response: &mut Map<String, Value>) {
    let okf = doc.okf.as_ref();
    let mut insert = |key: &str, value: Value| {
        response.insert(key.to_string(), value);
    };
    insert("stale", json!(stale));
    insert(
        "okf_type",
        okf.and_then(|m| (!m.doc_type.is_empty()).then(|| json!(m.doc_type)))
            .unwrap_or(Value::Null),
    );
    insert(
        "description",
        okf.and_then(|m| m.description.as_deref())
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
    );
    insert(
        "resource",
        okf.and_then(|m| m.resource.as_deref())
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
    );
    insert(
        "status",
        okf.and_then(|m| m.status.as_deref())
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
    );
    insert(
        "trust_tier",
        okf.map(|m| json!(m.trust_tier.as_str()))
            .unwrap_or(Value::Null),
    );
}

/// Reverse edges of the link graph: documents that link to `rel`.
fn referenced_by(index: &crate::knowledge::KnowledgeIndex, rel: &str) -> Value {
    let mut refs: Vec<String> = index
        .documents
        .values()
        .filter(|other| other.file_path != rel && other.links.iter().any(|l| l.target == rel))
        .map(|other| other.file_path.clone())
        .collect();
    refs.sort();
    if refs.is_empty() {
        Value::Null
    } else {
        json!(refs)
    }
}

fn section_outline(section: &KnowledgeSection) -> Value {
    json!({
        "section_id": section.section_id,
        "heading": outline_heading(section),
        "level": section.level,
        "line_start": section.line_start,
        "line_end": section.line_end,
    })
}

fn outline_heading(section: &KnowledgeSection) -> String {
    if section.hierarchy.is_empty() {
        "(preamble)".to_string()
    } else {
        section.hierarchy_path()
    }
}

/// Exact source slice for a section's 1-based inclusive line range.
fn slice_lines(source: &str, line_start: usize, line_end: usize) -> String {
    source
        .lines()
        .skip(line_start.saturating_sub(1))
        .take(line_end.saturating_sub(line_start) + 1)
        .collect::<Vec<_>>()
        .join("\n")
}

/// All section ids (capped) for the section-not-found error message.
fn section_id_list(doc: &KnowledgeDocument) -> String {
    let ids: Vec<&str> = doc.sections.iter().map(|s| s.section_id.as_str()).collect();
    if ids.len() <= 15 {
        ids.join(", ")
    } else {
        format!("{} … ({} sections)", ids[..15].join(", "), ids.len())
    }
}

fn served_reason(doc: &KnowledgeDocument, stale: bool, what: &str) -> String {
    let mut reason = format!("Served {what} of '{}'.", doc.title);
    if stale {
        reason.push_str(" Warning: the document is stale (its `stale_after` instant has passed).");
    }
    reason
}

fn follow_up_tool(doc: &KnowledgeDocument) -> &'static str {
    if doc.links.is_empty() {
        "search_knowledge"
    } else {
        "read_knowledge_document"
    }
}

fn follow_up_target(doc: &KnowledgeDocument, root: &Path) -> Value {
    match doc.links.first() {
        Some(link) => json!({
            "project": root.to_string_lossy(),
            "document": link.target,
        }),
        None => json!({ "project": root.to_string_lossy() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Two linked documents, one OKF, one plain Markdown, plus an
    /// unreachable file outside the knowledge index.
    fn fixture() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join("docs/guide.md"),
            "---\ntitle: Retry Guide\ntags: [ops, retry]\ntype: Playbook\nstatus: stable\nverified: { by: human:ana, at: 2026-06-25T09:00:00Z }\n---\n# Retry Guide\nPreamble text.\n## Backoff\nExponential backoff with jitter.\nSee [policies](policies.md) and [Overview](/README.md).\n## Deadlines\nPer-attempt deadlines.\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("docs/policies.md"),
            "Policies live here.\n# Policies\nFollow the [retry guide](guide.md#backoff).\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not indexed\n").unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    async fn read(
        root: &std::path::Path,
        document: &str,
        section: Option<&str>,
    ) -> anyhow::Result<Value> {
        read_knowledge_document(ReadKnowledgeDocumentParams {
            project: root.to_string_lossy().to_string(),
            document: document.to_string(),
            section: section.map(str::to_string),
        })
        .await
    }

    /// Structured error envelope for a failing call.
    fn err_json(value: anyhow::Result<Value>) -> Value {
        let err = value
            .expect_err("expected an error")
            .downcast::<ToolError>()
            .unwrap();
        err.to_json()
    }

    #[tokio::test]
    async fn whole_document_serves_raw_source_metadata_and_graph() {
        let (_dir, root) = fixture();
        let response = read(&root, "docs/guide.md", None).await.unwrap();

        assert_eq!(response["document"], "docs/guide.md");
        assert_eq!(response["doc_id"], "knowledge:docs/guide.md");
        assert_eq!(response["title"], "Retry Guide");
        assert_eq!(response["tags"], json!(["ops", "retry"]));
        assert_eq!(response["okf_type"], "Playbook");
        assert_eq!(response["trust_tier"], "human-reviewed");
        assert_eq!(response["stale"], false);
        // Raw source, front matter included, byte-faithful.
        let source = std::fs::read_to_string(root.join("docs/guide.md")).unwrap();
        assert_eq!(response["content"], source);
        assert_eq!(response["line_count"], source.lines().count());
        // Section outline with line coordinates.
        let ids: Vec<&str> = response["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["section_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![
                "retry-guide",
                "retry-guide-backoff",
                "retry-guide-deadlines"
            ]
        );
        // Parsed front matter bag including unmodeled keys.
        assert_eq!(response["front_matter"]["type"], "Playbook");
        // Link graph in both directions.
        let targets: Vec<&str> = response["related_docs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["target"].as_str().unwrap())
            .collect();
        assert_eq!(targets, vec!["docs/policies.md", "README.md"]);
        assert_eq!(response["referenced_by"], json!(["docs/policies.md"]));
    }

    #[tokio::test]
    async fn section_read_serves_exact_line_range() {
        let (_dir, root) = fixture();
        let source = std::fs::read_to_string(root.join("docs/guide.md")).unwrap();

        // Bare slug…
        let response = read(&root, "docs/guide.md", Some("retry-guide-backoff"))
            .await
            .unwrap();
        let section = &response["section"];
        assert_eq!(section["heading"], "Retry Guide > Backoff");
        assert_eq!(section["line_start"], 10);
        assert_eq!(section["line_end"], 12);
        let expected: Vec<&str> = source.lines().skip(9).take(3).collect();
        assert_eq!(section["content"], expected.join("\n"));
        // …and the full section id form from search results.
        let response = read(
            &root,
            "docs/guide.md",
            Some("knowledge:docs/guide.md#retry-guide-deadlines"),
        )
        .await
        .unwrap();
        assert_eq!(response["section"]["section_id"], "retry-guide-deadlines");
        // The outline is still present for navigation, the full text is not.
        assert_eq!(response["section_count"], 3);
        assert!(response["content"].is_null());
    }

    #[tokio::test]
    async fn preamble_section_is_served() {
        let (_dir, root) = fixture();
        let response = read(&root, "docs/policies.md", Some("preamble"))
            .await
            .unwrap();
        assert_eq!(response["section"]["heading"], "(preamble)");
        assert_eq!(response["section"]["content"], "Policies live here.");
    }

    #[tokio::test]
    async fn unknown_document_section_and_extension_fail_with_codes() {
        let (_dir, root) = fixture();

        let err = err_json(read(&root, "docs/missing.md", None).await);
        assert_eq!(err["error"]["code"], "DOCUMENT_NOT_FOUND");
        // Exists on disk but not a knowledge file.
        let err = err_json(read(&root, "notes.txt", None).await);
        assert_eq!(err["error"]["code"], "DOCUMENT_NOT_FOUND");
        let err = err_json(read(&root, "docs/guide.md", Some("nope")).await);
        assert_eq!(err["error"]["code"], "INVALID_ARGUMENT");
        assert!(err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("retry-guide-backoff"));
    }

    #[tokio::test]
    async fn traversal_and_absolute_document_paths_are_denied() {
        let (_dir, root) = fixture();
        // A Markdown file outside the project must not be served even when it
        // exists (unique name so parallel test binaries cannot collide).
        let outside = _dir
            .path()
            .parent()
            .unwrap()
            .join(format!("pitlane-read-outside-{}.md", std::process::id()));
        std::fs::write(&outside, "# Outside\n").unwrap();

        let err = err_json(read(&root, "../outside.md", None).await);
        assert_eq!(err["error"]["code"], "ACCESS_DENIED");
        let err = err_json(read(&root, &outside.to_string_lossy(), None).await);
        assert_eq!(err["error"]["code"], "ACCESS_DENIED");
        let _ = std::fs::remove_file(outside);
    }

    #[tokio::test]
    async fn stale_okf_documents_are_flagged() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("old.md"),
            "---\ntype: Metric\nstale_after: 2000-01-01T00:00:00Z\n---\n# Old\nBody.\n",
        )
        .unwrap();
        let root = dir.path().canonicalize().unwrap();

        let response = read(&root, "old.md", Some("old")).await.unwrap();
        assert_eq!(response["stale"], true);
        assert!(response["steering"]["why_this_matched"]
            .as_str()
            .unwrap()
            .contains("stale"));
    }
}
