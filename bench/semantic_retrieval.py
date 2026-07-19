#!/usr/bin/env python3
"""Repeatable, LLM-independent semantic retrieval benchmark.

The harness queries Pitlane's CLI directly, records exact rankings/scores, and
computes hit@1/3/5 plus mean reciprocal rank. It intentionally does not call
`investigate`, `locate_code`, an LLM, or fallback content searches.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Case:
    name: str
    query: str
    expected: tuple[str, ...]


CASES = (
    Case(
        "gpu_offload",
        "Where is GPU layer offloading implemented?",
        (
            "llama_model_base::load_tensors",
            "llama_model::n_gpu_layers",
            "llama_supports_gpu_offload",
        ),
    ),
    Case(
        "kv_cache_allocation",
        "Where is KV cache allocation implemented?",
        ("llama_kv_cache::llama_kv_cache",),
    ),
    Case(
        "embedding_pooling",
        "Where are embedding pooling strategies implemented?",
        (
            "llm_graph_context::build_pooling",
            "llama_context::pooling_type",
            "TextModel::_try_set_pooling_type",
        ),
    ),
    Case(
        "openai_embeddings",
        "Where are OpenAI compatible embedding requests handled?",
        (
            "server_routes::handle_embeddings_impl",
            "format_embeddings_response_oaicompat",
            "server_context_impl::send_embedding",
        ),
    ),
)


def run_case(command: list[str], project: Path, case: Case, limit: int) -> dict:
    proc = subprocess.run(
        command
        + [
            "search",
            str(project),
            case.query,
            "--mode",
            "semantic_debug",
            "--limit",
            str(limit),
        ],
        check=True,
        capture_output=True,
        text=True,
        env=os.environ.copy(),
    )
    payload = json.loads(proc.stdout)
    rankings = []
    first_rank = None
    for rank, result in enumerate(payload.get("results", []), 1):
        qualified = result.get("qualified") or result.get("name") or result.get("id")
        semantic = result.get("semantic") or {}
        rankings.append(
            {
                "rank": rank,
                "id": result.get("id"),
                "file": result.get("file"),
                "symbol": qualified,
                "kind": result.get("kind"),
                "raw_similarity": semantic.get("raw_similarity"),
                "final_score": semantic.get("final_score", result.get("score")),
                "adjustments": semantic.get("adjustments", {}),
            }
        )
        if first_rank is None and any(expected in qualified for expected in case.expected):
            first_rank = rank
    return {
        "name": case.name,
        "query": case.query,
        "expected": case.expected,
        "first_relevant_rank": first_rank,
        "rankings": rankings,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("project", type=Path, help="indexed llama.cpp checkout")
    parser.add_argument("--pitlane", type=Path, default=Path("target/debug/pitlane"))
    parser.add_argument("--limit", type=int, default=10)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    results = [run_case([str(args.pitlane)], args.project, case, args.limit) for case in CASES]
    ranks = [result["first_relevant_rank"] for result in results]
    metrics = {
        "queries": len(results),
        "top_1_accuracy": sum(rank is not None and rank <= 1 for rank in ranks) / len(ranks),
        "top_3_accuracy": sum(rank is not None and rank <= 3 for rank in ranks) / len(ranks),
        "top_5_accuracy": sum(rank is not None and rank <= 5 for rank in ranks) / len(ranks),
        "mean_reciprocal_rank": sum(0.0 if rank is None else 1.0 / rank for rank in ranks)
        / len(ranks),
    }
    report = {
        "project": str(args.project.resolve()),
        "model": os.environ.get("PITLANE_EMBED_MODEL"),
        "document_profile": os.environ.get(
            "PITLANE_EMBED_DOCUMENT_PROFILE", "metadata_code"
        ),
        "metrics": metrics,
        "cases": results,
    }
    output = json.dumps(report, indent=2)
    print(output)
    if args.output:
        args.output.write_text(output + "\n")


if __name__ == "__main__":
    main()
