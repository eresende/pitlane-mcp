use serde_json::{json, Value};

use crate::path_policy::{resolve_project_file, resolve_project_path};

/// Maximum lines that can be fetched in a single call.
const MAX_LINES: u32 = 500;

pub struct GetLinesParams {
    pub project: String,
    pub file_path: String,
    /// First line to return, 1-indexed inclusive.
    pub line_start: u32,
    /// Last line to return, 1-indexed inclusive.
    pub line_end: u32,
}

pub async fn get_lines(params: GetLinesParams) -> anyhow::Result<Value> {
    if params.line_start == 0 {
        anyhow::bail!("line_start must be >= 1");
    }
    if params.line_start > params.line_end {
        anyhow::bail!(
            "line_start ({}) must be <= line_end ({})",
            params.line_start,
            params.line_end
        );
    }

    let requested = params.line_end - params.line_start + 1;
    let capped = requested.min(MAX_LINES);
    let effective_end = params.line_start + capped - 1;
    let truncated = requested > MAX_LINES;

    let project_path = resolve_project_path(&params.project)?;
    let abs_path = resolve_project_file(&project_path, &params.file_path)?;

    let content = std::fs::read_to_string(&abs_path)
        .map_err(|e| anyhow::anyhow!("Cannot read {:?}: {}", abs_path, e))?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u32;

    // Clamp to actual file length (1-indexed → 0-indexed)
    let from = (params.line_start - 1).min(total_lines) as usize;
    let to = effective_end.min(total_lines) as usize;

    let source = lines[from..to].join("\n");

    let mut resp = json!({
        "file": params.file_path,
        "line_start": params.line_start,
        "line_end": to as u32,   // actual end after clamping
        "total_file_lines": total_lines,
        "source": source,
    });

    if truncated {
        resp["truncated"] = json!(true);
        resp["truncated_note"] =
            json!(format!(
            "Requested {} lines; returned first {}. Call again with line_start: {} to continue.",
            requested, MAX_LINES, effective_end + 1
        ));
    }

    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_file(dir: &TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        dir.path().to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn test_get_lines_basic() {
        let dir = TempDir::new().unwrap();
        let project = make_file(&dir, "lib.rs", "line1\nline2\nline3\nline4\nline5");

        let result = get_lines(GetLinesParams {
            project,
            file_path: "lib.rs".to_string(),
            line_start: 2,
            line_end: 4,
        })
        .await
        .unwrap();

        assert_eq!(result["source"].as_str().unwrap(), "line2\nline3\nline4");
        assert_eq!(result["line_start"].as_u64().unwrap(), 2);
        assert_eq!(result["line_end"].as_u64().unwrap(), 4);
    }

    #[tokio::test]
    async fn test_get_lines_clamps_to_file_end() {
        let dir = TempDir::new().unwrap();
        let project = make_file(&dir, "lib.rs", "a\nb\nc");

        let result = get_lines(GetLinesParams {
            project,
            file_path: "lib.rs".to_string(),
            line_start: 2,
            line_end: 999,
        })
        .await
        .unwrap();

        assert_eq!(result["source"].as_str().unwrap(), "b\nc");
        assert_eq!(result["line_end"].as_u64().unwrap(), 3);
    }

    #[tokio::test]
    async fn test_get_lines_truncates_at_cap() {
        let dir = TempDir::new().unwrap();
        let content: String = (1..=600).map(|i| format!("line{i}\n")).collect();
        let project = make_file(&dir, "big.rs", &content);

        let result = get_lines(GetLinesParams {
            project,
            file_path: "big.rs".to_string(),
            line_start: 1,
            line_end: 600,
        })
        .await
        .unwrap();

        assert!(result["truncated"].as_bool().unwrap());
        let returned_lines = result["source"].as_str().unwrap().lines().count();
        assert_eq!(returned_lines, MAX_LINES as usize);
    }

    #[tokio::test]
    async fn test_get_lines_start_zero_errors() {
        let dir = TempDir::new().unwrap();
        let project = make_file(&dir, "lib.rs", "hello");

        let err = get_lines(GetLinesParams {
            project,
            file_path: "lib.rs".to_string(),
            line_start: 0,
            line_end: 5,
        })
        .await;

        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_get_lines_start_after_end_errors() {
        let dir = TempDir::new().unwrap();
        let project = make_file(&dir, "lib.rs", "hello");

        let err = get_lines(GetLinesParams {
            project,
            file_path: "lib.rs".to_string(),
            line_start: 5,
            line_end: 2,
        })
        .await;

        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_get_lines_absolute_path() {
        let dir = TempDir::new().unwrap();
        let abs = dir.path().join("lib.rs");
        std::fs::write(&abs, "hello\nworld").unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let err = get_lines(GetLinesParams {
            project,
            file_path: abs.to_string_lossy().to_string(),
            line_start: 1,
            line_end: 2,
        })
        .await
        .unwrap_err();

        let err = err.downcast::<crate::error::ToolError>().unwrap();
        assert!(matches!(err, crate::error::ToolError::AccessDenied { .. }));
    }

    #[tokio::test]
    async fn test_get_lines_parent_escape_rejected() {
        let dir = TempDir::new().unwrap();
        let project = make_file(&dir, "lib.rs", "hello");

        let err = get_lines(GetLinesParams {
            project,
            file_path: "../secret.rs".to_string(),
            line_start: 1,
            line_end: 1,
        })
        .await
        .unwrap_err();

        let err = err.downcast::<crate::error::ToolError>().unwrap();
        assert!(matches!(err, crate::error::ToolError::AccessDenied { .. }));
    }
}
