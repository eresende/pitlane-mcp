"""Reviewer-only, versioned task quality; never included in model instructions."""
from __future__ import annotations

VERSION = "task-quality-v1"
TASKS = {
    "symbol_ignore_logic": {
        "components": ["ignore_rules", "directory_traversal", "central_source_owners"],
        "relationships": ["traversal_consults_matching", "matching_affects_visitation"],
    },
    "symbol_cli_config_flow": {
        "components": ["flag_parsing", "runtime_configuration", "search_execution", "central_source_owners"],
        "relationships": ["parsing_updates_options", "options_convert_to_runtime_config", "runtime_config_drives_search"],
    },
    "graph_nav_call_chain": {
        "components": ["entry_dispatch", "search_orchestration", "regex_execution", "output_writer", "source_owners"],
        "relationships": ["entry_to_search", "search_to_matching", "match_to_printing", "printing_to_stdout"],
    },
    "arch_package_map": {
        "components": ["cli", "traversal", "ignore", "regex_execution", "formatting", "central_files"],
        "relationships": ["cli_to_orchestration", "traversal_and_ignore", "matcher_searcher_printer_roles"],
    },
    "semantic_search_probe": {
        "components": ["binary_detection", "binary_handling", "source_owners"],
        "relationships": ["detection_trigger", "configuration_to_policy", "binary_policy_effects"],
    },
}


def review_template(task_id):
    return {"rubric_version": VERSION,
            **{dimension: [{"id": item, "score": None, "evidence": ""} for item in items]
               for dimension, items in TASKS[task_id].items()},
            "task_errors": [], "incidental_errors": [], "unsupported_symbols": [],
            "review_complete": False}


def assess(task_id, review):
    """Derive scores from anchored checks, never infer correctness from termination."""
    if review is None or review.get("review_complete") is not True:
        return {k: None for k in ("task_component_score", "task_relationship_score", "task_quality_score",
                                  "task_error_count", "incidental_error_count", "unsupported_symbol_count")}
    if review.get("rubric_version") != VERSION:
        raise ValueError("Unknown task quality rubric version")
    means = {}
    for dimension, ids in TASKS[task_id].items():
        checks = review.get(dimension, [])
        if len(checks) != len(ids) or {c.get("id") for c in checks} != set(ids):
            raise ValueError("Task quality requires the exact preregistered checks")
        for check in checks:
            if type(check.get("score")) not in (int, float) or check["score"] not in (0, 0.5, 1) or not check.get("evidence"):
                raise ValueError("Every task quality check requires a score and source evidence")
        means[dimension] = sum(c["score"] for c in checks) / len(checks)
    for kind in ("task_errors", "incidental_errors", "unsupported_symbols"):
        if not isinstance(review.get(kind), list):
            raise ValueError("Quality error collections must be explicit lists")
        for error in review[kind]:
            if not error.get("claim") or not error.get("evidence"):
                raise ValueError("Every error requires a claim and source evidence")
            if kind == "unsupported_symbols" and error.get("scope") not in ("required", "optional"):
                raise ValueError("Invented symbols require required/optional scope")
    value = (means["components"] + means["relationships"]) / 2
    if review["task_errors"] or any(e["scope"] == "required" for e in review["unsupported_symbols"]):
        value = min(value, 0.5)
    return {"task_component_score": means["components"], "task_relationship_score": means["relationships"],
            "task_quality_score": value, "task_error_count": len(review["task_errors"]),
            "incidental_error_count": len(review["incidental_errors"]),
            "unsupported_symbol_count": len(review["unsupported_symbols"])}
