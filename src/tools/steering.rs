use serde_json::{json, Value};

pub fn attach_steering(response: &mut Value, steering: Value) {
    response["steering"] = steering;
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
