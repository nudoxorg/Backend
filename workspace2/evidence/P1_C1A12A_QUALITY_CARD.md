# P1 C1a12A strict quality card

Base: isolated repair branch `codex/prototype-canonical-root-hydration-c1a-repair` at
`c90519a7`. This is the first narrow sequential repair after the independently verified C1a11
mechanical correction. It owns production formatting, public documentation, and strict clippy
only. It deliberately does not add behavioral tests, UI fixtures, lab code/TSV, C1b, or P2.

Allowed paths: `crates/nudox-root/src/root_view.rs`, the C1a-added coordinate bridge in
`crates/nudox-root/src/packed.rs`, and immutable locality facts in
`crates/nudox-root/src/locality/artifact/view.rs`; no other production surface unless a clippy
diagnostic on an immediately adjacent C1a added/reworked line requires the smallest documented
repair. No blanket lint allowance, dependency, unsafe code, or semantic/API broadening is
permitted.

Terminal gate: `cargo fmt --all -- --check` and
`cargo clippy -p nudox-root --all-targets -- -D warnings` both pass with no diagnostics. The
worker must retain every C1a10/C1a11 semantic invariant, use short documented helpers rather
than suppression, commit the result on the existing isolated repair branch, and report exact
commands plus LOC. A fresh read-only Terra review then decides whether C1a12B may start.
