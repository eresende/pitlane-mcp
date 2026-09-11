//! Integration tests: OKF (Open Knowledge Format) knowledge indexing
//! (Phase 2 of issue #118).
//!
//! Builds a representative OKF v0.2 bundle modeled on the spec's Appendix A
//! worked example (metrics, attested computations in different trust/staleness
//! states, cross-links, index.md/log.md reserved files) and exercises the
//! `search_knowledge` MCP tool end to end over it.

use serde_json::Value;

// ── Representative OKF v0.2 bundle ─────────────────────────────────────────

fn okf_bundle() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "metrics/income-statement.md",
            "---\ntype: Metric\ntitle: Income statement (fiscal year)\ndescription: Headline income-statement figures for a fiscal year.\ntags: [finance, income-statement]\nstatus: stable\ngenerated: { by: reference_agent/gemini-2.5-pro, at: 2026-06-20T22:53:05Z }\nverified: { by: human:ahormati, at: 2026-06-25T09:00:00Z }\nstale_after: 2026-12-31T00:00:00Z\nsources:\n  - id: fpa-handbook\n    resource: https://wiki.acme/finance/fpa-handbook\n    title: FP&A reporting handbook\n    author: team:finance-fpa\n---\n# Definition\n\nThe income statement reports [revenue](../computations/revenue.md) and\n[gross profit](../computations/profit.md) for a fiscal year, per the FP&A\nreporting handbook.[^fpa-handbook]\n\n[^fpa-handbook]: FP&A reporting handbook\n",
        ),
        (
            "computations/revenue.md",
            "---\ntype: Attested Computation\ntitle: Revenue for fiscal year\ndescription: Recognized revenue for a fiscal year, per Finance's definition.\ntags: [finance, revenue]\nstatus: stable\nruntime: bigquery\nexecutor:\n  resource: references/skills/run-on-bq.md\n  receipt: [job_id, executed_sql, result]\ngenerated: { by: reference_agent/gemini-2.5-pro, at: 2026-06-28T14:00:00Z }\nverified: { by: human:ahormati, at: 2026-06-25T09:00:00Z }\nstale_after: 2026-12-31T00:00:00Z\n---\n# Computation\n\n    SELECT SUM(amount) AS revenue\n    FROM finance.recognized_revenue\n    WHERE fiscal_year = @year\n",
        ),
        (
            "computations/profit.md",
            "---\ntype: Attested Computation\ntitle: Gross profit for fiscal year\ndescription: Gross profit by segment for a fiscal year.\ntags: [finance, profit]\nstatus: stable\nruntime: dbt\ngenerated: { by: reference_agent/gemini-2.5-pro, at: 2026-06-14T14:00:00Z }\nverified: { by: process:finance-nightly, at: 2026-06-12T08:00:00Z }\nstale_after: 2026-06-15T00:00:00Z\n---\n# Computation\n\n    SELECT gross_profit\n    FROM {{ ref('fct_income_statement') }}\n    WHERE fiscal_year = {{ var('year') }}\n",
        ),
        (
            "computations/legacy-revenue.md",
            "---\ntype: Attested Computation\ntitle: Legacy revenue estimate\ndescription: Superseded revenue estimate kept for history.\ntags: [finance, revenue]\nstatus: deprecated\ngenerated: { by: reference_agent/old-agent, at: 2025-01-01T00:00:00Z }\n---\n# Computation\n\n    SELECT SUM(amount) AS revenue\n    FROM finance.legacy_revenue\n",
        ),
        (
            "draft-runbook.md",
            "---\ntype: Playbook\ntitle: Revenue triage draft\ndescription: Draft steps for triaging revenue discrepancies.\nstatus: draft\ntags: [oncall]\n---\n# Steps\n\n1. Compare the attested revenue with the dashboard.\n",
        ),
        (
            "plain-notes.md",
            "Free-form notes with no front matter at all. Mentions revenue estimation.\n",
        ),
        // Reserved files: structural, indexed like any other Markdown.
        (
            "index.md",
            "---\nokf_version: \"0.2\"\ntitle: Finance bundle\n---\n# Concepts\n\n* [Income statement](metrics/income-statement.md) - headline figures\n",
        ),
        (
            "computations/index.md",
            "# Computations\n\n* [Revenue](revenue.md) - recognized revenue\n* [Gross profit](profit.md) - gross profit by segment\n",
        ),
    ]
}

fn setup_project() -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().unwrap();
    for (path, content) in okf_bundle() {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    let project = dir.path().canonicalize().unwrap();
    (dir, project.to_string_lossy().to_string())
}

fn results(response: &Value) -> &Vec<Value> {
    response["results"].as_array().unwrap()
}

fn section_by_file<'a>(results: &'a [Value], path: &str) -> &'a Value {
    results
        .iter()
        .find(|r| r["file_path"].as_str() == Some(path))
        .unwrap_or_else(|| panic!("no result for {path}: {results:?}"))
}

// ── Metadata extraction over the bundle ────────────────────────────────────

#[tokio::test]
async fn okf_metadata_is_indexed_across_the_bundle() {
    let (_dir, project) = setup_project();
    let response = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project: project.clone(),
            query: "fpa handbook fiscal year".into(),
            tag: None,
            path_filter: None,
            okf_type: None,
            status: None,
            min_trust: None,
            limit: Some(20),
            embed_config: None,
        },
    )
    .await
    .unwrap();
    let income = section_by_file(results(&response), "metrics/income-statement.md");
    assert_eq!(income["okf_type"], "Metric");
    assert_eq!(
        income["description"],
        "Headline income-statement figures for a fiscal year."
    );
    assert_eq!(income["trust_tier"], "human-reviewed");
    assert_eq!(income["status"], "stable");
    assert_eq!(income["stale"], false);

    let related = income["related_docs"].as_array().unwrap();
    let targets: Vec<&str> = related
        .iter()
        .filter_map(|l| l["target"].as_str())
        .collect();
    assert_eq!(
        targets,
        vec![
            "computations/revenue.md".to_string(),
            "computations/profit.md".to_string()
        ]
    );
}

#[tokio::test]
async fn stale_and_deprecated_metadata_is_reported() {
    let (_dir, project) = setup_project();
    let response = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project: project.clone(),
            query: "legacy revenue estimate".into(),
            tag: None,
            path_filter: None,
            okf_type: None,
            status: None,
            min_trust: None,
            limit: Some(10),
            embed_config: None,
        },
    )
    .await
    .unwrap();
    let legacy = section_by_file(results(&response), "computations/legacy-revenue.md");
    assert_eq!(legacy["status"], "deprecated");
    assert_eq!(legacy["trust_tier"], "unverified");
    assert_eq!(legacy["metadata_adjustment"], -0.10);

    let profit = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project: project.clone(),
            query: "gross profit segment dbt".into(),
            tag: None,
            path_filter: None,
            okf_type: None,
            status: None,
            min_trust: None,
            limit: Some(10),
            embed_config: None,
        },
    )
    .await
    .unwrap();
    let profit_section = section_by_file(results(&profit), "computations/profit.md");
    assert_eq!(profit_section["stale"], true);
    assert_eq!(profit_section["trust_tier"], "machine-confirmed");
    // Stale machine-confirmed: +0.02 − 0.10.
    assert_eq!(profit_section["metadata_adjustment"], -0.08);
}

// ── Filters ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn okf_type_filter_excludes_plain_markdown() {
    let (_dir, project) = setup_project();
    let response = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project: project.clone(),
            query: "revenue".into(),
            tag: None,
            path_filter: None,
            okf_type: Some("attested computation".into()),
            status: None,
            min_trust: None,
            limit: Some(20),
            embed_config: None,
        },
    )
    .await
    .unwrap();
    // Every hit is an Attested Computation; the plain-notes doc is filtered out.
    let files: Vec<&str> = results(&response)
        .iter()
        .filter_map(|r| r["file_path"].as_str())
        .collect();
    assert!(files.contains(&"computations/revenue.md"), "{files:?}");
    assert!(
        files.contains(&"computations/legacy-revenue.md"),
        "{files:?}"
    );
    assert!(!files.contains(&"plain-notes.md"), "{files:?}");
    assert!(!files.contains(&"index.md"), "{files:?}");
}

#[tokio::test]
async fn status_filter_accepts_only_matching_lifecycle() {
    let (_dir, project) = setup_project();
    let response = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project: project.clone(),
            query: "revenue".into(),
            tag: None,
            path_filter: None,
            okf_type: None,
            status: Some("deprecated".into()),
            min_trust: None,
            limit: Some(20),
            embed_config: None,
        },
    )
    .await
    .unwrap();
    let files: Vec<&str> = results(&response)
        .iter()
        .filter_map(|r| r["file_path"].as_str())
        .collect();
    assert_eq!(files, vec!["computations/legacy-revenue.md"]);
}

#[tokio::test]
async fn min_trust_filter_requires_human_review_when_set() {
    let (_dir, project) = setup_project();
    let response = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project: project.clone(),
            query: "revenue".into(),
            tag: None,
            path_filter: None,
            okf_type: None,
            status: None,
            min_trust: Some("human-reviewed".into()),
            limit: Some(20),
            embed_config: None,
        },
    )
    .await
    .unwrap();
    let files: Vec<&str> = results(&response)
        .iter()
        .filter_map(|r| r["file_path"].as_str())
        .collect();
    assert!(files.contains(&"metrics/income-statement.md"), "{files:?}");
    // Legacy revenue is unverified; profit is machine-confirmed only.
    assert!(
        !files.contains(&"computations/legacy-revenue.md"),
        "{files:?}"
    );
    assert!(!files.contains(&"computations/profit.md"), "{files:?}");
}

#[tokio::test]
async fn invalid_min_trust_fails_fast() {
    let (_dir, project) = setup_project();
    let error = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project,
            query: "revenue".into(),
            tag: None,
            path_filter: None,
            okf_type: None,
            status: None,
            min_trust: Some("trust-me".into()),
            limit: None,
            embed_config: None,
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("min_trust"), "{error}");
}

// ── Ranking ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn stable_verified_doc_outranks_identical_deprecated_doc() {
    let dir = tempfile::TempDir::new().unwrap();
    let body = "# Computation\n\n    SELECT SUM(amount) AS revenue FROM finance.revenue\n";
    // Same heading and body; only trust/lifecycle metadata differs, so the
    // BM25 scores tie and the metadata adjustment decides the order.
    std::fs::write(
        dir.path().join("stable.md"),
        format!(
            "---\ntype: Attested Computation\nverified: {{ by: human:ana, at: 2026-06-25T09:00:00Z }}\n---\n{body}"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("deprecated.md"),
        format!("---\ntype: Attested Computation\nstatus: deprecated\nstale_after: 2000-01-01T00:00:00Z\n---\n{body}"),
    )
    .unwrap();

    let response = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project: dir
                .path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            query: "revenue computation".into(),
            tag: None,
            path_filter: None,
            okf_type: None,
            status: None,
            min_trust: None,
            limit: Some(5),
            embed_config: None,
        },
    )
    .await
    .unwrap();
    let files: Vec<&str> = results(&response)
        .iter()
        .filter_map(|r| r["file_path"].as_str())
        .collect();
    assert_eq!(files.first(), Some(&"stable.md"), "{files:?}");
}

#[tokio::test]
async fn reserved_files_are_indexed_with_okf_version() {
    let (_dir, project) = setup_project();
    let response = pitlane_mcp::tools::search_knowledge::search_knowledge(
        pitlane_mcp::tools::search_knowledge::SearchKnowledgeParams {
            project,
            query: "finance bundle concepts".into(),
            tag: None,
            path_filter: None,
            okf_type: None,
            status: None,
            min_trust: None,
            limit: Some(5),
            embed_config: None,
        },
    )
    .await
    .unwrap();
    let index = section_by_file(results(&response), "index.md");
    // `okf_version` is extracted; a doc without `type` is still non-OKF.
    assert_eq!(index["okf_type"], Value::Null);
    assert_eq!(index["trust_tier"], Value::Null);
}
