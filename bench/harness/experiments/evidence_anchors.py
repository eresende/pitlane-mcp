"""Evaluator-only expected-evidence anchors: recall scaffolding for pilot tasks.

Each anchored component is a preregistered list of repository facts that a
complete answer must reference: exact file paths and distinctive symbol names,
all verified against the snapshot before they are used.

What this measures
------------------
For every component, whether the answer references at least one of its anchors.
That is coverage of required parts, which reference precision alone cannot show:
an answer that cites one true file scored full precision under grounding.

What this does not measure
--------------------------
It cannot tell whether a described relationship is correct, whether a component
was described rather than merely named, or whether the answer is useful. Those
stay with the task-quality rubric review. Relationship anchors are deliberately
not derived here, because co-occurrence of two components cannot distinguish the
direction of a relationship, and reporting that as recall would overstate it.

Anchors are pinned by module hash in the run manifest and never appear in model
prompts, so they cannot leak into the treatment.
"""
from __future__ import annotations

import re
from pathlib import Path

from bench.harness.framework.quality_scorer import (
    _extract_file_references,
    _repository_index,
    _resolve_file,
)

VERSION = "evidence-anchors-v1"

# Component ids match task_quality.TASKS so recall and review describe the same
# parts. Symbol anchors are restricted to distinctive identifiers: common words
# such as `search`, `Walk` or `main` would match ordinary prose and inflate
# recall without evidence.
ANCHORS = {
    "symbol_ignore_logic": {
        "ignore_rules": {"files": ["crates/ignore/src/gitignore.rs"],
                         "symbols": ["Gitignore", "GitignoreBuilder"]},
        "directory_traversal": {"files": ["crates/ignore/src/walk.rs"],
                                "symbols": ["WalkBuilder"]},
        "central_source_owners": {"files": ["crates/ignore/src/dir.rs", "crates/ignore/src/lib.rs"],
                                  "symbols": ["Ignore"]},
    },
    "symbol_cli_config_flow": {
        "flag_parsing": {"files": ["crates/core/flags/parse.rs", "crates/core/flags/defs.rs"],
                         "symbols": ["Parser", "parse_low"]},
        "runtime_configuration": {"files": ["crates/core/flags/hiargs.rs", "crates/core/flags/lowargs.rs"],
                                  "symbols": ["HiArgs", "LowArgs"]},
        "search_execution": {"files": ["crates/core/search.rs"],
                             "symbols": ["SearchWorkerBuilder"]},
        "central_source_owners": {"files": ["crates/core/flags/mod.rs", "crates/core/flags/config.rs"],
                                  "symbols": []},
    },
    "graph_nav_call_chain": {
        "entry_dispatch": {"files": ["crates/core/main.rs"], "symbols": []},
        "search_orchestration": {"files": ["crates/core/search.rs"],
                                 "symbols": ["SearchWorkerBuilder"]},
        "regex_execution": {"files": ["crates/regex/src/matcher.rs", "crates/searcher/src/searcher/mod.rs"],
                            "symbols": ["RegexMatcher", "SearcherBuilder"]},
        "output_writer": {"files": ["crates/printer/src/standard.rs"],
                          "symbols": ["StandardBuilder"]},
    },
    "arch_package_map": {
        "cli": {"files": ["crates/core/flags/parse.rs", "crates/core/flags/defs.rs"],
                "symbols": ["Parser"]},
        "traversal": {"files": ["crates/ignore/src/walk.rs"], "symbols": ["WalkBuilder"]},
        "ignore": {"files": ["crates/ignore/src/gitignore.rs", "crates/ignore/src/dir.rs"],
                   "symbols": ["Gitignore"]},
        "regex_execution": {"files": ["crates/regex/src/matcher.rs"], "symbols": ["RegexMatcher"]},
        "formatting": {"files": ["crates/printer/src/standard.rs", "crates/printer/src/json.rs"],
                       "symbols": ["JSONBuilder"]},
        "central_files": {"files": ["Cargo.toml", "crates/core/main.rs"], "symbols": []},
    },
    "semantic_search_probe": {
        "binary_detection": {"files": ["crates/searcher/src/line_buffer.rs", "crates/core/flags/defs.rs"],
                             "symbols": ["BinaryDetection", "BinaryMode"]},
        "binary_handling": {"files": ["crates/core/flags/hiargs.rs", "crates/core/search.rs"],
                            "symbols": ["HiArgs"]},
        "source_owners": {"files": ["crates/printer/src/standard.rs"], "symbols": []},
    },
}

# Rubric components that objective anchors cannot capture, so they stay with the
# reviewer instead of being scored by co-occurrence. `source_owners` asks whether
# the answer named the central owners of the pipeline, which overlaps the parts
# already anchored and would silently inflate coverage. Anchored plus review-only
# must equal the rubric's component list; verify() enforces that.
REVIEW_ONLY = {
    "graph_nav_call_chain": ["source_owners"],
}

UNANCHORED_FIELDS = {
    "evidence_component_matched": None,
    "evidence_component_count": None,
    "evidence_component_recall": None,
    "evidence_anchor_matched": None,
    "evidence_anchor_count": None,
    "evidence_anchor_recall": None,
    "evidence_review_only": None,
    "evidence_details": None,
}


def module_sha256() -> str:
    """Hash of this anchor definition, recorded so a report is reproducible."""
    import hashlib
    return hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


def verify(repo_path, task_ids=None):
    """Check every anchor against the repository; fail closed on any mismatch.

    A stale anchor must stop the report instead of silently lowering recall, so
    a renamed or moved symbol is a visible error rather than an unnoticed zero.
    """
    repo = Path(repo_path)
    tasks = list(task_ids) if task_ids is not None else list(ANCHORS)
    unknown = [task for task in tasks if task not in ANCHORS]
    if unknown:
        raise ValueError("No evidence anchors for tasks: " + ", ".join(sorted(unknown)))

    # The anchors must describe exactly the rubric's components: anchored plus
    # review-only has to cover every component id, or recall would report a
    # different task than the reviewer scores.
    from bench.harness.experiments.task_quality import TASKS
    for task in tasks:
        expected = set(TASKS[task]["components"])
        covered = set(ANCHORS[task]) | set(REVIEW_ONLY.get(task, []))
        if covered != expected:
            raise ValueError(f"Evidence anchors and rubric disagree for {task}: "
                             f"missing {sorted(expected - covered)}, unknown {sorted(covered - expected)}")

    _, source_files = _repository_index(repo)
    wanted = [symbol for task in tasks for spec in ANCHORS[task].values()
              for symbol in spec.get("symbols", [])]
    absent = _missing_symbols(wanted, source_files)

    problems = []
    files_checked = symbols_checked = 0
    for task in tasks:
        for component, spec in ANCHORS[task].items():
            for relative in spec.get("files", []):
                files_checked += 1
                if not (repo / relative).is_file():
                    problems.append(f"{task}/{component}: missing file {relative}")
            for symbol in spec.get("symbols", []):
                symbols_checked += 1
                if symbol in absent:
                    problems.append(f"{task}/{component}: symbol {symbol} not found")
    if problems:
        raise ValueError("Evidence anchors do not match the snapshot: " + "; ".join(problems))
    return {"version": VERSION, "module_sha256": module_sha256(), "verified": True,
            "tasks": sorted(tasks), "files_checked": files_checked,
            "symbols_checked": symbols_checked}


def assess(task_id, answer, repo_path):
    """Score anchored component coverage for one answer."""
    spec = ANCHORS.get(task_id)
    if spec is None:
        return dict(UNANCHORED_FIELDS)
    repo = Path(repo_path)
    by_basename, _ = _repository_index(repo)
    cited = set()
    for reference in _extract_file_references(answer or ""):
        resolved, _ambiguous = _resolve_file(reference, repo, by_basename)
        if resolved is not None:
            cited.add(resolved)

    details = {}
    components_covered = 0
    anchors_total = anchors_matched = 0
    for component, anchor in spec.items():
        matched = [f for f in anchor.get("files", []) if f in cited]
        matched += [s for s in anchor.get("symbols", [])
                    if re.search(r"\b" + re.escape(s) + r"\b", answer or "")]
        total = len(anchor.get("files", [])) + len(anchor.get("symbols", []))
        anchors_total += total
        anchors_matched += len(matched)
        components_covered += 1 if matched else 0
        details[component] = {"anchors": total, "matched": matched, "covered": bool(matched)}
    return {
        "evidence_component_matched": components_covered,
        "evidence_component_count": len(spec),
        "evidence_component_recall": components_covered / len(spec),
        "evidence_anchor_matched": anchors_matched,
        "evidence_anchor_count": anchors_total,
        "evidence_anchor_recall": anchors_matched / anchors_total if anchors_total else None,
        "evidence_review_only": list(REVIEW_ONLY.get(task_id, [])),
        "evidence_details": details,
    }


def apply(rows, repo_path, task_ids=None):
    """Attach evidence recall to rows in place and return the verification record."""
    check = verify(repo_path, task_ids)
    for row in rows:
        answer = row.get("final_answer") or ""
        row.update(assess(row.get("task_id"), answer, repo_path))
    return check


def _missing_symbols(symbols, source_files):
    """Return symbols absent from every source file, reading each file once."""
    wanted = list(dict.fromkeys(symbols))
    if not wanted:
        return set()
    found: set[str] = set()
    for src_file in source_files:
        if len(found) == len(wanted):
            break
        try:
            text = src_file.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        for symbol in wanted:
            if symbol not in found and re.search(r"\b" + re.escape(symbol) + r"\b", text):
                found.add(symbol)
    return set(wanted) - found
