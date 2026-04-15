use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::index::format::load_project_meta;
use crate::indexer::is_excluded_dir_name;
use crate::path_policy::resolve_project_path;
use crate::tools::index_project::load_project_index;
use crate::tools::steering::{attach_steering, build_steering, take_fallback_candidates};

/// Hard cap: if the number of directories exceeds this, we collapse per-file
/// items into directory-level aggregates to keep the response manageable.
const COLLAPSE_THRESHOLD: usize = 200;

/// Absolute maximum directories we will ever return (even in collapsed form).
const HARD_MAX_DIRS: usize = 500;

pub struct GetProjectOutlineParams {
    pub project: String,
    pub depth: Option<u32>,
    /// Only include files under this directory prefix (relative to project root).
    pub path: Option<String>,
    /// Maximum directory entries to return (default: 50). Capped at 500.
    pub max_dirs: Option<usize>,
    /// When true, return only directory names with file and symbol counts —
    /// no per-file items, no per-kind breakdowns. Use for very large codebases
    /// where even the collapsed outline exceeds token limits.
    pub summary: Option<bool>,
}

pub async fn get_project_outline(params: GetProjectOutlineParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let project_path = resolve_project_path(&params.project)?;
    let profile = load_project_meta(&project_path)
        .ok()
        .map(|meta| meta.repo_profile);
    let depth = params.depth.unwrap_or(2) as usize;
    let max_dirs = params.max_dirs.unwrap_or(50).min(HARD_MAX_DIRS);
    let summary = params.summary.unwrap_or(false);

    // Normalise the optional path filter to a forward-slash prefix for matching.
    let path_filter: Option<String> = params.path.as_ref().map(|p| {
        let mut s = p.replace('\\', "/");
        if !s.ends_with('/') {
            s.push('/');
        }
        s
    });

    // ── Summary mode: lightweight directory-only view ──────────────────
    if summary {
        // Map: directory_path -> (file_count, symbol_count)
        let mut dir_summary: BTreeMap<String, (usize, usize)> = BTreeMap::new();

        for (file_path, ids) in &index.by_file {
            let rel = file_path.strip_prefix(&project_path).unwrap_or(file_path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");

            if let Some(ref prefix) = path_filter {
                if !rel_str.starts_with(prefix.as_str()) && rel_str != prefix.trim_end_matches('/')
                {
                    continue;
                }
            }

            if rel
                .components()
                .any(|c| c.as_os_str().to_str().is_some_and(is_excluded_dir_name))
            {
                continue;
            }

            let dir_key = dir_at_depth(rel, depth);
            let entry = dir_summary.entry(dir_key).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += ids.len();
        }

        let total_dirs = dir_summary.len();
        let truncated = total_dirs > max_dirs;

        let dirs_json: Vec<Value> = dir_summary
            .iter()
            .take(max_dirs)
            .map(
                |(dir, (files, symbols))| json!({ "dir": dir, "files": files, "symbols": symbols }),
            )
            .collect();

        let steering_dirs = dirs_json.clone();
        let mut result = json!({
            "project": params.project,
            "total_files": index.file_count(),
            "total_symbols": index.symbol_count(),
            "depth": depth,
            "summary": true,
            "directories": dirs_json,
        });
        if let Some(ref profile) = profile {
            result["repo_profile"] = json!({
                "archetype": crate::index::repo_profile::archetype_label(profile.archetype),
                "role_counts": crate::index::repo_profile::summarize_role_counts(Some(profile)),
                "entrypoints": profile.entrypoints.clone(),
            });
        }

        if let Some(ref p) = params.path {
            result["path_filter"] = json!(p);
        }
        if truncated {
            result["truncated"] = json!(true);
            result["total_dirs"] = json!(total_dirs);
            result["showing_dirs"] = json!(max_dirs);
            result["hint"] = json!(
                "Output truncated. Use 'path' to scope to a subtree, or increase 'max_dirs'."
            );
        }
        let steering = if steering_dirs.is_empty() {
            build_steering(
                0.3,
                "The project outline established the repo topology but did not isolate a specific subtree."
                    .to_string(),
                "search_files",
                json!({ "project": params.project, "path": params.path }),
                take_fallback_candidates(&steering_dirs),
            )
        } else {
            build_steering(
                if truncated { 0.68 } else { 0.78 },
                "The directory outline identifies the most relevant subtree for follow-up navigation."
                    .to_string(),
                "search_files",
                json!({
                    "project": params.project,
                    "path": steering_dirs[0]["dir"],
                }),
                take_fallback_candidates(&steering_dirs),
            )
        };
        attach_steering(&mut result, steering);

        return Ok(result);
    }

    // ── Full mode: per-file detail ────────────────────────────────────
    // Map: directory_path -> { file_path -> { kind -> count } }
    let mut tree: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();

    for (file_path, ids) in &index.by_file {
        let rel = file_path.strip_prefix(&project_path).unwrap_or(file_path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        if let Some(ref prefix) = path_filter {
            if !rel_str.starts_with(prefix.as_str()) && rel_str != prefix.trim_end_matches('/') {
                continue;
            }
        }

        if rel
            .components()
            .any(|c| c.as_os_str().to_str().is_some_and(is_excluded_dir_name))
        {
            continue;
        }

        let dir_key = dir_at_depth(rel, depth);
        let file_key = rel_str;

        let file_entry = tree
            .entry(dir_key)
            .or_default()
            .entry(file_key)
            .or_default();

        for id in ids {
            if let Some(sym) = index.symbols.get(id) {
                *file_entry.entry(sym.kind.to_string()).or_insert(0) += 1;
            }
        }
    }

    let total_dirs = tree.len();
    let truncated = total_dirs > max_dirs;
    let collapse_items = total_dirs > COLLAPSE_THRESHOLD;

    let mut dirs_json = Vec::new();
    for (dir, files) in tree.iter().take(max_dirs) {
        let total_files = files.len();
        let total_symbols: usize = files.values().flat_map(|k| k.values()).sum();

        if collapse_items {
            let mut agg: BTreeMap<String, usize> = BTreeMap::new();
            for kinds in files.values() {
                for (k, v) in kinds {
                    *agg.entry(k.clone()).or_insert(0) += v;
                }
            }
            let kinds_obj: serde_json::Map<String, Value> =
                agg.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            dirs_json.push(json!({
                "dir": dir,
                "files": total_files,
                "symbols": total_symbols,
                "kinds": kinds_obj,
            }));
        } else {
            let files_map: serde_json::Map<String, Value> = files
                .iter()
                .map(|(file, kinds)| {
                    let kinds_obj: serde_json::Map<String, Value> =
                        kinds.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
                    (file.clone(), Value::Object(kinds_obj))
                })
                .collect();
            dirs_json.push(json!({
                "dir": dir,
                "files": total_files,
                "symbols": total_symbols,
                "items": files_map,
            }));
        }
    }

    let steering_dirs = dirs_json.clone();
    let mut result = json!({
        "project": params.project,
        "total_files": index.file_count(),
        "total_symbols": index.symbol_count(),
        "depth": depth,
        "directories": dirs_json,
    });
    if let Some(ref profile) = profile {
        result["repo_profile"] = json!({
            "archetype": crate::index::repo_profile::archetype_label(profile.archetype),
            "role_counts": crate::index::repo_profile::summarize_role_counts(Some(profile)),
            "entrypoints": profile.entrypoints.clone(),
        });
    }

    if let Some(ref p) = params.path {
        result["path_filter"] = json!(p);
    }
    if truncated {
        result["truncated"] = json!(true);
        result["total_dirs"] = json!(total_dirs);
        result["showing_dirs"] = json!(max_dirs);
        result["hint"] = json!(
            "Output truncated. Use 'path' to scope to a subtree, or increase 'max_dirs'. If the output still exceeds token limits, use summary=true for a lightweight directory-only view."
        );
    }
    if collapse_items {
        result["collapsed"] = json!(true);
        result["collapse_hint"] = json!(
            "Per-file items omitted (too many directories). Use 'path' to scope to a subtree for full detail."
        );
    }
    let steering = if steering_dirs.is_empty() {
        build_steering(
            0.3,
            "The project outline established the repo topology but did not isolate a specific subtree."
                .to_string(),
            "search_files",
            json!({ "project": params.project, "path": params.path }),
            take_fallback_candidates(&steering_dirs),
        )
    } else {
        build_steering(
            if collapse_items || truncated {
                0.7
            } else {
                0.82
            },
            "The directory outline identifies the most relevant subtree for follow-up navigation."
                .to_string(),
            "search_files",
            json!({
                    "project": params.project,
                "path": steering_dirs[0]["dir"],
            }),
            take_fallback_candidates(&steering_dirs),
        )
    };
    attach_steering(&mut result, steering);

    Ok(result)
}

fn dir_at_depth(rel_path: &Path, depth: usize) -> String {
    let components: Vec<_> = rel_path.components().collect();
    if components.len() <= 1 {
        return ".".to_string();
    }

    let dir_components = &components[..components.len() - 1];
    let take = depth.min(dir_components.len());
    let dir: PathBuf = dir_components[..take].iter().collect();
    dir.to_string_lossy().replace('\\', "/")
}
