use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::index::SymbolIndex;
use crate::indexer::language::SymbolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RepoArchetype {
    Cli,
    Library,
    Service,
    Frontend,
    Infra,
    Monorepo,
    TestHeavy,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PathRole {
    Entrypoint,
    Bootstrap,
    Cli,
    Config,
    Handler,
    Service,
    Infra,
    Test,
    Library,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoProfile {
    pub archetype: RepoArchetype,
    pub file_roles: HashMap<String, PathRole>,
    pub role_counts: HashMap<PathRole, usize>,
    pub entrypoints: Vec<String>,
}

pub fn build_repo_profile(project_path: &Path, index: &SymbolIndex) -> RepoProfile {
    let mut profile = RepoProfile::default();
    let mut role_counts: HashMap<PathRole, usize> = HashMap::new();
    let mut entrypoints = Vec::new();

    for file in index.by_file.keys() {
        let rel = file.strip_prefix(project_path).unwrap_or(file.as_path());
        let rel_str = normalise_path(rel);
        let role = classify_path_role(rel, index);
        profile.file_roles.insert(rel_str.clone(), role);
        *role_counts.entry(role).or_insert(0) += 1;

        if matches!(
            role,
            PathRole::Entrypoint | PathRole::Bootstrap | PathRole::Cli
        ) {
            entrypoints.push(rel_str);
        }
    }

    profile.role_counts = role_counts;
    profile.entrypoints = entrypoints;
    profile.archetype = classify_repo_archetype(index, &profile);
    profile
}

pub fn classify_path_role(path: &Path, _index: &SymbolIndex) -> PathRole {
    let rel = normalise_path(path);
    let lower = rel.to_lowercase();
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    if is_test_path(&lower, &file_name) {
        return PathRole::Test;
    }
    if is_config_path(&lower, &file_name) {
        return PathRole::Config;
    }
    if is_handler_path(&lower, &file_name) {
        return PathRole::Handler;
    }
    if is_infra_path(&lower, &file_name) {
        return PathRole::Infra;
    }
    if is_entrypoint_path(&lower, &file_name) {
        return PathRole::Entrypoint;
    }
    if is_cli_path(&lower, &file_name) {
        return PathRole::Cli;
    }
    if is_bootstrap_path(&lower, &file_name) {
        return PathRole::Bootstrap;
    }
    if file_name == "lib.rs" {
        return PathRole::Library;
    }
    if is_service_path(&lower, &file_name) {
        return PathRole::Service;
    }
    PathRole::Unknown
}

pub fn role_boost(role: PathRole, archetype: RepoArchetype, query: &str) -> i32 {
    let query_lower = query.to_lowercase();
    let tracey = query_lower.contains("trace")
        || query_lower.contains("flow")
        || query_lower.contains("path")
        || query_lower.contains("call");
    let impact = query_lower.contains("impact")
        || query_lower.contains("blast")
        || query_lower.contains("break")
        || query_lower.contains("refactor");
    let configy = query_lower.contains("config")
        || query_lower.contains("env")
        || query_lower.contains("setting")
        || query_lower.contains("option");

    let mut boost = match role {
        PathRole::Entrypoint => 14,
        PathRole::Bootstrap => 12,
        PathRole::Cli => 13,
        PathRole::Config => 10,
        PathRole::Handler => 11,
        PathRole::Service => 9,
        PathRole::Infra => 8,
        PathRole::Test => 5,
        PathRole::Library => 3,
        PathRole::Unknown => 0,
    };

    boost += match archetype {
        RepoArchetype::Cli
            if matches!(
                role,
                PathRole::Entrypoint | PathRole::Cli | PathRole::Bootstrap
            ) =>
        {
            8
        }
        RepoArchetype::Service
            if matches!(
                role,
                PathRole::Handler | PathRole::Bootstrap | PathRole::Config | PathRole::Service
            ) =>
        {
            8
        }
        RepoArchetype::Frontend
            if matches!(
                role,
                PathRole::Entrypoint | PathRole::Bootstrap | PathRole::Handler | PathRole::Config
            ) =>
        {
            6
        }
        RepoArchetype::Infra
            if matches!(
                role,
                PathRole::Infra | PathRole::Config | PathRole::Bootstrap
            ) =>
        {
            8
        }
        RepoArchetype::Library if matches!(role, PathRole::Library | PathRole::Bootstrap) => 6,
        RepoArchetype::TestHeavy if matches!(role, PathRole::Test) => 10,
        _ => 0,
    };

    if tracey
        && matches!(
            role,
            PathRole::Entrypoint | PathRole::Bootstrap | PathRole::Cli
        )
    {
        boost += 8;
    }
    if impact
        && matches!(
            role,
            PathRole::Test | PathRole::Config | PathRole::Bootstrap
        )
    {
        boost += 4;
    }
    if configy && matches!(role, PathRole::Config | PathRole::Bootstrap) {
        boost += 6;
    }

    boost
}

pub fn archetype_label(archetype: RepoArchetype) -> &'static str {
    match archetype {
        RepoArchetype::Cli => "cli",
        RepoArchetype::Library => "library",
        RepoArchetype::Service => "service",
        RepoArchetype::Frontend => "frontend",
        RepoArchetype::Infra => "infra",
        RepoArchetype::Monorepo => "monorepo",
        RepoArchetype::TestHeavy => "test_heavy",
        RepoArchetype::Unknown => "unknown",
    }
}

pub fn role_label(role: PathRole) -> &'static str {
    match role {
        PathRole::Entrypoint => "entrypoint",
        PathRole::Bootstrap => "bootstrap",
        PathRole::Cli => "cli",
        PathRole::Config => "config",
        PathRole::Handler => "handler",
        PathRole::Service => "service",
        PathRole::Infra => "infra",
        PathRole::Test => "test",
        PathRole::Library => "library",
        PathRole::Unknown => "unknown",
    }
}

fn classify_repo_archetype(index: &SymbolIndex, profile: &RepoProfile) -> RepoArchetype {
    let file_count = index.file_count().max(1);
    let test_count = *profile.role_counts.get(&PathRole::Test).unwrap_or(&0);
    if test_count * 3 >= file_count {
        return RepoArchetype::TestHeavy;
    }

    let serviceish = count_role(
        profile,
        &[PathRole::Handler, PathRole::Service, PathRole::Config],
    );
    let cliish = count_role(
        profile,
        &[PathRole::Entrypoint, PathRole::Cli, PathRole::Bootstrap],
    );
    let infraish = count_role(profile, &[PathRole::Infra]);
    let frontendish = count_frontend_signals(index);
    let libraryish = count_role(profile, &[PathRole::Library]);

    if infraish >= 3 && infraish >= serviceish {
        RepoArchetype::Infra
    } else if frontendish >= 3 && frontendish >= serviceish {
        RepoArchetype::Frontend
    } else if serviceish >= 3 && serviceish >= cliish {
        RepoArchetype::Service
    } else if cliish >= 1 && cliish >= libraryish {
        RepoArchetype::Cli
    } else if libraryish >= 1 && serviceish == 0 && cliish == 0 && infraish == 0 && frontendish == 0
    {
        RepoArchetype::Library
    } else if index.file_count() > 1 && profile.entrypoints.len() > 1 {
        RepoArchetype::Monorepo
    } else {
        RepoArchetype::Unknown
    }
}

fn count_role(profile: &RepoProfile, roles: &[PathRole]) -> usize {
    roles
        .iter()
        .map(|role| profile.role_counts.get(role).copied().unwrap_or(0))
        .sum()
}

fn count_frontend_signals(index: &SymbolIndex) -> usize {
    let mut count = 0usize;
    for file in index.by_file.keys() {
        let lower = normalise_path(file).to_lowercase();
        if lower.ends_with(".tsx")
            || lower.ends_with(".jsx")
            || lower.ends_with(".svelte")
            || lower.contains("/components/")
            || lower.contains("/pages/")
            || lower.contains("/app/")
            || lower.contains("/ui/")
        {
            count += 1;
        }
    }
    count
}

fn is_test_path(path: &str, file_name: &str) -> bool {
    has_dir_marker(path, "tests")
        || has_dir_marker(path, "test")
        || has_dir_marker(path, "spec")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with("_spec.rs")
        || file_name.ends_with(".spec.ts")
        || file_name.ends_with(".spec.tsx")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".test.tsx")
        || file_name.ends_with("tests.rs")
        || file_name.starts_with("test_")
}

fn is_config_path(path: &str, file_name: &str) -> bool {
    has_dir_marker(path, "config")
        || has_dir_marker(path, "configs")
        || has_dir_marker(path, "settings")
        || has_dir_marker(path, "env")
        || has_dir_marker(path, "options")
        || file_name == "config.rs"
        || file_name == "config.ts"
        || file_name == "settings.rs"
        || file_name.ends_with(".toml")
        || file_name.ends_with(".yaml")
        || file_name.ends_with(".yml")
        || file_name.ends_with(".json")
}

fn is_handler_path(path: &str, file_name: &str) -> bool {
    has_dir_marker(path, "handler")
        || has_dir_marker(path, "handlers")
        || has_dir_marker(path, "route")
        || has_dir_marker(path, "routes")
        || has_dir_marker(path, "controller")
        || has_dir_marker(path, "controllers")
        || has_dir_marker(path, "api")
        || file_name.contains("handler")
        || file_name.contains("route")
        || file_name.contains("controller")
}

fn is_infra_path(path: &str, file_name: &str) -> bool {
    has_dir_marker(path, "infra")
        || has_dir_marker(path, "db")
        || has_dir_marker(path, "storage")
        || has_dir_marker(path, "repo")
        || has_dir_marker(path, "client")
        || has_dir_marker(path, "queue")
        || has_dir_marker(path, "cache")
        || has_dir_marker(path, "fs")
        || file_name.contains("client")
        || file_name.contains("repo")
        || file_name.contains("storage")
}

fn is_cli_path(path: &str, file_name: &str) -> bool {
    has_dir_marker(path, "bin")
        || has_dir_marker(path, "cmd")
        || has_dir_marker(path, "cli")
        || file_name == "cli.rs"
}

fn is_bootstrap_path(path: &str, file_name: &str) -> bool {
    file_name == "main.rs"
        || file_name == "app.rs"
        || file_name == "server.rs"
        || file_name == "index.ts"
        || file_name == "index.js"
        || has_dir_marker(path, "bootstrap")
}

fn is_entrypoint_path(path: &str, file_name: &str) -> bool {
    file_name == "main.rs"
        || file_name == "main.ts"
        || file_name == "main.js"
        || file_name == "main.py"
        || file_name == "main.go"
        || file_name == "index.ts"
        || file_name == "index.js"
        || path.contains("/bin/")
}

fn is_service_path(path: &str, file_name: &str) -> bool {
    has_dir_marker(path, "service")
        || has_dir_marker(path, "services")
        || has_dir_marker(path, "domain")
        || has_dir_marker(path, "worker")
        || has_dir_marker(path, "task")
        || has_dir_marker(path, "job")
        || file_name.contains("service")
        || file_name.contains("worker")
        || file_name.contains("job")
}

fn has_dir_marker(path: &str, marker: &str) -> bool {
    path == marker
        || path.starts_with(&format!("{marker}/"))
        || path.contains(&format!("/{marker}/"))
}

fn normalise_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub fn path_role_for_file(
    project_path: &Path,
    file_path: &Path,
    profile: Option<&RepoProfile>,
) -> PathRole {
    let rel = file_path.strip_prefix(project_path).unwrap_or(file_path);
    let rel_str = normalise_path(rel);
    if let Some(profile) = profile {
        if let Some(role) = profile.file_roles.get(&rel_str) {
            return *role;
        }
    }
    classify_path_role(rel, &SymbolIndex::default())
}

pub fn profile_file_role(
    profile: Option<&RepoProfile>,
    project_path: &Path,
    file_path: &Path,
) -> Option<PathRole> {
    let rel = file_path.strip_prefix(project_path).unwrap_or(file_path);
    let rel_str = normalise_path(rel);
    profile.and_then(|p| p.file_roles.get(&rel_str).copied())
}

pub fn profile_entrypoints(profile: Option<&RepoProfile>) -> Vec<String> {
    profile.map(|p| p.entrypoints.clone()).unwrap_or_default()
}

pub fn summarize_role_counts(profile: Option<&RepoProfile>) -> HashMap<String, usize> {
    profile
        .map(|p| {
            p.role_counts
                .iter()
                .map(|(role, count)| (role_label(*role).to_string(), *count))
                .collect()
        })
        .unwrap_or_default()
}

pub fn compact_repo_map(profile: Option<&RepoProfile>) -> serde_json::Value {
    let Some(profile) = profile else {
        return serde_json::json!({
            "archetype": archetype_label(RepoArchetype::Unknown),
            "role_counts": HashMap::<String, usize>::new(),
            "entrypoints": Vec::<String>::new(),
            "top_roles": Vec::<serde_json::Value>::new(),
        });
    };

    let mut top_roles: Vec<(PathRole, usize)> = profile
        .role_counts
        .iter()
        .map(|(role, count)| (*role, *count))
        .collect();
    top_roles.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| role_label(a.0).cmp(role_label(b.0)))
    });
    top_roles.truncate(4);

    serde_json::json!({
        "archetype": archetype_label(profile.archetype),
        "role_counts": summarize_role_counts(Some(profile)),
        "entrypoints": profile.entrypoints,
        "top_roles": top_roles
            .into_iter()
            .map(|(role, count)| serde_json::json!({
                "role": role_label(role),
                "count": count,
            }))
            .collect::<Vec<_>>(),
    })
}

pub fn role_by_path(
    project_path: &Path,
    file_path: &Path,
    profile: Option<&RepoProfile>,
) -> PathRole {
    path_role_for_file(project_path, file_path, profile)
}

pub fn role_boost_for_path(
    project_path: &Path,
    file_path: &Path,
    profile: Option<&RepoProfile>,
    query: &str,
) -> i32 {
    let role = role_by_path(project_path, file_path, profile);
    let archetype = profile.map(|p| p.archetype).unwrap_or_default();
    role_boost(role, archetype, query)
}

pub fn symbol_kind_role_boost(kind: &SymbolKind) -> i32 {
    match kind {
        SymbolKind::Function | SymbolKind::Method => 4,
        SymbolKind::Struct | SymbolKind::Class | SymbolKind::Interface => 3,
        SymbolKind::Const => 2,
        SymbolKind::Mod => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::language::{make_symbol_id, Language, Symbol, SymbolKind};
    use std::sync::Arc;

    fn make_symbol(file: &str, name: &str) -> Symbol {
        let path = std::path::PathBuf::from(file);
        Symbol {
            id: make_symbol_id(&path, name, &SymbolKind::Function),
            name: name.to_string(),
            qualified: format!("crate::{name}"),
            kind: SymbolKind::Function,
            language: Language::Rust,
            file: Arc::new(path),
            byte_start: 0,
            byte_end: 0,
            line_start: 1,
            line_end: 1,
            signature: None,
            doc: None,
        }
    }

    #[test]
    fn test_build_repo_profile_classifies_cli_repo() {
        let mut index = SymbolIndex::new();
        index.insert(make_symbol("main.rs", "main"));
        index.insert(make_symbol("lib.rs", "helper"));

        let profile = build_repo_profile(Path::new("/tmp/project"), &index);
        assert_eq!(profile.archetype, RepoArchetype::Cli);
        assert_eq!(
            profile.file_roles.get("main.rs").copied(),
            Some(PathRole::Entrypoint)
        );
        assert_eq!(
            profile.file_roles.get("lib.rs").copied(),
            Some(PathRole::Library)
        );
    }

    #[test]
    fn test_build_repo_profile_classifies_library_repo() {
        let mut index = SymbolIndex::new();
        index.insert(make_symbol("lib.rs", "helper"));

        let profile = build_repo_profile(Path::new("/tmp/project"), &index);
        assert_eq!(profile.archetype, RepoArchetype::Library);
        assert_eq!(
            profile.file_roles.get("lib.rs").copied(),
            Some(PathRole::Library)
        );
    }

    #[test]
    fn test_compact_repo_map_summarizes_top_roles() {
        let mut index = SymbolIndex::new();
        index.insert(make_symbol("main.rs", "main"));
        index.insert(make_symbol("config/settings.rs", "settings"));
        index.insert(make_symbol("handlers/http.rs", "serve"));

        let profile = build_repo_profile(Path::new("/tmp/project"), &index);
        let summary = compact_repo_map(Some(&profile));

        assert_eq!(summary["archetype"], serde_json::json!("cli"));
        assert!(summary["role_counts"]["entrypoint"].as_u64().unwrap_or(0) >= 1);
        assert!(!summary["top_roles"].as_array().unwrap().is_empty());
    }
}
