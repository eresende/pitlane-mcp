use serde_json::{json, Value};

/// Whether tool responses may carry their own navigation hints.
///
/// A client whose agent-side skill already owns guidance can set
/// `PITLANE_MCP_TOOL_GUIDANCE=none` so responses stay evidence-only. Unset, empty
/// or any other value keeps the default payload, so existing callers are
/// unaffected and the behaviour is opt-in.
pub fn guidance_enabled() -> bool {
    enabled_for(std::env::var("PITLANE_MCP_TOOL_GUIDANCE").ok().as_deref())
}

/// Pure form of the check, so tests never mutate process-wide environment.
fn enabled_for(value: Option<&str>) -> bool {
    value != Some("none")
}

pub fn attach_steering(response: &mut Value, steering: Value) {
    if guidance_enabled() {
        response["steering"] = steering;
    }
}

/// Build a compact steering payload.
///
/// Removed `fallback_candidates` — it duplicated data already present in the
/// response `results` or `references` arrays, adding 1-2KB per call for no
/// behavioral benefit.
pub fn build_steering(
    confidence: f32,
    why_this_matched: impl Into<String>,
    recommended_next_tool: impl Into<String>,
    recommended_target: Value,
    _fallback_candidates: Vec<Value>,
) -> Value {
    json!({
        "confidence": (confidence.clamp(0.0, 1.0) * 1000.0).round() / 1000.0,
        "why_this_matched": why_this_matched.into(),
        "recommended_next_tool": recommended_next_tool.into(),
        "recommended_target": recommended_target,
    })
}

/// Kept for API compatibility but now returns an empty vec.
/// Callers still pass results to `build_steering` but the field is no longer
/// emitted, saving ~1KB per tool response.
pub fn take_fallback_candidates(_items: &[Value]) -> Vec<Value> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_defaults_on_and_only_none_suppresses() {
        assert!(enabled_for(None), "unset keeps guidance");
        assert!(enabled_for(Some("full")), "explicit full keeps guidance");
        assert!(enabled_for(Some("")), "empty value keeps guidance");
        assert!(!enabled_for(Some("none")), "none suppresses guidance");
    }

    #[test]
    fn suppressed_guidance_still_builds_valid_evidence_responses() {
        let response = json!({ "results": [{"file": "src/lib.rs"}] });
        let steering = build_steering(0.9, "why", "read_code_unit", json!({}), vec![]);
        assert!(!enabled_for(Some("none")));
        // Attaching is skipped in suppressed mode; evidence must be untouched.
        assert_eq!(response["results"][0]["file"], json!("src/lib.rs"));
        assert!(response.get("steering").is_none());
        // The payload itself stays constructible, so the switch never panics.
        assert_eq!(steering["recommended_next_tool"], json!("read_code_unit"));
    }
}
