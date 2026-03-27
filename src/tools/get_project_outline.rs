use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::indexer::is_excluded_dir_name;
use crate::tools::index_project::load_project_index;

pub struct GetProjectOutlineParams {
    pub project: String,
    pub depth: Option<u32>,
}

pub async fn get_project_outline(params: GetProjectOutlineParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let depth = params.depth.unwrap_or(2) as usize;

    let project_path = Path::new(&params.project).canonicalize()
        .unwrap_or_else(|_| Path::new(&params.project).to_path_buf());

    // Group files by directory up to `depth` levels
    // Map: directory_path -> { file_path -> { kind -> count } }
    let mut tree: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();

    for (file_path, ids) in &index.by_file {
        // Get relative path
        let rel = file_path.strip_prefix(&project_path).unwrap_or(file_path);

        // Skip files inside well-known dependency/build directories
        if rel.components().any(|c| {
            c.as_os_str().to_str().map_or(false, is_excluded_dir_name)
        }) {
            continue;
        }

        // Truncate to `depth` directory levels
        let dir_key = dir_at_depth(rel, depth);
        let file_key = rel.to_string_lossy().to_string();

        // Count symbols by kind
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

    // Convert to JSON
    let mut dirs_json = Vec::new();
    for (dir, files) in &tree {
        let mut files_json = Vec::new();
        for (file, kinds) in files {
            let kinds_json: Vec<Value> = kinds.iter()
                .map(|(k, v)| json!({ "kind": k, "count": v }))
                .collect();
            let total: usize = kinds.values().sum();
            files_json.push(json!({
                "path": file,
                "symbol_count": total,
                "by_kind": kinds_json,
            }));
        }
        let total_symbols: usize = files.values()
            .flat_map(|k| k.values())
            .sum();
        dirs_json.push(json!({
            "directory": dir,
            "file_count": files.len(),
            "symbol_count": total_symbols,
            "files": files_json,
        }));
    }

    Ok(json!({
        "project": params.project,
        "total_files": index.file_count(),
        "total_symbols": index.symbol_count(),
        "depth": depth,
        "directories": dirs_json,
    }))
}

fn dir_at_depth(rel_path: &Path, depth: usize) -> String {
    let components: Vec<_> = rel_path.components().collect();
    if components.len() <= 1 {
        return ".".to_string();
    }

    // Take up to `depth` directory components (not counting the filename)
    let dir_components = &components[..components.len() - 1];
    let take = depth.min(dir_components.len());
    let dir: PathBuf = dir_components[..take].iter().collect();
    dir.to_string_lossy().to_string()
}
