use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    LazyLock, RwLock,
};

use crate::sync_utils::{rw_read, rw_write};

const MAX_RECENT_ITEMS: usize = 16;
const RECENT_QUERY_LIMIT: usize = 12;

#[derive(Default)]
struct ProjectSessionState {
    recent_files: HashMap<String, u64>,
    recent_symbols: HashMap<String, u64>,
    recent_dirs: HashMap<String, u64>,
    recent_queries: VecDeque<(String, u64)>,
    recent_content: HashMap<String, u64>,
    recent_target_content: HashMap<String, (String, u64)>,
}

pub struct ContentObservation {
    pub content_seen: bool,
    pub target_seen: bool,
    pub changed_since_last_read: bool,
}

static SESSION: LazyLock<RwLock<HashMap<PathBuf, ProjectSessionState>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static CLOCK: AtomicU64 = AtomicU64::new(1);

fn next_tick() -> u64 {
    CLOCK.fetch_add(1, Ordering::Relaxed)
}

fn normalise(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != ':')
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

fn with_state_mut(project: &Path, f: impl FnOnce(&mut ProjectSessionState)) {
    let mut guard = rw_write(&SESSION);
    let state = guard.entry(project.to_path_buf()).or_default();
    f(state);
}

fn with_state<T>(project: &Path, f: impl FnOnce(&ProjectSessionState) -> T) -> T {
    let guard = rw_read(&SESSION);
    match guard.get(project) {
        Some(state) => f(state),
        None => f(&ProjectSessionState::default()),
    }
}

pub fn record_query(project: &Path, query: &str) {
    let query = query.trim();
    if query.is_empty() {
        return;
    }
    let tick = next_tick();
    let query = query.to_lowercase();
    with_state_mut(project, |state| {
        state.recent_queries.push_front((query, tick));
        while state.recent_queries.len() > RECENT_QUERY_LIMIT {
            state.recent_queries.pop_back();
        }
    });
}

pub fn record_file(project: &Path, file: &Path) {
    let tick = next_tick();
    let file = normalise(file);
    let dir = Path::new(&file)
        .parent()
        .map(normalise)
        .unwrap_or_else(|| ".".to_string());
    with_state_mut(project, |state| {
        state.recent_files.insert(file.clone(), tick);
        state.recent_dirs.insert(dir, tick);
        prune_recent(state);
    });
}

pub fn record_symbol(project: &Path, symbol_id: &str, file: Option<&Path>) {
    let tick = next_tick();
    with_state_mut(project, |state| {
        state.recent_symbols.insert(symbol_id.to_string(), tick);
        if let Some(file) = file {
            let file = normalise(file);
            let dir = Path::new(&file)
                .parent()
                .map(normalise)
                .unwrap_or_else(|| ".".to_string());
            state.recent_files.insert(file, tick);
            state.recent_dirs.insert(dir, tick);
        }
        prune_recent(state);
    });
}

pub fn record_files(project: &Path, files: impl IntoIterator<Item = String>) {
    for file in files {
        record_file(project, Path::new(&file));
    }
}

pub fn record_symbols(project: &Path, symbols: impl IntoIterator<Item = (String, Option<String>)>) {
    for (symbol_id, file) in symbols {
        let file_ref = file.as_deref().map(Path::new);
        record_symbol(project, &symbol_id, file_ref);
    }
}

pub fn record_content(project: &Path, namespace: &str, identity: &str, content: &str) -> bool {
    observe_content(project, namespace, identity, content).content_seen
}

pub fn observe_content(
    project: &Path,
    namespace: &str,
    identity: &str,
    content: &str,
) -> ContentObservation {
    let tick = next_tick();
    let key = content_key(namespace, identity, content);
    let target_key = format!("{namespace}:{identity}");
    let digest = blake3::hash(content.as_bytes()).to_hex().to_string();
    let mut observation = ContentObservation {
        content_seen: false,
        target_seen: false,
        changed_since_last_read: false,
    };
    with_state_mut(project, |state| {
        observation.content_seen = state.recent_content.contains_key(&key);
        if let Some((previous_digest, _)) = state.recent_target_content.get(&target_key) {
            observation.target_seen = true;
            observation.changed_since_last_read = previous_digest != &digest;
        }
        state.recent_content.insert(key, tick);
        state
            .recent_target_content
            .insert(target_key, (digest, tick));
        prune_recent(state);
    });
    observation
}

pub fn has_seen_file(project: &Path, file: &Path) -> bool {
    let file = normalise(file);
    with_state(project, |state| state.recent_files.contains_key(&file))
}

pub fn has_seen_symbol(project: &Path, symbol_id: &str) -> bool {
    with_state(project, |state| {
        state.recent_symbols.contains_key(symbol_id)
    })
}

pub fn file_boost(project: &Path, file: &Path) -> i32 {
    with_state(project, |state| {
        let now = CLOCK.load(Ordering::Relaxed);
        file_boost_from_state(state, file, now)
    })
}

pub fn directory_boost(project: &Path, dir: &Path) -> i32 {
    with_state(project, |state| {
        let now = CLOCK.load(Ordering::Relaxed);
        directory_boost_from_state(state, dir, now)
    })
}

pub fn symbol_boost(project: &Path, symbol_id: &str, file: Option<&Path>) -> i32 {
    with_state(project, |state| {
        let now = CLOCK.load(Ordering::Relaxed);
        let mut boost = score_recent(state.recent_symbols.get(symbol_id).copied(), now, 20);
        if let Some(file) = file {
            boost = boost.max(file_boost_from_state(state, file, now));
        }
        boost += query_overlap_boost(state, symbol_id);
        boost
    })
}

pub fn query_boost(project: &Path, current_query: &str) -> i32 {
    with_state(project, |state| query_overlap_boost(state, current_query))
}

fn query_overlap_boost(state: &ProjectSessionState, current: &str) -> i32 {
    let current_tokens = tokenize(current);
    if current_tokens.is_empty() {
        return 0;
    }

    let mut best = 0;
    for (recent, _) in &state.recent_queries {
        let recent_tokens = tokenize(recent);
        if recent_tokens.is_empty() {
            continue;
        }
        let overlap = current_tokens
            .iter()
            .filter(|token| recent_tokens.contains(token))
            .count();
        if overlap > 0 {
            best = best.max(3 + overlap as i32 * 2);
        }
    }
    best
}

fn file_ancestors(path: &str) -> Vec<String> {
    let mut ancestors = Vec::new();
    let mut current = Path::new(path).parent();
    while let Some(parent) = current {
        let rel = normalise(parent);
        if rel.is_empty() || rel == "." {
            break;
        }
        ancestors.push(rel);
        current = parent.parent();
    }
    ancestors
}

fn file_boost_from_state(state: &ProjectSessionState, file: &Path, now: u64) -> i32 {
    let file = normalise(file);
    let mut boost = score_recent(state.recent_files.get(&file).copied(), now, 18);

    for ancestor in file_ancestors(&file) {
        boost = boost.max(score_recent(
            state.recent_dirs.get(&ancestor).copied(),
            now,
            12,
        ));
    }

    boost + query_overlap_boost(state, &file)
}

fn directory_boost_from_state(state: &ProjectSessionState, dir: &Path, now: u64) -> i32 {
    let dir = normalise(dir);
    let mut boost = score_recent(state.recent_dirs.get(&dir).copied(), now, 14);

    for ancestor in file_ancestors(&dir) {
        boost = boost.max(score_recent(
            state.recent_dirs.get(&ancestor).copied(),
            now,
            10,
        ));
    }

    let child_prefix = format!("{dir}/");
    for (recent_dir, seen) in &state.recent_dirs {
        if recent_dir.starts_with(&child_prefix) {
            boost = boost.max(score_recent(Some(*seen), now, 12));
        }
    }

    boost + query_overlap_boost(state, &dir)
}

fn score_recent(last_seen: Option<u64>, now: u64, base: i32) -> i32 {
    let Some(seen) = last_seen else {
        return 0;
    };
    let age = now.saturating_sub(seen) as i32;
    (base - age * 2).max(0)
}

fn prune_recent(state: &mut ProjectSessionState) {
    if state.recent_files.len() > MAX_RECENT_ITEMS {
        let mut entries: Vec<(String, u64)> = state
            .recent_files
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        state.recent_files = entries
            .into_iter()
            .take(MAX_RECENT_ITEMS)
            .collect::<HashMap<_, _>>();
    }
    if state.recent_symbols.len() > MAX_RECENT_ITEMS {
        let mut entries: Vec<(String, u64)> = state
            .recent_symbols
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        state.recent_symbols = entries
            .into_iter()
            .take(MAX_RECENT_ITEMS)
            .collect::<HashMap<_, _>>();
    }
    if state.recent_dirs.len() > MAX_RECENT_ITEMS {
        let mut entries: Vec<(String, u64)> = state
            .recent_dirs
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        state.recent_dirs = entries
            .into_iter()
            .take(MAX_RECENT_ITEMS)
            .collect::<HashMap<_, _>>();
    }
    while state.recent_queries.len() > RECENT_QUERY_LIMIT {
        state.recent_queries.pop_back();
    }
    if state.recent_content.len() > MAX_RECENT_ITEMS {
        let mut entries: Vec<(String, u64)> = state
            .recent_content
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        state.recent_content = entries
            .into_iter()
            .take(MAX_RECENT_ITEMS)
            .collect::<HashMap<_, _>>();
    }
    if state.recent_target_content.len() > MAX_RECENT_ITEMS {
        let mut entries: Vec<(String, (String, u64))> = state
            .recent_target_content
            .iter()
            .map(|(k, (digest, tick))| (k.clone(), (digest.clone(), *tick)))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1 .1));
        state.recent_target_content = entries
            .into_iter()
            .take(MAX_RECENT_ITEMS)
            .collect::<HashMap<_, _>>();
    }
}

fn content_key(namespace: &str, identity: &str, content: &str) -> String {
    let digest = blake3::hash(content.as_bytes()).to_hex();
    format!("{namespace}:{identity}:{digest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_project(prefix: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("/tmp/{prefix}-{}", next_tick()))
    }

    #[test]
    fn test_file_boost_prefers_recent_file_and_dir() {
        let mut state = ProjectSessionState::default();
        let file = Path::new("src/tools/search.rs");
        state.recent_files.insert(normalise(file), 10);
        state.recent_dirs.insert("src/tools".to_string(), 10);

        assert!(file_boost_from_state(&state, file, 11) > 0);
        assert!(file_boost_from_state(&state, Path::new("src/tools/other.rs"), 11) > 0);
    }

    #[test]
    fn test_symbol_boost_prefers_recent_symbol() {
        let project = unique_project("session-test-symbol");
        record_symbol(&project, "abc", Some(Path::new("src/lib.rs")));
        assert!(symbol_boost(&project, "abc", Some(Path::new("src/lib.rs"))) > 0);
    }

    #[test]
    fn test_directory_boost_prefers_recent_directory() {
        let mut state = ProjectSessionState::default();
        state.recent_dirs.insert("src/tools".to_string(), 10);

        assert!(directory_boost_from_state(&state, Path::new("src/tools"), 11) > 0);
        assert!(directory_boost_from_state(&state, Path::new("src"), 11) > 0);
    }

    #[test]
    fn test_record_content_reports_repeated_reads() {
        let project = unique_project("session-test-content");
        let first = record_content(&project, "symbol", "abc", "fn alpha() {}");
        let second = record_content(&project, "symbol", "abc", "fn alpha() {}");

        assert!(!first);
        assert!(second);
    }

    #[test]
    fn test_observe_content_reports_changed_target() {
        let project = unique_project("session-test-diff");
        let first = observe_content(&project, "symbol", "abc", "fn alpha() {}");
        let second = observe_content(&project, "symbol", "abc", "fn beta() {}");

        assert!(!first.content_seen);
        assert!(!first.target_seen);
        assert!(!first.changed_since_last_read);
        assert!(!second.content_seen);
        assert!(second.target_seen);
        assert!(second.changed_since_last_read);
    }

    #[test]
    fn test_has_seen_symbol_and_file_track_exact_targets() {
        let project = unique_project("session-test-seen");
        record_symbol(&project, "abc", Some(Path::new("src/lib.rs")));

        assert!(has_seen_symbol(&project, "abc"));
        assert!(!has_seen_symbol(&project, "def"));
        assert!(has_seen_file(&project, Path::new("src/lib.rs")));
        assert!(!has_seen_file(&project, Path::new("src/other.rs")));
    }
}
