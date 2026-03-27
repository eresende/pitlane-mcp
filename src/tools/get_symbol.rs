use std::io::{Read, Seek, SeekFrom};

use serde_json::{Value, json};

use crate::tools::index_project::load_project_index;

pub struct GetSymbolParams {
    pub project: String,
    pub symbol_id: String,
    pub include_context: Option<bool>,
}

pub async fn get_symbol(params: GetSymbolParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;

    let sym = index.symbols.get(&params.symbol_id)
        .ok_or_else(|| anyhow::anyhow!("Symbol not found: {}", params.symbol_id))?;

    let include_context = params.include_context.unwrap_or(false);

    // Open the file and read the symbol bytes
    let mut file = std::fs::File::open(&sym.file)
        .map_err(|e| anyhow::anyhow!("Cannot open file {:?}: {}", sym.file, e))?;

    let source_text = if include_context {
        // Read entire file to get context lines
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let lines: Vec<&str> = content.lines().collect();

        let context_before = 3usize;
        let context_after = 3usize;
        let start_line = sym.line_start.saturating_sub(1) as usize; // 0-indexed
        let end_line = sym.line_end as usize; // exclusive

        let from = start_line.saturating_sub(context_before);
        let to = (end_line + context_after).min(lines.len());

        lines[from..to].join("\n")
    } else {
        // Seek to byte_start and read exactly the symbol bytes
        file.seek(SeekFrom::Start(sym.byte_start as u64))?;
        let len = sym.byte_end - sym.byte_start;
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf)?;
        String::from_utf8_lossy(&buf).to_string()
    };

    Ok(json!({
        "id": sym.id,
        "name": sym.name,
        "qualified": sym.qualified,
        "kind": sym.kind.to_string(),
        "language": sym.language.to_string(),
        "file": sym.file.display().to_string(),
        "line_start": sym.line_start,
        "line_end": sym.line_end,
        "source": source_text,
        "signature": sym.signature,
        "doc": sym.doc,
    }))
}
