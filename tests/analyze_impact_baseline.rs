use std::{collections::HashSet, path::Path, process::Command};

use pitlane_mcp::tools::{
    index_project::{index_project, IndexProjectParams},
    orchestrator::{analyze_impact, AnalyzeImpactParams},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const RIPGREP_REVISION: &str = "4649aa9700619f94cf9c66876e9549d83420e16c";
const SEED_SYMBOL: &str = "crates/core/flags/hiargs.rs::HiArgs::matcher#method";
const BASELINE: &str = include_str!("../bench/baselines/analyze-impact-ripgrep.json");

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct BaselineMetrics {
    serialized_bytes: usize,
    returned_impact_symbols: usize,
    returned_impact_files: usize,
    total_impact_symbols: u64,
    total_impact_files: u64,
    symbol_reason_entries: usize,
    file_reason_entries: usize,
    reason_bytes: usize,
    duplicate_reason_entries: usize,
    support_edge_entries: usize,
    unique_support_edges: usize,
    duplicate_support_edges: usize,
    symbol_direct_calls: u64,
    symbol_direct_references: u64,
    file_direct_calls: u64,
    file_direct_references: u64,
    summary_direct_calls: u64,
    summary_direct_references: u64,
}

#[tokio::test]
#[ignore = "requires the pinned ripgrep checkout created by bench/setup.sh"]
async fn ripgrep_analyze_impact_response_baseline() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = root.join("bench/repos/ripgrep");
    assert!(
        repo.join(".git").is_dir(),
        "missing {}; run `bash bench/setup.sh` first",
        repo.display()
    );
    assert_eq!(
        git_revision(&repo),
        RIPGREP_REVISION,
        "ripgrep fixture moved"
    );

    let project = repo.to_string_lossy().into_owned();
    index_project(IndexProjectParams {
        path: project.clone(),
        exclude: None,
        force: Some(false),
        max_files: None,
        progress_token: None,
        peer: None,
        embed_config: None,
        on_index_progress: None,
        on_phase3_progress: None,
    })
    .await
    .expect("index ripgrep fixture");

    let mut response = analyze_impact(AnalyzeImpactParams {
        project: project.clone(),
        query: None,
        symbol_id: Some(SEED_SYMBOL.to_owned()),
        file_path: None,
        scope: None,
        depth: Some(3),
        limit: Some(12),
    })
    .await
    .expect("analyze ripgrep impact");

    normalize_response(&mut response, &project);
    let actual = measure(&response);
    let expected: BaselineMetrics =
        serde_json::from_str(BASELINE).expect("parse analyze-impact baseline");

    println!(
        "{}",
        serde_json::to_string_pretty(&actual).expect("serialize metrics")
    );
    assert_eq!(actual, expected);
}

fn git_revision(repo: &Path) -> String {
    let output = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .expect("run git rev-parse");
    assert!(output.status.success(), "git rev-parse failed");
    String::from_utf8(output.stdout)
        .expect("git revision is UTF-8")
        .trim()
        .to_owned()
}

fn normalize_response(response: &mut Value, project: &str) {
    if let Some(object) = response.as_object_mut() {
        object.remove("index_revision");
        object.remove("session_state");
        object.remove("steering");
    }
    normalize_paths(response, project);
}

fn normalize_paths(value: &mut Value, project: &str) {
    match value {
        Value::String(text) => *text = text.replace(project, "<repo>"),
        Value::Array(values) => {
            for value in values {
                normalize_paths(value, project);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                normalize_paths(value, project);
            }
        }
        _ => {}
    }
}

fn measure(response: &Value) -> BaselineMetrics {
    let symbols = array(response, "impact_symbols");
    let files = array(response, "impact_files");
    let symbol_reasons = reasons(symbols);
    let file_reasons = reasons(files);
    let all_reasons: Vec<_> = symbol_reasons
        .iter()
        .chain(file_reasons.iter())
        .copied()
        .collect();
    let unique_reasons: HashSet<_> = all_reasons.iter().copied().collect();

    let support_edges: Vec<_> = symbols
        .iter()
        .flat_map(|item| {
            array(item, "support_edges")
                .iter()
                .map(move |edge| (text(item, "id"), edge))
        })
        .chain(files.iter().flat_map(|item| {
            array(item, "support_edges")
                .iter()
                .map(move |edge| (text(item, "file"), edge))
        }))
        .collect();
    let unique_support_edges: HashSet<_> = support_edges
        .iter()
        .map(|(impacted_id, edge)| {
            (
                *impacted_id,
                text(edge, "direction"),
                text(edge, "relation"),
                text(edge, "via_symbol_id"),
            )
        })
        .collect();

    let (symbol_direct_calls, symbol_direct_references) = provenance_totals(symbols);
    let (file_direct_calls, file_direct_references) = provenance_totals(files);
    let summary = &response["edge_provenance_summary"];

    BaselineMetrics {
        serialized_bytes: serde_json::to_vec(response).unwrap().len(),
        returned_impact_symbols: symbols.len(),
        returned_impact_files: files.len(),
        total_impact_symbols: number(response, "total_impact_symbols"),
        total_impact_files: number(response, "total_impact_files"),
        symbol_reason_entries: symbol_reasons.len(),
        file_reason_entries: file_reasons.len(),
        reason_bytes: all_reasons.iter().map(|reason| reason.len()).sum(),
        duplicate_reason_entries: all_reasons.len() - unique_reasons.len(),
        support_edge_entries: support_edges.len(),
        unique_support_edges: unique_support_edges.len(),
        duplicate_support_edges: support_edges.len() - unique_support_edges.len(),
        symbol_direct_calls,
        symbol_direct_references,
        file_direct_calls,
        file_direct_references,
        summary_direct_calls: number(summary, "direct_calls"),
        summary_direct_references: number(summary, "direct_references"),
    }
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn reasons(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .flat_map(|item| array(item, "reasons"))
        .filter_map(Value::as_str)
        .collect()
}

fn provenance_totals(items: &[Value]) -> (u64, u64) {
    items.iter().fold((0, 0), |(calls, references), item| {
        (
            calls + number(&item["provenance"], "direct_calls"),
            references + number(&item["provenance"], "direct_references"),
        )
    })
}

fn number(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
