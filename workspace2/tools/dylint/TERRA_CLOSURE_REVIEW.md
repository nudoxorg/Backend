# Independent semantic-enforcement closure review

The read-only `gpt-5.6-terra` reviewer found five defects in the first implementation and three more
during closure. This record preserves the counterexamples so later work cannot mistake warm-machine
success for a complete gate.

Closed defects:

- local macro expansions were skipped together with external macros;
- `map_err(|_source| Replacement)` laundered a discarded source error;
- direct trait-object aliases bypassed the dynamic-dispatch rule;
- nested aliases such as `type Wrapped = Box<dyn Trait>` bypassed the rule;
- only the root workspace received the complete quality gate;
- the UI runner accepted an empty fixture directory;
- the unsafe-custody documentation described two files while enforcement named four modules;
- one layout experiment retained an avoidable `unreachable!` branch.

The nested-alias repair walks the resolved compiler type recursively, while a nearby non-dynamic alias
remains legal. The UI inventory now fails when empty or when either side of an `.rs`/`.stderr` pair is
missing.

Open blocker:

- an empty Cargo home cannot build the lint workspace offline because the flake does not yet carry the
  pinned Rust-Clippy Git source, and Dylint still discovers its driver under the user's home directory.
  The falsifier is the complete quality gate with empty `CARGO_HOME` and `DYLINT_DRIVER_PATH` after
  only entering the offline Nix environment.

No product or roadmap closure may cite this lane until that falsifier passes.
