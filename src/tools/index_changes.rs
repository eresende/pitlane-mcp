use serde_json::{json, Value};

use crate::index::format::{load_project_meta, RevisionChange};
use crate::path_policy::resolve_project_path;
use crate::tools::index_project::load_project_index;

pub struct GetIndexChangesParams {
    pub project: String,
    /// Return only revisions strictly greater than this value.
    /// Defaults to the current revision (i.e. "no changes").
    pub since_revision: Option<u64>,
    /// Maximum number of revisions returned, newest first (default: 20).
    pub limit: Option<usize>,
}

/// Feed of symbols changed since an index revision (issue #82).
///
/// Revisions are assigned by every persisted content change (fresh indexing,
/// watcher batch flush, full resync). A revision is visible to this tool once
/// the index flush lands; background embedding work does not delay it.
pub async fn get_index_changes(params: GetIndexChangesParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let canonical = resolve_project_path(&params.project)?;
    let meta = load_project_meta(&canonical)?;
    let current = meta.revision.max(index.revision);

    let since = params.since_revision.unwrap_or(current);
    let limit = params.limit.unwrap_or(20).max(1);

    // Newest first. Revisions the client already knows about are excluded.
    let mut entries: Vec<&RevisionChange> = meta
        .change_log
        .iter()
        .filter(|change| change.revision > since)
        .collect();
    entries.sort_by_key(|change| std::cmp::Reverse(change.revision));

    let earliest_logged = meta.change_log.iter().map(|c| c.revision).min();
    // The feed is complete when every revision between `since + 1` and
    // `current` has a log entry; otherwise the log was trimmed.
    let complete = match earliest_logged {
        Some(earliest) => since + 1 >= earliest,
        // No entries at all: complete iff there is nothing to report.
        None => since >= current,
    };

    // Aggregate distinct changed symbol IDs across the returned revisions.
    let mut changed_symbols: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for change in &entries {
        for id in change
            .added
            .iter()
            .chain(change.removed.iter())
            .chain(change.modified.iter())
        {
            if seen.insert(id.clone()) {
                changed_symbols.push(id.clone());
            }
        }
    }

    let changes: Vec<Value> = entries
        .iter()
        .take(limit)
        .map(|change| {
            json!({
                "revision": change.revision,
                "added": change.added,
                "removed": change.removed,
                "modified": change.modified,
                "truncated": change.truncated,
            })
        })
        .collect();

    let omitted_revisions = entries.len().saturating_sub(limit);

    Ok(json!({
        "project": canonical.display().to_string(),
        "current_revision": current,
        "since_revision": since,
        "up_to_date": since >= current,
        "complete": complete,
        "changed_symbol_count": changed_symbols.len(),
        "changed_symbols": changed_symbols,
        "revisions": changes,
        "omitted_revisions": omitted_revisions,
        "limits": { "max_revisions": limit },
        "note": if since >= current {
            "Index is at or ahead of the requested revision; no changes to report."
        } else if !complete {
            "The change log was trimmed; some revisions between since_revision and current_revision are no longer available. Re-index to re-baseline."
        } else {
            "Revisions become visible once the index flush completes; pending embedding work does not affect them."
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::format::IndexMeta;
    use crate::tools::index_project::{index_project, IndexProjectParams};
    use tempfile::TempDir;

    async fn index_dir(dir: &TempDir) -> String {
        index_project(IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
            on_index_progress: None,
            on_phase3_progress: None,
        })
        .await
        .unwrap();
        dir.path().to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn test_fresh_index_reports_no_changes_at_current_revision() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}\n").unwrap();
        let project = index_dir(&dir).await;

        let result = get_index_changes(GetIndexChangesParams {
            project,
            since_revision: None,
            limit: None,
        })
        .await
        .unwrap();

        assert_eq!(result["current_revision"], json!(1));
        assert_eq!(result["up_to_date"], json!(true));
        assert_eq!(result["revisions"], json!([]));
    }

    #[tokio::test]
    async fn test_revisions_from_zero_include_baseline() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}\n").unwrap();
        let project = index_dir(&dir).await;

        let result = get_index_changes(GetIndexChangesParams {
            project,
            since_revision: Some(0),
            limit: None,
        })
        .await
        .unwrap();

        // Revision 1 is the baseline index; since=0 sees it as one change.
        assert_eq!(result["current_revision"], json!(1));
        assert_eq!(result["complete"], json!(true));
        let revisions = result["revisions"].as_array().unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0]["revision"], json!(1));
    }

    #[test]
    fn test_record_change_assigns_monotonic_revisions_and_trims() {
        let mut meta = IndexMeta::new(std::path::Path::new("/tmp/x"));
        assert_eq!(meta.revision, 0);
        for i in 0..60 {
            meta.record_change(vec![format!("sym{i}")], Vec::new(), Vec::new());
        }
        assert_eq!(meta.revision, 60);
        assert_eq!(meta.change_log.len(), 50);
        assert_eq!(meta.change_log[0].revision, 11, "oldest entries trimmed");
        assert_eq!(meta.change_log[49].revision, 60);
    }

    #[test]
    fn test_record_change_caps_lists_and_flags_truncation() {
        let mut meta = IndexMeta::new(std::path::Path::new("/tmp/x"));
        let big: Vec<String> = (0..300).map(|i| format!("sym{i}")).collect();
        meta.record_change(big.clone(), big.clone(), big);
        let entry = &meta.change_log[0];
        assert!(entry.truncated);
        assert_eq!(entry.added.len(), 200);
        assert_eq!(entry.removed.len(), 200);
        assert_eq!(entry.modified.len(), 200);
    }

    #[test]
    fn test_meta_without_change_log_loads_with_defaults() {
        // A meta.json written before revision tracking must deserialize with
        // revision 0 and an empty log (serde defaults).
        let json = r#"{
            "project_path": "/tmp/x",
            "version": 5,
            "indexed_at": "0",
            "file_mtimes": {},
            "source_file_count": 0,
            "repo_profile": {"archetype": "library", "file_roles": {}, "role_counts": {}, "entrypoints": []}
        }"#;
        let meta: IndexMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.revision, 0);
        assert!(meta.change_log.is_empty());
    }
    #[tokio::test]
    async fn test_reindex_preserves_change_log_history() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn one() {}\n").unwrap();
        let project = index_dir(&dir).await; // baseline revision 1

        // Second (forced) index: revision 2, and revision 1 stays queryable.
        index_project(IndexProjectParams {
            path: project.clone(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
            on_index_progress: None,
            on_phase3_progress: None,
        })
        .await
        .unwrap();

        let result = get_index_changes(GetIndexChangesParams {
            project,
            since_revision: Some(0),
            limit: None,
        })
        .await
        .unwrap();

        assert_eq!(result["current_revision"], json!(2));
        assert_eq!(result["complete"], json!(true));
        let revisions = result["revisions"].as_array().unwrap();
        assert_eq!(revisions.len(), 2, "revisions={revisions:?}");
        assert_eq!(revisions[0]["revision"], json!(2));
        assert_eq!(revisions[1]["revision"], json!(1));
    }
}
