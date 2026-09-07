use super::*;
use tempfile::TempDir;

fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "-q"]).unwrap();
    write(
        &dir,
        "lib.rs",
        "pub fn compute() -> i32 {\n    1\n}\npub fn caller() -> i32 {\n    compute()\n}\n",
    );
    write(
        &dir,
        "tests/check.rs",
        "fn test_compute() {\n    compute();\n}\n",
    );
    commit(&dir);
    dir
}

fn write(dir: &TempDir, path: &str, content: &str) {
    let path = dir.path().join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn commit(dir: &TempDir) {
    git(dir.path(), &["add", "--all"]).unwrap();
    git(
        dir.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    )
    .unwrap();
}

fn run(dir: &TempDir, base: &str, worktree: bool) -> Value {
    analyze(AnalyzeChangesParams {
        project: dir.path().display().to_string(),
        base_ref: base.to_string(),
        include_working_tree: Some(worktree),
        depth: Some(2),
        limit: Some(12),
    })
    .unwrap()
}

#[test]
fn hunk_ranges_include_defaults_and_zero_length() {
    let hunks = parse_hunks(b"@@ -2 +2,3 @@\n-old\n+new\n@@ -9,0 +12 @@\n+x\n").unwrap();
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].old.count, 1);
    assert_eq!(hunks[0].new.count, 3);
    assert_eq!(hunks[1].old.count, 0);
    assert_eq!(hunks[1].new.start, 12);
}

#[test]
fn clean_tree_and_invalid_revision() {
    let dir = fixture();
    let result = run(&dir, "HEAD", false);
    assert_eq!(result["changed_files"], json!([]));
    assert_eq!(result["changed_symbols"], json!([]));
    assert_eq!(result["base_impact"]["impact_symbols"], json!([]));
    for revision in ["", "missing-ref", "--help"] {
        assert!(resolve_commit(dir.path(), revision).is_err());
    }
    let nongit = TempDir::new().unwrap();
    assert!(analyze(AnalyzeChangesParams {
        project: nongit.path().display().to_string(),
        base_ref: "HEAD".into(),
        include_working_tree: None,
        depth: None,
        limit: None,
    })
    .is_err());
}

#[test]
fn committed_change_ignores_worktree_and_reuses_impact_evidence() {
    let dir = fixture();
    write(
        &dir,
        "lib.rs",
        "pub fn compute() -> i32 {\n    2\n}\npub fn caller() -> i32 {\n    compute()\n}\n",
    );
    commit(&dir);
    write(&dir, "lib.rs", "pub fn unrelated() {}\n");
    let result = run(&dir, "HEAD~1", false);
    let changed = result["changed_symbols"].as_array().unwrap();
    assert_eq!(changed.len(), 2);
    assert!(changed
        .iter()
        .all(|s| s["name"] == "compute" && s["change"] == "modified"));
    let impact = result["target_impact"]["impact_symbols"]
        .as_array()
        .unwrap();
    let caller = impact.iter().find(|s| s["name"] == "caller").unwrap();
    assert_eq!(caller["certainty"], "heuristic");
    assert!(!caller["support_edges"].as_array().unwrap().is_empty());
    assert!(result["target_impact"]["test_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "test_compute"));
    assert!(!result.to_string().contains("unrelated"));
    assert!(result["changed_files"][0]["hunks"][0]["old"]["start"] == 2);
}

#[test]
fn deleted_symbol_uses_base_graph_even_when_current_calls_are_unresolved() {
    let dir = fixture();
    write(
        &dir,
        "lib.rs",
        "pub fn caller() -> i32 {\n    compute()\n}\n",
    );
    let result = run(&dir, "HEAD", true);
    assert!(result["changed_symbols"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "compute" && s["change"] == "deleted"));
    assert!(result["base_impact"]["impact_symbols"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "caller"));
    assert!(result["base_impact"]["test_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "test_compute"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "pub fn caller() -> i32 {\n    compute()\n}\n"
    );
}

#[test]
fn staged_unstaged_untracked_and_ignored_files() {
    let dir = fixture();
    write(&dir, ".gitignore", "ignored.rs\n");
    write(&dir, "ignored.rs", "fn ignored() {}\n");
    write(&dir, "new file.rs", "fn fresh() {}\n");
    write(&dir, "lib.rs", "fn staged() {}\n");
    git(dir.path(), &["add", "lib.rs"]).unwrap();
    write(&dir, "lib.rs", "fn unstaged() {}\n");
    let before = git(dir.path(), &["status", "--porcelain=v1", "-z"]).unwrap();
    let result = run(&dir, "HEAD", true);
    let changed = result["changed_symbols"].as_array().unwrap();
    assert!(changed.iter().any(|s| s["name"] == "unstaged"));
    assert!(changed.iter().any(|s| s["name"] == "fresh"));
    assert!(!changed
        .iter()
        .any(|s| s["name"] == "staged" || s["name"] == "ignored"));
    assert_eq!(
        before,
        git(dir.path(), &["status", "--porcelain=v1", "-z"]).unwrap()
    );
    assert_eq!(run(&dir, "HEAD", false)["changed_files"], json!([]));
}

#[test]
fn deleted_files_renames_and_unmapped_documentation() {
    let dir = fixture();
    std::fs::rename(dir.path().join("lib.rs"), dir.path().join("renamed.rs")).unwrap();
    write(&dir, "README.md", "# Hello\n");
    let result = run(&dir, "HEAD", true);
    let files = result["changed_files"].as_array().unwrap();
    assert!(files
        .iter()
        .any(|f| f["file"] == "lib.rs" && f["change"] == "deleted"));
    assert!(files
        .iter()
        .any(|f| f["file"] == "renamed.rs" && f["change"] == "added"));
    assert!(files.iter().any(|f| f["file"] == "README.md"
        && f["mapped_symbol_count"] == 0
        && f["unmapped_reason"].is_string()));
}

#[test]
fn subtree_analysis_does_not_include_siblings() {
    let dir = fixture();
    write(&dir, "package/lib.rs", "fn inner() {\n    let a = 1;\n}\n");
    commit(&dir);
    write(&dir, "package/lib.rs", "fn inner() {\n    let a = 2;\n}\n");
    write(&dir, "lib.rs", "fn outside() {}\n");
    let result = analyze(AnalyzeChangesParams {
        project: dir.path().join("package").display().to_string(),
        base_ref: "HEAD".into(),
        include_working_tree: Some(true),
        depth: None,
        limit: None,
    })
    .unwrap();
    assert_eq!(result["changed_files"].as_array().unwrap().len(), 1);
    assert!(result["changed_symbols"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["name"] == "inner"));
}

#[test]
fn zero_length_boundary_does_not_mark_neighboring_symbol() {
    let dir = fixture();
    write(&dir, "lib.rs", "pub fn compute() -> i32 {\n    1\n}\nfn inserted() {}\npub fn caller() -> i32 {\n    compute()\n}\n");
    let result = run(&dir, "HEAD", true);
    let changed = result["changed_symbols"].as_array().unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0]["name"], "inserted");
    assert_eq!(changed[0]["change"], "added");
}

#[test]
fn exclusions_and_parse_errors_are_reported_without_hiding_changes() {
    let dir = fixture();
    write(&dir, "generated.rs", "fn generated() {}\n");
    write(&dir, "node_modules/vendor.rs", "fn vendor() {}\n");
    commit(&dir);
    write(&dir, "generated.rs", "fn generated_new() {}\n");
    write(&dir, "node_modules/vendor.rs", "fn vendor_new() {}\n");
    write(&dir, "broken.rs", "fn broken() { let = ; }\n");
    let mut meta = crate::index::format::IndexMeta::new(dir.path());
    meta.effective_excludes = vec!["generated.rs".into()];
    let index_dir = crate::index::format::index_dir(dir.path()).unwrap();
    std::fs::create_dir_all(&index_dir).unwrap();
    crate::index::format::save_meta(&meta, &index_dir.join("meta.json")).unwrap();
    let result = run(&dir, "HEAD", true);
    let changed = result["changed_symbols"].as_array().unwrap();
    assert!(!changed
        .iter()
        .any(|s| s["name"] == "generated_new" || s["name"] == "vendor_new"));
    assert!(result["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["file"] == "broken.rs"));
    assert!(result["omissions"]["target"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["file"] == "broken.rs"));
    assert!(result["effective_excludes"]
        .as_array()
        .unwrap()
        .contains(&json!("generated.rs")));
}

#[test]
fn bounded_results_report_omissions_and_label_uncertain_references() {
    let dir = fixture();
    write(&dir, "lib.rs", "pub fn compute() -> i32 {\n    1\n}\nfn indirect() { let callback = compute; }\nfn caller_one() { compute(); }\nfn caller_two() { compute(); }\n");
    commit(&dir);
    write(&dir, "lib.rs", "pub fn compute() -> i32 {\n    2\n}\nfn indirect() { let callback = compute; }\nfn caller_one() { compute(); }\nfn caller_two() { compute(); }\n");
    let result = run(&dir, "HEAD", true);
    let indirect = result["target_impact"]["impact_symbols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "indirect")
        .unwrap();
    assert!(indirect["support_edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["certainty"] == "uncertain_reference"));
    let limited = analyze(AnalyzeChangesParams {
        project: dir.path().display().to_string(),
        base_ref: "HEAD".into(),
        include_working_tree: Some(true),
        depth: Some(1),
        limit: Some(1),
    })
    .unwrap();
    assert_eq!(
        limited["target_impact"]["impact_symbols"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        limited["target_impact"]["omitted_impact_symbols"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn oversized_file_is_not_reported_as_a_deletion() {
    let dir = fixture();
    write(&dir, "lib.rs", &"x".repeat(MAX_FILE_BYTES + 1));
    let result = run(&dir, "HEAD", true);
    assert!(result["omissions"]["target"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["file"] == "lib.rs"));
    assert_eq!(result["changed_symbols"], json!([]));
}

#[cfg(unix)]
#[test]
fn symlinks_are_not_followed() {
    use std::os::unix::fs::symlink;
    let dir = fixture();
    let external = TempDir::new().unwrap();
    write(&external, "secret.rs", "fn secret() {}\n");
    symlink(
        external.path().join("secret.rs"),
        dir.path().join("link.rs"),
    )
    .unwrap();
    commit(&dir);
    let result = run(&dir, "HEAD~1", false);
    assert!(result["omissions"]["target"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["file"] == "link.rs"));
    assert!(!result.to_string().contains("secret"));
    let result = run(&dir, "HEAD", true);
    assert!(result["omissions"]["target"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["file"] == "link.rs"));
}
