# Pinned offline tooling finding F-03

## Independent reproduction

Manager checkout `9bfc26272efaf2ad5fb2e91c404e0d45f9269fc8` used a new disposable root
`/tmp/nudox-a2-offline-control.AgLgDL` with empty `cargo-home`, `target`, and `tmp` directories. The
pinned command was:

```text
nix develop ./workspace2#quality --command sh -c \
  'export PATH="$NUDOX_STABLE_TOOLCHAIN/bin:$PATH"; \
   export CARGO_HOME="$1/cargo-home"; export CARGO_TARGET_DIR="$1/target"; \
   export TMPDIR="$1/tmp"; \
   cargo test --manifest-path workspace2/adapters/durable-journal/Cargo.toml \
     --locked --offline --features wave-a2-publication-red --test wave_a2_red' \
  sh /tmp/nudox-a2-offline-control.AgLgDL
```

Nix entered the pinned environment and Cargo stopped before compilation with `no matching package
named blake3 found`, searched in the crates.io index. The expected missing-publication import error was
therefore not reached.

`workspace2/tools/dylint/DECISIONS.md` and `TERRA_CLOSURE_REVIEW.md` independently record the same
unfinished closure class for the pinned Rust-Clippy Git source and generated Dylint driver: the current
flake intentionally does not supply them to an empty Cargo/driver cache, and calls fresh-cache offline
closure a release blocker. `workspace2/flake.nix` supplies toolchains and Dylint executables, not a
vendored Cargo registry or the lint workspace's Git checkout.

## Smallest correction and ownership

The smallest reproducible correction is a shared quality-environment change: place immutable Cargo
registry sources for every locked shipping manifest, the pinned `rust-clippy` Git revision used by
`tools/dylint/nudox-semantic-lints`, and the generated Dylint driver in the Nix closure or a
repository-owned vendor tree; configure `CARGO_HOME`/Cargo source replacement and driver discovery to
use only those paths. The fresh-cache red, quality, and Dylint commands must then run with empty
external `CARGO_HOME`, `RUSTUP_HOME`, `CARGO_TARGET_DIR`, and `TMPDIR` beneath a disposable build root.

This capability cannot make that correction: its allowed source scope is durable-journal plus its
evidence directory, while the required files are the shared `workspace2/flake.nix`, Cargo/vendor
configuration, and Dylint tooling. Until the shared quality owner supplies that closure, F-03 remains
an external tooling block and no warm-cache result is recorded as a gate pass.
