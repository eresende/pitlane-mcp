//! Knowledge document model and Markdown parsing.
//!
//! A knowledge document is one source file (e.g. a Markdown page) parsed into
//! heading-aware sections. Each section keeps its heading hierarchy and line
//! range so retrieval results can be read back with exact coordinates, and the
//! front matter is parsed as full YAML and preserved as a generic metadata
//! bag so OKF-specific fields (relationships, provenance, ...) can be modeled
//! in later phases without changing the core structures.

use std::collections::HashMap;

use pulldown_cmark::{Event, Parser as MarkdownParser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::links::{extract_links, KnowledgeLink};
use super::okf::{extract_okf, OkfMeta};

/// A knowledge document — one source file parsed into heading-aware sections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeDocument {
    /// Stable identifier: `knowledge:<relative/path>`.
    pub doc_id: String,
    /// Path relative to the project root, forward slashes.
    pub file_path: String,
    /// Document title: front matter `title`, first H1 heading, or file stem.
    pub title: String,
    /// Tags/categories from front matter (`tags` / `categories`).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Raw front matter as compact JSON (empty string when absent) for fields
    /// not modeled explicitly (including OKF extension keys). Stored as a
    /// string because bincode cannot deserialize `serde_json::Value` (it
    /// deserializes via `deserialize_any`).
    #[serde(default)]
    pub front_matter: String,
    /// Extracted OKF metadata, when the front matter carries a non-empty
    /// `type` key (Open Knowledge Format v0.2).
    #[serde(default)]
    pub okf: Option<OkfMeta>,
    /// Markdown links to other knowledge documents, normalized to
    /// bundle-relative paths (spec §6). Deduplicated by target, document order.
    #[serde(default)]
    pub links: Vec<KnowledgeLink>,
    /// Heading-aware sections in document order.
    pub sections: Vec<KnowledgeSection>,
}

impl KnowledgeDocument {
    /// Stable identifier for a document at `relative_path`.
    pub fn doc_id_for(relative_path: &str) -> String {
        format!("knowledge:{relative_path}")
    }

    /// Parsed front matter (empty object when absent).
    pub fn front_matter_value(&self) -> Value {
        serde_json::from_str(&self.front_matter).unwrap_or(Value::Object(Map::new()))
    }
}

/// A heading-delimited section of a knowledge document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeSection {
    /// Slugified heading hierarchy, unique within the document.
    pub section_id: String,
    /// This section's own heading text (empty for the preamble).
    pub heading: String,
    /// Full heading path from the document root, e.g. `["Setup", "Retry policy"]`.
    #[serde(default)]
    pub hierarchy: Vec<String>,
    /// Heading level; 0 = preamble (content before the first heading).
    pub level: usize,
    /// Section body: Markdown source between this heading and the next heading
    /// at any level.
    pub content: String,
    /// 1-based inclusive line range within the source file.
    pub line_start: usize,
    pub line_end: usize,
}

impl KnowledgeSection {
    /// Full identifier used in the embedding store and BM25 index.
    pub fn full_id(&self, doc_id: &str) -> String {
        format!("{doc_id}#{}", self.section_id)
    }

    /// Heading path rendered as `A > B > C` (empty for the preamble).
    pub fn hierarchy_path(&self) -> String {
        self.hierarchy.join(" > ")
    }

    /// True when the section carries no body text. Such sections are kept for
    /// navigation but skipped during embedding.
    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }

    /// Text embedded for this section: document context, heading path, body.
    pub fn embedding_text(&self, doc: &KnowledgeDocument) -> String {
        let mut text = doc.title.clone();
        if !self.hierarchy.is_empty() {
            text.push_str(" > ");
            text.push_str(&self.hierarchy_path());
        }
        text.push('\n');
        text.push_str(&self.content);
        text
    }
}

/// Parse Markdown `source` into a knowledge document for `relative_path`.
pub fn parse_markdown(relative_path: &str, source: &str) -> KnowledgeDocument {
    let (front_matter, body_start) = split_front_matter(source);
    let body = &source[body_start..];
    let okf = extract_okf(&front_matter);

    let line_starts = line_starts(source);
    let total_lines = source.lines().count().max(1);

    // Collect headings in document order. Offsets are relative to `body`.
    struct Heading {
        level: usize,
        text: String,
        start: usize,
        end: usize,
    }
    let mut headings: Vec<Heading> = Vec::new();
    let mut current: Option<usize> = None;
    for (event, range) in MarkdownParser::new(body).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let idx = headings.len();
                headings.push(Heading {
                    level: level as usize,
                    text: String::new(),
                    start: range.start,
                    end: body.len(),
                });
                current = Some(idx);
            }
            Event::Text(text) | Event::Code(text) if current.is_some() => {
                headings[current.unwrap()].text.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if current.is_some() => {
                headings[current.unwrap()].text.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(idx) = current.take() {
                    headings[idx].end = range.end;
                }
            }
            _ => {}
        }
    }

    // Preamble: content before the first heading (or the whole document when
    // there are no headings at all).
    let mut sections: Vec<KnowledgeSection> = Vec::new();
    let mut used_ids: HashMap<String, usize> = HashMap::new();
    let preamble_end = headings.first().map(|h| h.start).unwrap_or(body.len());
    if !body[..preamble_end].trim().is_empty() {
        sections.push(KnowledgeSection {
            section_id: "preamble".to_string(),
            heading: String::new(),
            hierarchy: Vec::new(),
            level: 0,
            content: body[..preamble_end].trim().to_string(),
            line_start: line_of(&line_starts, body_start),
            line_end: if preamble_end == body.len() {
                total_lines
            } else {
                line_of(&line_starts, body_start + preamble_end - 1)
            },
        });
        *used_ids.entry("preamble".to_string()).or_insert(0) += 1;
    }

    // One section per heading: body runs to the next heading at any level.
    let mut stack: Vec<(usize, String)> = Vec::new();
    for (i, h) in headings.iter().enumerate() {
        while stack.last().is_some_and(|(level, _)| *level >= h.level) {
            stack.pop();
        }
        let heading_text = h.text.trim().to_string();
        stack.push((h.level, heading_text.clone()));
        let hierarchy: Vec<String> = stack.iter().map(|(_, t)| t.clone()).collect();

        let end = headings.get(i + 1).map(|n| n.start).unwrap_or(body.len());
        let base = if heading_text.is_empty() {
            "section".to_string()
        } else {
            slugify(&hierarchy.join(" "))
        };
        let count = used_ids.entry(base.clone()).or_insert(0);
        *count += 1;
        let section_id = if *count == 1 {
            base
        } else {
            format!("{base}-{}", count)
        };

        sections.push(KnowledgeSection {
            section_id,
            heading: heading_text,
            hierarchy,
            level: h.level,
            content: body[h.end..end].trim().to_string(),
            line_start: line_of(&line_starts, body_start + h.start),
            line_end: if end == body.len() {
                total_lines
            } else {
                line_of(&line_starts, body_start + end - 1)
            },
        });
    }

    let fm_json = front_matter_value(&front_matter);
    let title = extract_title(&fm_json)
        .or_else(|| {
            headings
                .iter()
                .find(|h| h.level == 1)
                .map(|h| h.text.trim().to_string())
                .filter(|t| !t.is_empty())
        })
        .unwrap_or_else(|| file_stem(relative_path));

    KnowledgeDocument {
        doc_id: KnowledgeDocument::doc_id_for(relative_path),
        file_path: relative_path.to_string(),
        title,
        tags: extract_tags(&fm_json),
        okf,
        links: extract_links(body, relative_path),
        front_matter: if front_matter.is_null() {
            String::new()
        } else {
            serde_json::to_string(&fm_json).unwrap_or_default()
        },
        sections,
    }
}

/// Byte offset of each line start in `source` (line 0 starts at 0).
fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// 1-based line number containing byte offset `offset`.
fn line_of(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|start| *start <= offset)
}

/// Split leading YAML front matter from `source`.
///
/// Returns the parsed front matter (or `serde_yaml::Value::Null`) and the byte
/// offset where the document body begins. Full YAML is supported (nested
/// mappings, block/inline lists, flow `{a: b}` pairs) so OKF fields like
/// `generated: { by, at }` and `sources:` entries parse correctly. YAML that
/// fails to parse, or a block without a closing delimiter, is treated as plain
/// Markdown. Unknown keys are preserved verbatim.
fn split_front_matter(source: &str) -> (serde_yaml::Value, usize) {
    let mut lines = source.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return (serde_yaml::Value::Null, 0);
    };
    if first.trim_end() != "---" {
        return (serde_yaml::Value::Null, 0);
    }
    let mut offset = first.len();
    let mut block = String::new();
    for line in lines {
        if line.trim_end() == "---" || line.trim_end() == "..." {
            match serde_yaml::from_str(&block) {
                Ok(value) => return (value, offset + line.len()),
                Err(_) => return (serde_yaml::Value::Null, 0),
            }
        }
        block.push_str(line);
        offset += line.len();
    }
    // No closing delimiter — treat the file as plain Markdown.
    (serde_yaml::Value::Null, 0)
}

/// Convert a `serde_yaml::Value` into a `serde_json::Value` for the generic
/// front-matter bag.
fn front_matter_value(value: &serde_yaml::Value) -> Value {
    match value {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Number(serde_json::Number::from(i))
            } else if let Some(u) = n.as_u64() {
                Value::Number(serde_json::Number::from(u))
            } else {
                n.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map_or(Value::String(n.to_string()), Value::Number)
            }
        }
        serde_yaml::Value::String(s) => Value::String(s.clone()),
        serde_yaml::Value::Sequence(items) => {
            Value::Array(items.iter().map(front_matter_value).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let mut obj = Map::new();
            for (k, v) in map {
                let key = match k {
                    serde_yaml::Value::String(s) => s.clone(),
                    // Non-string mapping keys are stringified through JSON.
                    other => front_matter_value(other)
                        .to_string()
                        .trim_matches('"')
                        .to_string(),
                };
                obj.insert(key, front_matter_value(v));
            }
            Value::Object(obj)
        }
        // Unknown tags are flattened to their inner scalar.
        serde_yaml::Value::Tagged(tag) => front_matter_value(&tag.value),
    }
}

/// Front matter `title`, when present and non-empty.
fn extract_title(fm: &Value) -> Option<String> {
    fm.get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Front matter `tags` and/or `categories`, as a flat list of strings.
fn extract_tags(fm: &Value) -> Vec<String> {
    let mut tags = Vec::new();
    for key in ["tags", "categories"] {
        let Some(value) = fm.get(key) else {
            continue;
        };
        match value {
            Value::Array(items) => {
                for item in items {
                    if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        tags.push(s.to_string());
                    }
                }
            }
            Value::String(s) => {
                for part in s.split(',') {
                    let part = part.trim();
                    if !part.is_empty() {
                        tags.push(part.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    tags
}

/// Lowercase, hyphen-separated slug of `s` (non-alphanumeric runs → `-`).
fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c.to_lowercase().next().unwrap());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "section".to_string()
    } else {
        trimmed
    }
}

/// File stem (no directory, no final extension) of a relative path.
fn file_stem(relative_path: &str) -> String {
    let name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_string(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_keep_hierarchy_content_and_line_ranges() {
        let source =
            "# Setup\nIntro text.\n## **Retry** `policy`\nDetails here.\n## Other\nMore.\n";
        let doc = parse_markdown("docs/setup.md", source);
        assert_eq!(doc.doc_id, "knowledge:docs/setup.md");
        assert_eq!(doc.title, "Setup");
        assert_eq!(doc.sections.len(), 3);

        // Preamble under H1? No — "Intro text." belongs to the Setup section.
        let setup = &doc.sections[0];
        assert_eq!(setup.section_id, "setup");
        assert_eq!(setup.heading, "Setup");
        assert_eq!(setup.hierarchy, vec!["Setup".to_string()]);
        assert_eq!(setup.level, 1);
        assert_eq!(setup.content.trim(), "Intro text.");
        assert_eq!((setup.line_start, setup.line_end), (1, 2));

        let retry = &doc.sections[1];
        assert_eq!(retry.section_id, "setup-retry-policy");
        assert_eq!(retry.heading, "Retry policy");
        assert_eq!(
            retry.hierarchy,
            vec!["Setup".to_string(), "Retry policy".to_string()]
        );
        assert_eq!(retry.content.trim(), "Details here.");

        let other = &doc.sections[2];
        assert_eq!(other.section_id, "setup-other");
        assert_eq!((other.line_start, other.line_end), (5, 6));
    }

    #[test]
    fn preamble_and_headingless_documents() {
        let doc = parse_markdown("notes.md", "Just a note.\n# Title\nBody.\n");
        assert_eq!(doc.sections.len(), 2);
        assert_eq!(doc.sections[0].section_id, "preamble");
        assert_eq!(doc.sections[0].level, 0);
        assert_eq!(doc.sections[0].content.trim(), "Just a note.");

        let doc = parse_markdown("plain.md", "No headings at all.\n");
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].section_id, "preamble");
        assert_eq!(doc.title, "plain");
    }

    #[test]
    fn code_blocks_and_setext_headings_are_handled() {
        // Blank line before the setext heading: without it, CommonMark folds
        // the whole preceding paragraph ("Text. Final") into the heading.
        let source =
            "# Top\n```md\n# Fake heading inside code\n```\n## Sub\nText.\n\nFinal\n=====\nEnd.\n";
        let doc = parse_markdown("a.md", source);
        let ids: Vec<_> = doc.sections.iter().map(|s| s.section_id.as_str()).collect();
        assert_eq!(ids, vec!["top", "top-sub", "final"]);
        // The fenced "# Fake heading" must not create a section.
        assert!(!doc.sections.iter().any(|s| s.heading.contains("Fake")));
    }

    #[test]
    fn duplicate_headings_get_colliding_slugs() {
        let source = "# A\n## X\none\n## X\ntwo\n# B\n## X\nthree\n";
        let doc = parse_markdown("a.md", source);
        let ids: Vec<_> = doc.sections.iter().map(|s| s.section_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "a-x", "a-x-2", "b", "b-x"]);
    }

    #[test]
    fn empty_sections_are_flagged_but_kept() {
        let source = "# API\n## GET /users\nReturns users.\n";
        let doc = parse_markdown("api.md", source);
        assert_eq!(doc.sections.len(), 2);
        assert!(doc.sections[0].is_empty());
        assert!(!doc.sections[1].is_empty());
        assert_eq!(
            doc.sections[1].full_id(&doc.doc_id),
            "knowledge:api.md#api-get-users"
        );
    }

    #[test]
    fn front_matter_provides_title_tags_and_metadata_bag() {
        let source = "---\ntitle: \"Retry Guide\"\ntags: [ops, retry]\ncategories:\n  - runbook\nokf_version: v1\n---\n# Heading\nBody.\n";
        let doc = parse_markdown("guide.md", source);
        assert_eq!(doc.title, "Retry Guide");
        assert_eq!(
            doc.tags,
            vec![
                "ops".to_string(),
                "retry".to_string(),
                "runbook".to_string()
            ]
        );
        assert_eq!(
            doc.front_matter_value()["okf_version"],
            Value::String("v1".into())
        );
        // Body line numbers account for the front matter lines.
        let heading = &doc.sections[0];
        assert_eq!(heading.line_start, 8);
        assert_eq!(doc.sections.len(), 1);
    }

    #[test]
    fn missing_front_matter_falls_back_to_h1_or_stem() {
        let doc = parse_markdown("deep/dir/page.md", "Body only.\n");
        assert_eq!(doc.title, "page");
        let doc = parse_markdown("x.md", "# The Title\nBody.\n");
        assert_eq!(doc.title, "The Title");
    }

    #[test]
    fn unterminated_front_matter_is_treated_as_body() {
        let source = "---\ntitle: never closed\n# Real\nBody.\n";
        let doc = parse_markdown("a.md", source);
        assert_eq!(doc.front_matter, String::new());
        // The first line is a code-fence-like boundary only when paired; here
        // pulldown sees it as text, so the whole thing is one preamble section.
        assert!(doc.sections.iter().any(|s| s.section_id == "preamble"));
    }

    #[test]
    fn front_matter_list_edge_cases() {
        let source =
            "---\ntags: []\nempty:\n  - a\nnested:\n  deep: value\nok: 'yes'\n---\nBody.\n";
        let doc = parse_markdown("a.md", source);
        assert_eq!(doc.tags, Vec::<String>::new());
        let fm = doc.front_matter_value();
        assert_eq!(fm["empty"], Value::Array(vec![Value::String("a".into())]));
        // Full YAML parses nested mappings.
        assert_eq!(fm["nested"]["deep"], Value::String("value".into()));
        assert_eq!(fm["ok"], Value::String("yes".into()));
    }

    #[test]
    fn okf_front_matter_is_extracted_and_bag_preserved() {
        let source = "---\ntype: Metric\ntitle: Revenue\nstatus: stable\ngenerated: { by: agent/gemini, at: 2026-06-20T22:53:05Z }\nverified: { by: human:ana, at: 2026-06-25T09:00:00Z }\nruntime: bigquery\n---\n# Definition\nBody.\n";
        let doc = parse_markdown("finance/revenue.md", source);
        let okf = doc.okf.as_ref().unwrap();
        assert_eq!(okf.doc_type, "Metric");
        assert_eq!(okf.trust_tier, super::super::okf::TrustTier::HumanReviewed);
        assert_eq!(okf.generated_by.as_deref(), Some("agent/gemini"));
        // Unmodeled OKF keys stay in the generic bag.
        assert_eq!(doc.front_matter_value()["runtime"], "bigquery");

        // Plain Markdown (no `type`) gets no OKF metadata.
        let doc = parse_markdown("a.md", "---\ntitle: Plain\n---\nBody.\n");
        assert!(doc.okf.is_none());
    }

    #[test]
    fn malformed_yaml_front_matter_is_treated_as_body() {
        let source = "---\ntitle: [unclosed\nmore: {broken\n---\nBody.\n";
        let doc = parse_markdown("a.md", source);
        assert_eq!(doc.front_matter, String::new());
        assert!(doc.okf.is_none());
        assert!(doc.sections.iter().any(|s| s.section_id == "preamble"));
    }

    #[test]
    fn embedding_text_includes_document_context() {
        let source = "# Setup\n## Retry policy\nUse `retry_count`.\n";
        let doc = parse_markdown("docs/setup.md", source);
        let retry = &doc.sections[1];
        assert_eq!(
            retry.embedding_text(&doc),
            "Setup > Setup > Retry policy\nUse `retry_count`."
        );
    }

    #[test]
    fn round_trip_serialization() {
        let source = "# A\n## B\ntext\n";
        let doc = parse_markdown("a.md", source);
        let bytes = bincode::serde::encode_to_vec(&doc, bincode::config::standard()).unwrap();
        let loaded: KnowledgeDocument =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .unwrap()
                .0;
        assert_eq!(loaded, doc);
    }
}
