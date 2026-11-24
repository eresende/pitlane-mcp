"""Task-neutral provisional completion policy, shared by navigation arms A/B/C."""

ANSWER_GUIDANCE = (
    "Keep the final answer concise, at most 300 words, while covering every requested part. "
    "Identify the essential source owners and symbols. Connect the relevant "
    "operations into an evidence-backed execution flow, explaining the effects "
    "of decisions and state changes. State missing evidence and uncertainty "
    "explicitly. Prefer the causal explanation over a symbol inventory or an "
    "exhaustive implementation tour; omit unnecessary implementation details. "
    "Do not invent facts or claim completeness unsupported by the evidence."
)

CAUSAL_GUIDANCE = (
    "Answer the original repository question now using only the evidence already "
    "retrieved in this conversation. No additional tools are available. "
    + ANSWER_GUIDANCE
)
