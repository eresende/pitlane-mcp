use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct ProjectStats {
    pub get_symbol_calls: u64,
    pub signature_only_applied: u64,
    /// Raw bytes that would have been returned without signature-only.
    pub full_source_bytes: u64,
    /// Raw bytes actually returned to the caller.
    pub returned_bytes: u64,
}

impl ProjectStats {
    pub fn tokens_saved_approx(&self) -> u64 {
        self.full_source_bytes.saturating_sub(self.returned_bytes) / 4
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct UsageStats {
    pub total: ProjectStats,
    pub by_project: HashMap<String, ProjectStats>,
}

fn pitlane_home() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| PathBuf::from(h).join(".pitlane"))
}

fn stats_path() -> Option<PathBuf> {
    pitlane_home().map(|h| h.join("stats.json"))
}

pub fn load_stats() -> UsageStats {
    let path = match stats_path() {
        Some(p) => p,
        None => return UsageStats::default(),
    };
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return UsageStats::default(),
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_stats(stats: &UsageStats) {
    let path = match stats_path() {
        Some(p) => p,
        None => return,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(stats) {
        let _ = std::fs::write(path, data);
    }
}

fn tally(
    entry: &mut ProjectStats,
    signature_only_applied: bool,
    full_source_bytes: u64,
    returned_bytes: u64,
) {
    entry.get_symbol_calls += 1;
    entry.full_source_bytes += full_source_bytes;
    entry.returned_bytes += returned_bytes;
    if signature_only_applied {
        entry.signature_only_applied += 1;
    }
}

/// Record one `get_symbol` call. Never propagates errors — stats are best-effort.
pub fn record_get_symbol(
    project: &str,
    signature_only_applied: bool,
    full_source_bytes: u64,
    returned_bytes: u64,
) {
    let mut stats = load_stats();
    tally(
        &mut stats.total,
        signature_only_applied,
        full_source_bytes,
        returned_bytes,
    );
    tally(
        stats.by_project.entry(project.to_string()).or_default(),
        signature_only_applied,
        full_source_bytes,
        returned_bytes,
    );
    save_stats(&stats);
}
