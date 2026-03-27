use std::path::Path;

use serde_json::{Value, json};

use crate::tools::index_project::load_project_index;

pub struct GetFileOutlineParams {
    pub project: String,
    pub file_path: String,
}

pub async fn get_file_outline(params: GetFileOutlineParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;

    let project_path = Path::new(&params.project).canonicalize()
        .unwrap_or_else(|_| Path::new(&params.project).to_path_buf());

    // Try to resolve the file path relative to project root
    let abs_file_path = if Path::new(&params.file_path).is_absolute() {
        Path::new(&params.file_path).to_path_buf()
    } else {
        project_path.join(&params.file_path)
    };

    // Find symbols for this file
    let file_ids = index.by_file.get(&abs_file_path);

    let mut symbols: Vec<Value> = match file_ids {
        None => {
            // Try to find by partial match
            let query_lower = params.file_path.to_lowercase();
            let mut found = Vec::new();
            for (file, ids) in &index.by_file {
                let file_str = file.to_string_lossy().to_lowercase();
                if file_str.ends_with(&query_lower) || file_str.contains(&query_lower) {
                    for id in ids {
                        if let Some(sym) = index.symbols.get(id) {
                            found.push(json!({
                                "id": sym.id,
                                "name": sym.name,
                                "qualified": sym.qualified,
                                "kind": sym.kind.to_string(),
                                "line_start": sym.line_start,
                                "line_end": sym.line_end,
                                "signature": sym.signature,
                            }));
                        }
                    }
                    break;
                }
            }
            found
        }
        Some(ids) => {
            ids.iter()
                .filter_map(|id| index.symbols.get(id))
                .map(|sym| json!({
                    "id": sym.id,
                    "name": sym.name,
                    "qualified": sym.qualified,
                    "kind": sym.kind.to_string(),
                    "line_start": sym.line_start,
                    "line_end": sym.line_end,
                    "signature": sym.signature,
                }))
                .collect()
        }
    };

    // Sort by line_start
    symbols.sort_by_key(|s| s["line_start"].as_u64().unwrap_or(0));

    Ok(json!({
        "file": params.file_path,
        "symbols": symbols,
        "count": symbols.len(),
    }))
}
