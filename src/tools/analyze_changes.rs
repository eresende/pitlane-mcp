//! Revision-local Git diff analysis. Never checks out files or mutates the cached index.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::index::SymbolIndex;
use crate::indexer::{self, language::Symbol, registry::build_default_registry, Indexer};
use crate::path_policy::{open_regular_file, regular_file_metadata, resolve_project_path};
use crate::tools::orchestrator::{impact_from_seeds, AnalyzeImpactParams, ImpactSeed};

#[cfg(test)]
mod tests;

const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;

pub struct AnalyzeChangesParams {
    pub project: String,
    pub base_ref: String,
    pub include_working_tree: Option<bool>,
    pub depth: Option<usize>,
    pub limit: Option<usize>,
}

pub async fn analyze_changes(params: AnalyzeChangesParams) -> Result<Value> {
    tokio::task::spawn_blocking(move || analyze(params)).await?
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .context("analyze_changes requires Git")?;
    anyhow::ensure!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn resolve_commit(root: &Path, revision: &str) -> Result<String> {
    anyhow::ensure!(!revision.trim().is_empty(), "base_ref must not be empty");
    let bytes = git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )?;
    Ok(String::from_utf8(bytes)?.trim().to_string())
}

#[derive(Default)]
struct Snapshot {
    files: BTreeMap<PathBuf, Vec<u8>>,
    omissions: Vec<Value>,
    unavailable: BTreeSet<PathBuf>,
    bytes: usize,
}

impl Snapshot {
    fn insert(&mut self, path: PathBuf, bytes: Vec<u8>) -> Result<()> {
        self.bytes += bytes.len();
        anyhow::ensure!(
            self.bytes <= MAX_SNAPSHOT_BYTES,
            "Revision snapshot exceeds the 128 MiB analysis limit; use a smaller project subtree"
        );
        self.files.insert(path, bytes);
        Ok(())
    }

    fn omit(&mut self, path: &Path, reason: &str) {
        self.unavailable.insert(path.to_path_buf());
        self.omissions.push(json!({"file": path, "reason": reason}));
    }
}

fn safe_relative(path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    anyhow::ensure!(
        !path.as_os_str().is_empty()
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "Unsupported Git path: {}",
        path.display()
    );
    Ok(path)
}

fn revision_snapshot(root: &Path, repo: &Path, revision: &str) -> Result<Snapshot> {
    let prefix = root.strip_prefix(repo)?;
    let listing = git(
        repo,
        &["ls-tree", "-r", "-l", "-z", "--full-tree", revision],
    )?;
    let mut snapshot = Snapshot::default();
    for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = std::str::from_utf8(entry).context("Non-UTF-8 Git paths are not supported")?;
        let (metadata, path) = entry.split_once('\t').context("Invalid Git tree entry")?;
        let path = safe_relative(path)?;
        let Ok(relative) = path.strip_prefix(prefix) else {
            continue;
        };
        let fields: Vec<_> = metadata.split_whitespace().collect();
        anyhow::ensure!(fields.len() == 4, "Invalid Git tree metadata");
        if !matches!(fields[0], "100644" | "100755") || fields[1] != "blob" {
            snapshot.omit(relative, "symlink or submodule");
            continue;
        }
        if fields[3].parse::<usize>()? > MAX_FILE_BYTES {
            snapshot.omit(relative, "file exceeds 1 MiB");
            continue;
        }
        snapshot.insert(
            relative.to_path_buf(),
            git(repo, &["cat-file", "blob", fields[2]])?,
        )?;
    }
    Ok(snapshot)
}

fn working_snapshot(root: &Path) -> Result<Snapshot> {
    let listing = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ],
    )?;
    let mut snapshot = Snapshot::default();
    let mut seen = BTreeSet::new();
    for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let relative = safe_relative(
            std::str::from_utf8(entry).context("Non-UTF-8 Git paths are not supported")?,
        )?;
        if !seen.insert(relative.clone()) {
            continue;
        }
        let path = root.join(&relative);
        // Refuse intermediate symlinks too, including links pointing inside the project.
        let mut ancestor = root.to_path_buf();
        let mut symlink = false;
        for part in relative.components() {
            ancestor.push(part);
            if std::fs::symlink_metadata(&ancestor).is_ok_and(|m| m.file_type().is_symlink()) {
                symlink = true;
                break;
            }
        }
        if symlink {
            snapshot.omit(&relative, "symlink");
            continue;
        }
        let Some(metadata) = regular_file_metadata(&path)? else {
            continue;
        };
        if metadata.len() > MAX_FILE_BYTES as u64 {
            snapshot.omit(&relative, "file exceeds 1 MiB");
            continue;
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        open_regular_file(&path)?
            .take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_BYTES {
            snapshot.omit(&relative, "file exceeds 1 MiB");
            continue;
        }
        snapshot.insert(relative, bytes)?;
    }
    Ok(snapshot)
}

fn snapshot_index(
    root: &Path,
    snapshot: &mut Snapshot,
    excludes: &globset::GlobSet,
) -> Result<SymbolIndex> {
    let registry = build_default_registry();
    let extra = indexer::extra_excluded_dir_names();
    let mut index = SymbolIndex::new();
    for (relative, source) in &snapshot.files {
        let absolute = root.join(relative);
        let ext = relative.extension().and_then(|e| e.to_str()).unwrap_or("");
        if indexer::path_is_excluded(&absolute, root, excludes, &extra)
            || excludes.is_match(&absolute)
            || indexer::is_declaration_file(relative)
        {
            continue;
        }
        let Some(parser) = registry.iter().find(|p| p.extensions().contains(&ext)) else {
            continue;
        };
        let Some(language) = indexer::tree_sitter_language_for_extension(ext) else {
            continue;
        };
        let mut ts = tree_sitter::Parser::new();
        ts.set_language(&language)?;
        let Some(tree) = ts.parse(source, None) else {
            snapshot
                .omissions
                .push(json!({"file": relative, "reason": "parse failed"}));
            continue;
        };
        if tree.root_node().has_error() {
            snapshot.omissions.push(
                json!({"file": relative, "reason": "parse errors; symbols may be incomplete"}),
            );
        }
        for mut symbol in parser.extract_symbols(source, &tree, relative) {
            symbol.file = Arc::new(absolute.clone());
            index.insert(symbol);
        }
    }
    index.graph = crate::graph::build_navigation_graph_with_source(&index, |symbol| {
        let source = snapshot
            .files
            .get(symbol.file.strip_prefix(root)?)
            .context("Missing revision source")?;
        Ok(String::from_utf8_lossy(source).into_owned())
    });
    Ok(index)
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
struct LineRange {
    start: u32,
    count: u32,
}

impl LineRange {
    fn overlaps(self, symbol: &Symbol) -> bool {
        // A zero-length range is an insertion/deletion boundary, not an edited line.
        self.count > 0
            && self.start <= symbol.line_end
            && self.start.saturating_add(self.count - 1) >= symbol.line_start
    }
}

#[derive(Debug, serde::Serialize)]
struct Hunk {
    old: LineRange,
    new: LineRange,
}

fn parse_range(range: &str) -> Result<LineRange> {
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    Ok(LineRange {
        start: start.parse()?,
        count: count.parse()?,
    })
}

fn parse_hunks(diff: &[u8]) -> Result<Vec<Hunk>> {
    let mut hunks = Vec::new();
    for line in String::from_utf8_lossy(diff)
        .lines()
        .filter(|line| line.starts_with("@@ "))
    {
        let mut fields = line.split_whitespace().skip(1);
        let old = fields
            .next()
            .and_then(|s| s.strip_prefix('-'))
            .context("Invalid old hunk range")?;
        let new = fields
            .next()
            .and_then(|s| s.strip_prefix('+'))
            .context("Invalid new hunk range")?;
        hunks.push(Hunk {
            old: parse_range(old)?,
            new: parse_range(new)?,
        });
    }
    Ok(hunks)
}

fn diff_hunks(root: &Path, old: &[u8], new: &[u8]) -> Result<Vec<Hunk>> {
    use std::io::Write;
    let mut before = tempfile::NamedTempFile::new()?;
    let mut after = tempfile::NamedTempFile::new()?;
    before.write_all(old)?;
    after.write_all(new)?;
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--text",
            "--no-color",
            "--unified=0",
            "--inter-hunk-context=0",
            "--diff-algorithm=myers",
            "--",
        ])
        .arg(before.path())
        .arg(after.path())
        .output()?;
    anyhow::ensure!(
        matches!(output.status.code(), Some(0 | 1)),
        "Git diff failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_hunks(&output.stdout)
}

fn seed(symbol: &Symbol) -> ImpactSeed {
    ImpactSeed {
        id: symbol.id.clone(),
        name: symbol.name.clone(),
        kind: symbol.kind.to_string(),
        file: symbol.file.to_string_lossy().into_owned(),
    }
}

fn symbol_value(symbol: &Symbol, revision: &str, change: &str, hunks: &[usize]) -> Value {
    json!({"id": symbol.id, "name": symbol.name, "kind": symbol.kind.to_string(),
        "file": symbol.file, "line_start": symbol.line_start, "line_end": symbol.line_end,
        "revision": revision, "change": change, "hunk_indices": hunks,
        "evidence": "symbol line range overlaps changed lines"})
}

fn revision_impact(
    params: &AnalyzeChangesParams,
    root: &Path,
    index: &SymbolIndex,
    seeds: Vec<ImpactSeed>,
    revision: &str,
) -> Result<Value> {
    if seeds.is_empty() {
        return Ok(json!({
            "revision": revision, "certainty": "heuristic",
            "seed_symbols": [], "impact_symbols": [], "impact_files": [], "test_candidates": [],
            "depth_limit": params.depth.unwrap_or(2).clamp(1, 3),
            "limit": params.limit.unwrap_or(8).clamp(1, 12),
            "total_impact_symbols": 0, "total_impact_files": 0,
            "omitted_impact_symbols": 0, "omitted_impact_files": 0,
        }));
    }
    let profile = crate::index::repo_profile::build_repo_profile(root, index);
    let mut result = impact_from_seeds(
        &AnalyzeImpactParams {
            project: params.project.clone(),
            query: None,
            symbol_id: None,
            file_path: None,
            scope: None,
            depth: params.depth,
            limit: params.limit,
        },
        index,
        root,
        &seeds,
        Some(&profile),
        false,
    )?;
    // Historical IDs can equal current IDs; always qualify graph evidence by revision.
    result.as_object_mut().unwrap().remove("steering");
    result["revision"] = json!(revision);
    result["certainty"] = json!("heuristic");
    for key in ["impact_symbols", "impact_files"] {
        for item in result[key].as_array_mut().unwrap() {
            item["revision"] = json!(revision);
            item["certainty"] = json!("heuristic");
            if let Some(edges) = item["support_edges"].as_array_mut() {
                for edge in edges {
                    edge["certainty"] = json!(if edge["relation"] == "calls" {
                        "heuristic_call"
                    } else {
                        "uncertain_reference"
                    });
                }
            }
        }
    }
    // Include directly changed tests as well as graph-reachable test candidates.
    let mut tests = BTreeMap::new();
    for symbol in result["impact_symbols"].as_array().unwrap() {
        if let Some(sym) = symbol["id"].as_str().and_then(|id| index.symbols.get(id)) {
            if is_test(sym, root, index) {
                let mut candidate = symbol.clone();
                candidate["test_evidence"] =
                    json!("test-like path or symbol name; impact supported by graph edges");
                tests.insert(sym.id.clone(), candidate);
            }
        }
    }
    for seed in seeds {
        let sym = &index.symbols[&seed.id];
        if is_test(sym, root, index) {
            let candidate = tests.entry(seed.id).or_insert_with(|| {
                json!({
                    "id": sym.id, "name": sym.name, "file": sym.file,
                    "revision": revision, "certainty": "heuristic",
                    "test_evidence": "test-like path or symbol name",
                })
            });
            candidate["change_evidence"] = json!("symbol line range overlaps changed lines");
        }
    }
    result["test_candidates"] = json!(tests.into_values().collect::<Vec<_>>());
    Ok(result)
}

fn is_test(symbol: &Symbol, root: &Path, index: &SymbolIndex) -> bool {
    use crate::index::repo_profile::{classify_path_role, PathRole};
    classify_path_role(
        symbol.file.strip_prefix(root).unwrap_or(&symbol.file),
        index,
    ) == PathRole::Test
        || symbol.name.starts_with("test_")
        || symbol.name.ends_with("_test")
        || symbol.qualified.contains("tests::")
        || symbol.qualified.contains("Test")
}

fn analyze(params: AnalyzeChangesParams) -> Result<Value> {
    let root = resolve_project_path(&params.project)?;
    let repo_output = String::from_utf8(git(&root, &["rev-parse", "--show-toplevel"])?)?;
    let repo = PathBuf::from(repo_output.strip_suffix('\n').unwrap_or(&repo_output));
    let repo = repo.canonicalize()?;
    let base = resolve_commit(&root, &params.base_ref)?;
    let head = resolve_commit(&root, "HEAD")?;
    let include_working_tree = params.include_working_tree.unwrap_or(false);
    let target_revision = if include_working_tree {
        "working_tree"
    } else {
        &head
    };
    let mut before = revision_snapshot(&root, &repo, &base)?;
    let mut after = if include_working_tree {
        working_snapshot(&root)?
    } else {
        revision_snapshot(&root, &repo, &head)?
    };
    let mut patterns = indexer::default_exclude_patterns();
    patterns.extend(indexer::load_gitignore_patterns(&root));
    if let Ok(meta) = crate::index::format::load_project_meta(&root) {
        patterns.extend(meta.effective_excludes);
    }
    let excludes = Indexer::build_exclude_set(&patterns)?;
    let old_index = snapshot_index(&root, &mut before, &excludes)?;
    let new_index = snapshot_index(&root, &mut after, &excludes)?;
    let paths: BTreeSet<_> = before.files.keys().chain(after.files.keys()).collect();
    let mut files = Vec::new();
    let mut changed_symbols = Vec::new();
    let mut old_seeds = BTreeMap::new();
    let mut new_seeds = BTreeMap::new();
    for path in paths {
        let old = before.files.get(path);
        let new = after.files.get(path);
        if old == new {
            continue;
        }
        // Do not turn an unreadable/oversized target into a claimed deletion.
        if before.unavailable.contains(path) || after.unavailable.contains(path) {
            continue;
        }
        let hunks = diff_hunks(
            &root,
            old.map(Vec::as_slice).unwrap_or_default(),
            new.map(Vec::as_slice).unwrap_or_default(),
        )?;
        let change = if old.is_none() {
            "added"
        } else if new.is_none() {
            "deleted"
        } else {
            "modified"
        };
        let absolute = root.join(path);
        let start = changed_symbols.len();
        let mut mapped_hunks = BTreeSet::new();
        for (index, other, revision, is_old, seeds) in [
            (&old_index, &new_index, base.as_str(), true, &mut old_seeds),
            (
                &new_index,
                &old_index,
                target_revision,
                false,
                &mut new_seeds,
            ),
        ] {
            let mut symbols: Vec<_> = index
                .by_file
                .get(&absolute)
                .into_iter()
                .flatten()
                .filter_map(|id| index.symbols.get(id))
                .collect();
            symbols.sort_by(|a, b| a.line_start.cmp(&b.line_start).then(a.id.cmp(&b.id)));
            for symbol in symbols {
                let overlaps: Vec<_> = hunks
                    .iter()
                    .enumerate()
                    .filter_map(|(i, h)| {
                        if (if is_old { h.old } else { h.new }).overlaps(symbol) {
                            Some(i)
                        } else {
                            None
                        }
                    })
                    .collect();
                if overlaps.is_empty() {
                    continue;
                }
                let change = if other.symbols.contains_key(&symbol.id) {
                    "modified"
                } else if is_old {
                    "deleted"
                } else {
                    "added"
                };
                mapped_hunks.extend(overlaps.iter().copied());
                changed_symbols.push(symbol_value(symbol, revision, change, &overlaps));
                seeds.insert(symbol.id.clone(), seed(symbol));
            }
        }
        let unmapped_hunks: Vec<_> = (0..hunks.len())
            .filter(|i| !mapped_hunks.contains(i))
            .collect();
        files.push(json!({"file": path, "change": change, "hunks": hunks,
            "unmapped_hunk_indices": unmapped_hunks,
            "mapped_symbol_count": changed_symbols.len() - start,
            "unmapped_reason": if start == changed_symbols.len() { Some("No indexed symbol overlaps these lines (unsupported, excluded, or file-level change)") } else { None }}));
    }
    let base_impact = revision_impact(
        &params,
        &root,
        &old_index,
        old_seeds.into_values().collect(),
        &base,
    )?;
    let target_impact = revision_impact(
        &params,
        &root,
        &new_index,
        new_seeds.into_values().collect(),
        target_revision,
    )?;
    Ok(json!({
        "base_ref": params.base_ref, "base_revision": base, "head_revision": head,
        "effective_excludes": patterns,
        "target_revision": target_revision, "include_working_tree": include_working_tree,
        "changed_files": files, "changed_symbols": changed_symbols,
        "base_impact": base_impact, "target_impact": target_impact,
        "omissions": {"base": before.omissions, "target": after.omissions},
        "limitations": [
            "Graph edges and test classification are heuristic, not proof of runtime impact or test coverage.",
            "Impact lists are ranked and bounded by limit (default 8, maximum 12) and depth (default 2, maximum 3); test candidates come from those results and changed tests.",
            "Renames are represented as deletion/addition. File-level edits outside symbol ranges remain unmapped.",
            "Exclusions use the current project policy for both revisions. Files over 1 MiB, symlinks and submodules are omitted.",
            "Working-tree snapshots are not atomic; do not edit files during analysis. Staged content is included only as reflected in files on disk."
        ]
    }))
}
