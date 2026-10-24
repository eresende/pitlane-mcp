"""Descriptive summaries; no significance or quality claims from efficiency alone."""
from __future__ import annotations

import csv
import json
from pathlib import Path
import statistics


METRICS = ("quality_score", "correctness_score", "prompt_tokens", "completion_tokens",
           "reasoning_tokens", "cached_tokens", "total_tokens", "execution_seconds", "tool_output_bytes",
           "native_tool_calls", "mcp_tool_calls", "failed_tool_calls", "invalid_tool_arguments",
           "tool_retries", "task_component_score", "task_relationship_score", "task_quality_score",
           "task_error_count", "incidental_error_count", "unsupported_symbol_count",
           "tool_calls", "exact_redundant_successful_calls", "native_fallback_calls")


def describe(values):
    values = [v for v in values if v is not None]
    return {"n": len(values), "mean": statistics.mean(values) if values else None,
            "median": statistics.median(values) if values else None,
            "stdev": statistics.stdev(values) if len(values) > 1 else None,
            "min": min(values) if values else None, "max": max(values) if values else None}


def total(rows, key):
    values = [r.get(key) for r in rows]
    return sum(values) if values and all(v is not None for v in values) else None


def summaries(rows):
    result = {}
    for arm in "ABC":
        group = [r for r in rows if r["configuration"] == arm]
        successful = [r for r in group if r.get("task_success") is True]
        task_successful = [r for r in group if r.get("task_quality_success") is True]
        calls = total(group, "tool_calls")
        failures = total(group, "failed_tool_calls")
        result[arm] = {"runs": len(group), "completed": sum(r["status"] == "completed" for r in group),
                       "reviewed_successes": len(successful),
                       "task_oriented_successes": len(task_successful),
                       "termination_counts": {kind: sum(r.get("termination_kind") == kind for r in group)
                                              for kind in ("natural", "budget_synthesis", "unfinished", "unavailable")},
                       "metrics": {k: describe([r.get(k) for r in group]) for k in METRICS},
                       "tool_failure_rate": failures / calls if calls and failures is not None else None,
                       "successful_only": {k: describe([r.get(k) for r in successful]) for k in METRICS},
                       "task_successful_only": {k: describe([r.get(k) for r in task_successful]) for k in METRICS}}
    return result


def write_report(out, manifest, rows, validation):
    out = Path(out)
    summary = summaries(rows)
    per_task = {task: summaries([r for r in rows if r["task_id"] == task]) for task in sorted({r["task_id"] for r in rows})}
    comparisons = {}
    for left, right in (("A", "B"), ("B", "C"), ("A", "C")):
        comparisons[left + "_vs_" + right] = {
            k: {stat + "_delta_right_minus_left":
                summary[right]["metrics"][k][stat] - summary[left]["metrics"][k][stat]
                if summary[right]["metrics"][k][stat] is not None and summary[left]["metrics"][k][stat] is not None else None
                for stat in ("mean", "median")} for k in METRICS}
    data = {"schema_version": 1, "arms": summary, "per_task": per_task,
            "comparisons": comparisons, "smoke_validation": validation,
            "execution_validation": validation,
            "correctness_evaluation": {"strict_reviewed": sum(r.get("correctness_score") is not None for r in rows),
                                       "task_reviewed": sum(r.get("task_quality_score") is not None for r in rows),
                                       "used_as_execution_gate": False}}
    (out / "summary.json").write_text(json.dumps(data, indent=2) + "\n")
    (out / "results.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
    if rows:
        columns = [k for k in rows[0] if k not in {"tool_events", "usage_per_request", "response_metadata", "final_answer"}]
        with (out / "results.csv").open("w", newline="") as handle:
            writer = csv.DictWriter(handle, fieldnames=columns, extrasaction="ignore")
            writer.writeheader()
            writer.writerows(rows)
    def fmt(v):
        return "unavailable" if v is None else f"{v:.3f}"
    lines = ["# Navigation skill experiment", "", f"Phase: {manifest['phase']}. Runs: {len(rows)}.", "",
             "## Environment", "", "```json", json.dumps(manifest["environment"], indent=2), "```", "",
             f"Repository commit: `{manifest['repo']['commit']}`. Model: `{manifest['config']['model']}`.",
             f"Input identity: `{manifest['identity']}`. Order: `{manifest['schedule']}`.", "",
             "## Method", "",
             "A: native tools; B: native + MCP; C: native + MCP + exact skill text. Fresh conversations and MCP sessions; identical task prompts, sampling, limits, and tracked-file snapshot. Index setup and preflight are excluded from navigation time. Server-provided tool descriptions are present in B and C. No host agent configuration is loaded.", "",
             "The legacy score is repository-reference grounding precision. It does not establish completeness, correct relationships, or task success. Correctness and success stay unavailable until repository-backed manual review. Review rubric: cover every requested component, verify cited files/symbols and relationships, and distinguish inference. Scores: 1 complete/correct, 0.5 partial, 0 incorrect; only 1 with completed execution counts as successful.", "",
             "Technical readiness is an execution gate; correctness is a post-execution outcome. Imperfect completed answers remain benchmark observations. Strict success is unchanged. Task-oriented quality, when reviewed under the pinned task-quality-v1 rubric, separately records components, relationships, task errors, incidental errors and unsupported symbols. Historical annotations are not silently replaced.", "",
             "## Raw summaries", "",
             "| Arm | Runs | Completed | Grounding mean / median | Tokens mean / median | Time mean / median (s) | Native / MCP calls | Failed / calls |",
             "|---|---:|---:|---|---|---|---|---|"]
    for arm, info in summary.items():
        m = info["metrics"]
        pair = lambda key: fmt(m[key]["mean"]) + " / " + fmt(m[key]["median"])
        group = [r for r in rows if r["configuration"] == arm]
        lines.append(f"| {arm} | {info['runs']} | {info['completed']} | {pair('quality_score')} | {pair('total_tokens')} | {pair('execution_seconds')} | {fmt(total(group, 'native_tool_calls'))} / {fmt(total(group, 'mcp_tool_calls'))} | {fmt(total(group, 'failed_tool_calls'))} / {fmt(total(group, 'tool_calls'))} |")
    lines += ["", "| Arm | Input mean / median | Output mean / median | Reasoning mean / median | Tool bytes mean | Invalid arguments | Exact retries |",
              "|---|---|---|---|---:|---:|---:|"]
    for arm, info in summary.items():
        m = info["metrics"]
        pair = lambda key: fmt(m[key]["mean"]) + " / " + fmt(m[key]["median"])
        group = [r for r in rows if r["configuration"] == arm]
        lines.append(f"| {arm} | {pair('prompt_tokens')} | {pair('completion_tokens')} | {pair('reasoning_tokens')} | {fmt(m['tool_output_bytes']['mean'])} | {fmt(total(group, 'invalid_tool_arguments'))} | {fmt(total(group, 'tool_retries'))} |")
    index_path = out / "index_setup.json"
    if index_path.exists():
        index = json.loads(index_path.read_text())
        lines += ["", f"Cold MCP setup including indexing/embedding readiness: {fmt(index.get('cold_setup_seconds'))} seconds (excluded from navigation). Full readiness/version metadata is in index_setup.json."]
    lines += ["", "## Comparisons", "", "Positive deltas mean the right arm is larger; lower tokens/time are not benefits unless correctness is maintained.", "",
              "| Comparison | Grounding mean delta | Total tokens mean delta | Time mean delta (s) |", "|---|---:|---:|---:|"]
    for comparison, values in comparisons.items():
        lines.append("| " + comparison + " | " + " | ".join(fmt(values[k]["mean_delta_right_minus_left"]) for k in ("quality_score", "total_tokens", "execution_seconds")) + " |")
    lines += ["", "## Per-task results", "", "| Task | Arm | Run | Status | Grounding | Correctness | Tokens | Seconds |", "|---|---|---:|---|---:|---|---|---:|"]
    for r in rows:
        lines.append(f"| {r['task_id']} | {r['configuration']} | {r['run_number']} | {r['status']} | {fmt(r['quality_score'])} | {fmt(r['correctness_score'])} | {fmt(r['total_tokens'])} | {fmt(r['execution_seconds'])} |")
    lines += ["", "## Task-oriented quality (independent review)", "",
              "| Task | Arm | Components | Relationships | Task quality | Task errors | Incidental errors | Unsupported symbols | Completion |",
              "|---|---|---:|---:|---:|---:|---:|---:|---|"]
    for r in rows:
        lines.append(f"| {r['task_id']} | {r['configuration']} | " + " | ".join(fmt(r.get(k)) for k in
                     ("task_component_score", "task_relationship_score", "task_quality_score", "task_error_count", "incidental_error_count", "unsupported_symbol_count")) + f" | {r.get('termination_kind', 'unavailable')} |")
    lines += ["", "## Validation and limitations", "", "```json", json.dumps(validation, indent=2), "```", "",
              "Usage is cumulative provider-reported input/output across every model request, including repeated history. Reasoning tokens, if reported, are an output subset and are never added again to total. Missing fields propagate to null. Tool responses are measured as UTF-8 bytes, not estimated tokens. Provider cached-token fields are retained per request and summarised cumulatively when every request reports them; arms run back-to-back on identical prompts can otherwise differ through provider prompt caching rather than navigation behavior.", "",
              "Exact repeated successful calls and exact retries after failure are mechanical proxies. Semantic redundancy, appropriate selection, unnecessary MCP calls and quality require manual review; unavailable values are null. Native fallbacks count native calls after the first MCP call; initial native use is recorded separately by total native count.", "",
              "The pilot tasks are fixed in pilot_tasks; legacy full tasks remain separately configured. Neither suite establishes hypothetical change-impact accuracy. The native baseline is the existing harness's three read/search/list tools, not a full coding CLI. Skill loading is explicit system-prompt injection, not CLI auto-discovery. Snapshots omit agent configuration and Git history, and reject escaping/broken symlinks; diff analysis is not evaluated.", "",
              "The provider is a hosted endpoint, so engine version, quantization and hardware are provider-managed and not observable from the API. The model listing is sampled before/after each run, which detects a changed or withdrawn model but cannot detect an operator-side deployment change. No model is downloaded and no inference settings are changed.", "",
              "Strict-success and task-success efficiency are separate in summary.json, with n/mean/median/sample standard deviation/min/max. Across-task variation is not repeated-run variation; a one-attempt-per-task pilot cannot establish repeatability or significance.", "",
              "## Reproduce", "", "See bench/harness/experiments/README.md. All machine-readable inputs are pinned in run_manifest.json; requests contain no provider retries. Run smoke, review answers against the snapshot, enter reviews.json, re-report, then explicitly invoke full with --validated-smoke. Artifacts and answers are local and must not be committed.", "",
              "## Conclusion", "", "This report describes observed execution and grounding. Effectiveness remains undetermined until correctness review and repeated runs are complete.", ""]
    (out / "report.md").write_text("\n".join(lines))
