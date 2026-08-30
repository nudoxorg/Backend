# P1 C1a12C1 final format/replay card

Base: `c1b7449d` on the isolated repair branch. Independent lab review verified literal allocator
scope law, source/TSV boundaries, replayed 24 rows, strict lab clippy, and 227/300 lab LOC, but
blocked because the committed lab binary is not rustfmt-clean. This card permits exactly
`layout-lab/src/bin/p1-canonical-root-control.rs` and
`layout-lab/raw/p1-canonical-root-control.tsv`.

Run rustfmt only on the lab source, rerun strict release clippy, then rerun the exact release
command into the committed TSV from the formatted source. Do not change lab behavior, expected
facts, workload set, scope placement, output schema, environment claims, production/test/UI
code, manifests, or dependencies. Commit the formatted source and newly actual TSV. A final
read-only Terra closure review must verify formatting and replay facts before manager integration.
