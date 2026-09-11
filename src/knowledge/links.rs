//! Markdown link extraction between knowledge documents (OKF spec §6).
//!
//! Concepts relate to each other via standard markdown links. Links whose
//! target is another document (`.md`) are resolved to bundle-relative paths
//! and stored on the [`KnowledgeDocument`](crate::knowledge::document)
//! so search results can expose relationships without re-parsing bodies.
//! Absolute (`/tables/orders.md`) and relative (`./other.md`,
//! `../computations/revenue.md`) forms are supported; external URLs,
//! anchors-only links, and non-document targets are ignored. Per spec §6.1,
//! broken links are tolerated — targets are not validated here.

use serde::{Deserialize, Serialize};

/// A link from one knowledge document to another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeLink {
    /// Normalized target path relative to the project root, forward slashes
    /// (e.g. `computations/revenue.md`).
    pub target: String,
    /// Link text as written in the body.
    pub text: String,
}

/// Extract document-to-document links from `body`. `relative_path` (project-
/// relative, forward slashes) is the containing document; relative targets are
/// resolved against its parent directory. Duplicates collapse on target
/// (first link text wins).
pub fn extract_links(body: &str, relative_path: &str) -> Vec<KnowledgeLink> {
    let base_dir = match relative_path.rfind('/') {
        Some(i) => &relative_path[..i],
        None => "",
    };

    let mut links: Vec<KnowledgeLink> = Vec::new();
    let mut current: Option<KnowledgeLink> = None;
    for (event, _) in pulldown_cmark::Parser::new(body).into_offset_iter() {
        match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) => {
                current = normalize_target(&dest_url, base_dir).map(|link| KnowledgeLink {
                    target: link.target,
                    text: String::new(),
                });
            }
            pulldown_cmark::Event::Text(text) | pulldown_cmark::Event::Code(text)
                if current.is_some() =>
            {
                if let Some(link) = current.as_mut() {
                    if !link.text.is_empty() {
                        link.text.push(' ');
                    }
                    link.text.push_str(&text);
                }
            }
            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Link) => {
                if let Some(link) = current.take() {
                    // Deduplicate by target: the first link text wins.
                    if links.iter().any(|l| l.target == link.target) {
                        continue;
                    }
                    links.push(link);
                }
            }
            _ => {}
        }
    }
    links
}

/// Normalize a link destination to a project-relative `.md` path, or `None`
/// when the target is not a document link.
fn normalize_target(dest: &str, base_dir: &str) -> Option<KnowledgeLink> {
    let dest = dest.trim();
    if dest.is_empty() || dest.starts_with("mailto:") {
        return None;
    }
    if dest.contains("://") || dest.starts_with('#') {
        return None;
    }
    // Strip a fragment/anchor: `other.md#section` still targets `other.md`.
    let (path, _anchor) = dest.split_once('#').unwrap_or((dest, ""));
    if !path.to_ascii_lowercase().ends_with(".md") {
        return None;
    }

    let target = if let Some(stripped) = path.strip_prefix('/') {
        // Bundle-relative: interpreted against the project root.
        normalize_segments(stripped)?
    } else {
        let joined = if base_dir.is_empty() {
            path.to_string()
        } else {
            format!("{base_dir}/{path}")
        };
        normalize_segments(&joined)?
    };
    if target.is_empty() {
        return None;
    }
    Some(KnowledgeLink {
        target,
        text: String::new(),
    })
}

/// Collapse `.` / `..` segments in a `/`-separated path. Returns `None` when
/// the path escapes above the root.
fn normalize_segments(path: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    Some(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_relative_and_relative_links_are_normalized() {
        let body = "Join key: [customers](/tables/customers.md) and [revenue](../computations/revenue.md) plus [neighbour](./other.md#section).";
        let links = extract_links(body, "metrics/income.md");
        assert_eq!(
            links,
            vec![
                KnowledgeLink {
                    target: "tables/customers.md".into(),
                    text: "customers".into()
                },
                KnowledgeLink {
                    target: "computations/revenue.md".into(),
                    text: "revenue".into()
                },
                KnowledgeLink {
                    target: "metrics/other.md".into(),
                    text: "neighbour".into()
                },
            ]
        );
    }

    #[test]
    fn root_level_relative_links_resolve_against_the_root() {
        let links = extract_links("[other](other.md) [up](../outside.md)", "readme.md");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "other.md");
    }

    #[test]
    fn external_and_non_document_targets_are_ignored() {
        let body = "[web](https://example.com/doc.md) [anchor](#section) [png](img.png) [mail](mailto:a@b.c) [empty]( )";
        assert!(extract_links(body, "a.md").is_empty());
    }

    #[test]
    fn escaping_paths_are_rejected() {
        assert!(extract_links("[x](../../outside.md)", "a.md").is_empty());
    }

    #[test]
    fn duplicates_collapse_by_target_and_broken_links_are_kept() {
        let body = "[a](/tables/orders.md) [b](/tables/orders.md) [missing](/nope/ghost.md)";
        let links = extract_links(body, "m.md");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].text, "a");
        assert_eq!(links[1].target, "nope/ghost.md");
    }

    #[test]
    fn links_in_code_and_autolinks_are_not_extracted() {
        let body = "```\n[x](/tables/orders.md)\n```\nInline `<https://example.com>` and plain /tables/orders.md text.";
        assert!(extract_links(body, "a.md").is_empty());
    }
}
