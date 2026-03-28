use std::io::{BufRead, BufReader};
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::tools::index_project::load_project_index;

pub struct FindUsagesParams {
    pub project: String,
    pub symbol_id: String,
    pub scope: Option<String>,
}

pub async fn find_usages(params: FindUsagesParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;

    let sym = index
        .symbols
        .get(&params.symbol_id)
        .ok_or_else(|| anyhow::anyhow!("Symbol not found: {}", params.symbol_id))?;

    let symbol_name = sym.name.clone();
    let project_path = Path::new(&params.project)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&params.project).to_path_buf());

    // Build scope glob set if provided
    let scope_set: Option<GlobSet> = params.scope.as_deref().map(|scope| {
        let mut builder = GlobSetBuilder::new();
        if let Ok(glob) = Glob::new(scope) {
            builder.add(glob);
        }
        builder
            .build()
            .unwrap_or_else(|_| GlobSetBuilder::new().build().unwrap())
    });

    let mut usages = Vec::new();

    // Walk all source files in the project
    for entry in WalkDir::new(&project_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "rs" && ext != "py" {
            continue;
        }

        // Apply scope filter
        if let Some(ref set) = scope_set {
            let rel = path.strip_prefix(&project_path).unwrap_or(path);
            if !set.is_match(rel) && !set.is_match(path) {
                continue;
            }
        }

        // Search lines for symbol name
        match search_file_for_name(path, &symbol_name) {
            Ok(hits) => {
                for (line_num, col, snippet) in hits {
                    let rel = path.strip_prefix(&project_path).unwrap_or(path);
                    usages.push(json!({
                        "file": rel.to_string_lossy(),
                        "line": line_num,
                        "column": col,
                        "snippet": snippet,
                    }));
                }
            }
            Err(_) => continue,
        }
    }

    Ok(json!({
        "symbol_id": params.symbol_id,
        "symbol_name": symbol_name,
        "usages": usages,
        "count": usages.len(),
    }))
}

fn search_file_for_name(path: &Path, name: &str) -> anyhow::Result<Vec<(usize, usize, String)>> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut hits = Vec::new();

    for (i, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        let line_num = i + 1;

        // Find all occurrences of name in the line
        let mut search_start = 0;
        while let Some(pos) = line[search_start..].find(name) {
            let abs_pos = search_start + pos;

            // Check word boundaries (simple check: surrounding chars are not word chars)
            let before_ok = abs_pos == 0 || {
                let c = line.as_bytes()[abs_pos - 1] as char;
                !c.is_alphanumeric() && c != '_'
            };
            let after_ok = abs_pos + name.len() >= line.len() || {
                let c = line.as_bytes()[abs_pos + name.len()] as char;
                !c.is_alphanumeric() && c != '_'
            };

            if before_ok && after_ok {
                hits.push((line_num, abs_pos + 1, line.trim().to_string()));
            }

            search_start = abs_pos + 1;
            if search_start >= line.len() {
                break;
            }
        }
    }

    Ok(hits)
}
