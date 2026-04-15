use serde_json::{json, Value};

const DEFAULT_FALLBACK_COUNT: usize = 3;

pub fn attach_steering(response: &mut Value, steering: Value) {
    response["steering"] = steering;
}

pub fn build_steering(
    confidence: f32,
    why_this_matched: impl Into<String>,
    recommended_next_tool: impl Into<String>,
    recommended_target: Value,
    fallback_candidates: Vec<Value>,
) -> Value {
    json!({
        "confidence": (confidence.clamp(0.0, 1.0) * 1000.0).round() / 1000.0,
        "why_this_matched": why_this_matched.into(),
        "recommended_next_tool": recommended_next_tool.into(),
        "recommended_target": recommended_target,
        "fallback_candidates": fallback_candidates,
    })
}

pub fn take_fallback_candidates(items: &[Value]) -> Vec<Value> {
    items.iter().take(DEFAULT_FALLBACK_COUNT).cloned().collect()
}
