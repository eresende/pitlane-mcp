# Analyze-impact baselines

The ignored integration test `ripgrep_analyze_impact_response_baseline` records the current
response-size, evidence-duplication, and provenance behavior for a dense `analyze_impact`
query against the ripgrep 14.1.1 fixture.

Set up the pinned fixture and run the baseline with:

```bash
bash bench/setup.sh
cargo test --test analyze_impact_baseline -- --ignored --nocapture
```

The test normalizes the checkout path and removes session-only response fields before
measuring. When intentionally changing the response, compare the printed metrics with
`analyze-impact-ripgrep.json`, verify that the directional changes are expected, and then
update the checked-in baseline.

The pre-fix run for issues #129 and #130 measured 51,512 serialized bytes, 323
reason entries (22,808 bytes), and top-level provenance that combined symbol and
file aggregates. The fixed baseline measures 13,120 bytes with no serialized
reasons or duplicate structured support edges. Top-level provenance now equals
the returned-symbol totals.

Future changes should preserve these properties:

- substantially fewer `serialized_bytes`, `reason_bytes`, and duplicate evidence entries;
- no file-level repetition of symbol reasons or support edges;
- top-level provenance totals equal the returned-symbol totals, independent of display
  evidence truncation.
