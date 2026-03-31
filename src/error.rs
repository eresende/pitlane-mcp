/// Structured, machine-readable errors returned by all MCP tools.
///
/// Every variant serialises to a JSON object:
/// ```json
/// {
///   "error": {
///     "code": "PROJECT_NOT_INDEXED",
///     "message": "Project '/tmp/foo' has not been indexed yet.",
///     "hint": "Call index_project first."
///   }
/// }
/// ```
use serde_json::{json, Value};

#[derive(Debug)]
pub enum ToolError {
    /// The caller queried a project that has no index on disk or in cache.
    ProjectNotIndexed { project: String },
    /// A symbol ID was supplied that does not exist in the index.
    SymbolNotFound { symbol_id: String },
    /// A parameter value was syntactically or semantically invalid.
    InvalidArgument { param: String, message: String },
    /// Catch-all for unexpected I/O or internal failures.
    Internal { message: String },
}

impl ToolError {
    pub fn code(&self) -> &'static str {
        match self {
            ToolError::ProjectNotIndexed { .. } => "PROJECT_NOT_INDEXED",
            ToolError::SymbolNotFound { .. } => "SYMBOL_NOT_FOUND",
            ToolError::InvalidArgument { .. } => "INVALID_ARGUMENT",
            ToolError::Internal { .. } => "INTERNAL_ERROR",
        }
    }

    pub fn hint(&self) -> &'static str {
        match self {
            ToolError::ProjectNotIndexed { .. } => "Call index_project first.",
            ToolError::SymbolNotFound { .. } => {
                "Use search_symbols or get_file_outline to obtain a valid symbol ID."
            }
            ToolError::InvalidArgument { .. } => {
                "Check the parameter value and consult the tool description."
            }
            ToolError::Internal { .. } => "Check the project path and try again.",
        }
    }

    /// Serialise to the structured JSON error envelope.
    pub fn to_json(&self) -> Value {
        json!({
            "error": {
                "code": self.code(),
                "message": self.to_string(),
                "hint": self.hint(),
            }
        })
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::ProjectNotIndexed { project } => {
                write!(f, "Project '{}' has not been indexed yet.", project)
            }
            ToolError::SymbolNotFound { symbol_id } => {
                write!(f, "Symbol '{}' not found in the index.", symbol_id)
            }
            ToolError::InvalidArgument { param, message } => {
                write!(f, "Invalid value for '{}': {}", param, message)
            }
            ToolError::Internal { message } => write!(f, "{}", message),
        }
    }
}

impl std::error::Error for ToolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_not_indexed_code_and_hint() {
        let e = ToolError::ProjectNotIndexed {
            project: "/tmp/foo".to_string(),
        };
        assert_eq!(e.code(), "PROJECT_NOT_INDEXED");
        assert_eq!(e.hint(), "Call index_project first.");
    }

    #[test]
    fn test_symbol_not_found_code_and_hint() {
        let e = ToolError::SymbolNotFound {
            symbol_id: "src/lib.rs::foo#function".to_string(),
        };
        assert_eq!(e.code(), "SYMBOL_NOT_FOUND");
        assert!(e.hint().contains("search_symbols"));
    }

    #[test]
    fn test_invalid_argument_code_and_hint() {
        let e = ToolError::InvalidArgument {
            param: "language".to_string(),
            message: "Unknown language 'cobol'".to_string(),
        };
        assert_eq!(e.code(), "INVALID_ARGUMENT");
        assert!(e.hint().contains("parameter"));
    }

    #[test]
    fn test_internal_error_code_and_hint() {
        let e = ToolError::Internal {
            message: "disk exploded".to_string(),
        };
        assert_eq!(e.code(), "INTERNAL_ERROR");
        assert!(e.hint().contains("project path"));
    }

    #[test]
    fn test_display_includes_project_path() {
        let e = ToolError::ProjectNotIndexed {
            project: "/tmp/myproject".to_string(),
        };
        assert!(e.to_string().contains("/tmp/myproject"));
    }

    #[test]
    fn test_display_includes_symbol_id() {
        let e = ToolError::SymbolNotFound {
            symbol_id: "src/lib.rs::bar#method".to_string(),
        };
        assert!(e.to_string().contains("src/lib.rs::bar#method"));
    }

    #[test]
    fn test_display_includes_param_and_message() {
        let e = ToolError::InvalidArgument {
            param: "kind".to_string(),
            message: "Unknown kind 'widget'".to_string(),
        };
        let s = e.to_string();
        assert!(s.contains("kind"));
        assert!(s.contains("Unknown kind 'widget'"));
    }

    #[test]
    fn test_to_json_structure() {
        let e = ToolError::ProjectNotIndexed {
            project: "/tmp/foo".to_string(),
        };
        let v = e.to_json();
        let err = &v["error"];
        assert_eq!(err["code"].as_str().unwrap(), "PROJECT_NOT_INDEXED");
        assert!(err["message"].as_str().unwrap().contains("/tmp/foo"));
        assert_eq!(err["hint"].as_str().unwrap(), "Call index_project first.");
    }

    #[test]
    fn test_to_json_all_variants_have_required_fields() {
        let variants: Vec<ToolError> = vec![
            ToolError::ProjectNotIndexed { project: "p".to_string() },
            ToolError::SymbolNotFound { symbol_id: "s".to_string() },
            ToolError::InvalidArgument { param: "x".to_string(), message: "bad".to_string() },
            ToolError::Internal { message: "oops".to_string() },
        ];
        for e in variants {
            let v = e.to_json();
            let err = &v["error"];
            assert!(err["code"].is_string(), "missing code for {:?}", e);
            assert!(err["message"].is_string(), "missing message for {:?}", e);
            assert!(err["hint"].is_string(), "missing hint for {:?}", e);
        }
    }
}
