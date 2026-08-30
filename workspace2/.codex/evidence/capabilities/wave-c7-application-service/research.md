# Wave C.7 research journal (Phase 0)

| Source/experiment | Observation | Decision informed | Saturation |
|---|---|---|---|
| `nudox-id` public types (bounded `rg` inspection) | Snapshot/generation identity is typed and domain checked; application must pass identity through rather than reconstruct raw bytes | C7-01/C7-02 | control confirmed |
| `nudox-root` locality/view APIs (bounded `rg` inspection) | Locality facts are validated against a generation and expose deterministic scans/lookups | C7-03 | control confirmed |
| `nudox-operation`, `nudox-runtime`, `nudox-workflow` manifests/source | Existing operation vocabulary and bounded terminal/progress mechanisms are concrete; application should compose, not duplicate queues or state | C7-05/C7-06 | control confirmed |
| `nudox-observe` source/tests | Typed lazy Probe seam is accepted; disabled-interest path must remain builder-lazy | C7-08 | control confirmed |
| Standard-library control decision | Start with explicit closed enums and bounded fixed-capacity state; no dispatch macro/generic abstraction is justified before two consumers and codegen evidence | all rows | control retained |

No backend or adapter choice is authorized by this journal. The service imports the compiler vocab
and index vocab crates through the nested planes; root-owned manifest wiring is required before
focused Cargo gates can run.
