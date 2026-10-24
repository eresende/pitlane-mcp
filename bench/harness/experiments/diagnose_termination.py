"""Minimal termination diagnostics; never launches benchmark arms or the full suite.

Raw wire data is written ONLY to an ignored/local artifact directory. It may
contain public repository source and model reasoning. Do not commit it.
"""
from __future__ import annotations

import argparse
from dataclasses import asdict
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib

from bench.harness.experiments.navigation_skill import ROOT, DEFAULT_CONFIG, NEUTRAL, digest, dump
from bench.harness.experiments.provider import TracedProvider, read_key, usage_summary
from bench.harness.experiments.tools import IsolatedMCP, ExperimentTools, deadline, DeadlineExceeded, tree_digest
from bench.harness.framework.agentic_loop import AgenticLoop, FINAL_TURN_PROMPT


class SelectedTools:
    def __init__(self, executor, names):
        self.executor = executor
        self.definitions = [t for t in executor.get_tool_definitions() if t.name in names]
        self.names = set(names)

    def get_tool_definitions(self):
        return self.definitions

    def total_response_bytes(self):
        return self.executor.total_response_bytes()

    def execute(self, name, arguments):
        if name not in self.names:
            raise ValueError("Diagnostic requested an unoffered tool")
        return self.executor.execute(name, arguments)


def run_diagnostics(base_url, key, out, snapshot, mcp_home, config, system_template=NEUTRAL, case_filter=None):
    out, snapshot = Path(out), Path(snapshot).resolve()
    before = tree_digest(snapshot)
    # Ground truth comes from the actual tool response, not a fabricated symbol.
    oracle = ExperimentTools(snapshot)
    cargo = oracle.execute("read_file", {"path": "Cargo.toml"})
    package_name = tomllib.loads(cargo.content)["package"]["name"]
    cases = [
        ("no_tools", "What is 2 + 3? Give the answer directly.", set(), "5"),
        ("one_native", "Use read_file to read Cargo.toml. What name is declared in its [package] section? Once you have that name, answer the question; no further research is needed.", {"read_file"}, package_name),
        ("one_mcp", "Use read_code_unit to read Cargo.toml lines 1 through 4. What name is declared in its [package] section? Once you have that name, answer the question; no further research is needed.", {"read_code_unit"}, package_name),
    ]
    rows = []
    for name, prompt, names, expected in cases:
        if case_filter and name != case_filter:
            continue
        print("Diagnostic: " + name, flush=True)
        case_out = out / name
        case_out.mkdir(parents=True, mode=0o700)
        backend = TracedProvider(base_url, config["model"], key, trace_dir=case_out / "wire",
                                 temperature=config["temperature"], max_tokens=config["max_output_tokens"],
                                 request_options=config["request_options"], timeout=config["timeout_seconds"])
        mcp = IsolatedMCP(mcp_home) if name == "one_mcp" else None
        try:
            with deadline(config["setup_timeout_seconds"]):
                if mcp:
                    mcp.startup(str(snapshot))
                executor = ExperimentTools(snapshot, mcp)
            result = None
            try:
                with deadline(config["timeout_seconds"]):
                    result = AgenticLoop().run(prompt, backend, SelectedTools(executor, names),
                                              max_iterations=config["max_iterations"], timeout_seconds=config["timeout_seconds"],
                                              prompt_id=name, mode="diagnostic", repo_path=str(snapshot),
                                              system_prompt=system_template.format(repo=str(snapshot)),
                                              reserve_final_turn=config.get("reserve_final_turn", False))
            except DeadlineExceeded:
                pass
            dump(case_out / "observations.json", backend.observations)
            if result:
                dump(case_out / "parsed_result.json", asdict(result))
            answer = result.final_answer if result else ""
            correct = bool(re.search(r"\b" + re.escape(expected) + r"\b", answer))
            expected_calls = 0 if name == "no_tools" else 1
            row = {"case": name, "status": result.status if result else "timeout", "final_answer": answer,
                   "termination_kind": "budget_synthesis" if result and any(
                       m.role == "user" and m.content == FINAL_TURN_PROMPT for m in result.conversation
                   ) else "natural" if result and result.status == "completed" else "unfinished",
                   "answer_correct": correct, "expected_answer": expected,
                   "tool_calls": len(executor.events), "expected_tool_calls": expected_calls,
                   "tool_errors": sum(e["error"] is not None for e in executor.events),
                   "model_requests": len(backend.usages), "usage": usage_summary(backend.usages),
                   "finish_reasons": [o["finish_reason"] for o in backend.observations],
                   "raw_final_without_tools": any(o["assistant_content"] and not o["tool_calls"] for o in backend.observations),
                   "raw_content_alongside_tools": any(o["assistant_content"] and o["tool_calls"] for o in backend.observations),
                   "observations": backend.observations, "events": executor.events,
                   "passed": bool(result and result.status == "completed" and correct and
                                  len(executor.events) == expected_calls and all(not e["error"] for e in executor.events))}
            dump(case_out / "summary.json", row)
            rows.append(row)
            print(f"  {row['status']}; correct={correct}; calls={row['tool_calls']}; finish={row['finish_reasons']}", flush=True)
        finally:
            if mcp:
                mcp.shutdown()
    assert tree_digest(snapshot) == before, "Diagnostic changed snapshot"
    dump(out / "summary.json", rows)
    return rows


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=os.environ.get("STRATA_BASE_URL"))
    parser.add_argument("--env-file")
    parser.add_argument("--api-key-env", default="STRATA_API_KEY")
    parser.add_argument("--config", default=str(DEFAULT_CONFIG))
    parser.add_argument("--smoke", required=True, help="Existing smoke artifacts with pre-indexed snapshot")
    parser.add_argument("--out", required=True)
    parser.add_argument("--case", choices=["no_tools", "one_native", "one_mcp"])
    parser.add_argument("--iteration-budget", type=int, help="Optional lower diagnostic-only budget to exercise the boundary")
    args = parser.parse_args(argv)
    if not args.base_url:
        parser.error("Set STRATA_BASE_URL or --base-url")
    out = Path(args.out).resolve()
    if out.exists():
        parser.error("Use a new output directory")
    if out.is_relative_to(ROOT) and subprocess.run(["git", "-C", str(ROOT), "check-ignore", "--quiet", str(out)]).returncode:
        parser.error("Diagnostic artifacts must be ignored or outside the repository")
    smoke = Path(args.smoke).resolve()
    config = json.loads(Path(args.config).read_text())
    if args.iteration_budget is not None:
        if not 1 <= args.iteration_budget <= config["max_iterations"]:
            parser.error("Diagnostic budget must be positive and cannot increase the configured limit")
        config["max_iterations"] = args.iteration_budget
    key = read_key(args.api_key_env, args.env_file)
    out.mkdir(parents=True, mode=0o700)
    dump(out / "manifest.json", {"config": config, "provider_endpoint_sha256": digest(args.base_url.encode()),
                                "system_template": NEUTRAL, "system_template_sha256": digest(NEUTRAL.encode()),
                                "snapshot": str(smoke / "repository"), "skill_loaded": False,
                                "case_filter": args.case,
                                "scope": "Three simple termination diagnostics, not the three-arm smoke"})
    rows = run_diagnostics(args.base_url, key, out, smoke / "repository", smoke / "mcp-state", config, case_filter=args.case)
    if not all(r["passed"] for r in rows):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
