# P5 C0-COMPILER R7 frozen-skeleton gate

The R6 hostile pre-edit reviewer found that the frozen consumer's Debug implementation did not consume
the contained `FrontendError`, producing a warnings-denied failure.  R7 changes no forwarding or
capability semantics: `ConsumerError::Frontend(error)` now calls
`formatter.debug_tuple("Frontend").field(error).finish()`, preserving the typed error for observation.
The complete skeleton was then copied into the four permitted production locations in a detached
worktree at `126a54f087f7a7d52a39930e6fab15e345ae696c` and gated there.

| command | target | status |
| --- | --- | --- |
| `cargo fmt --manifest-path workspace2/planes/compiler/Cargo.toml --all -- --check` | formatted four-file R7 skeleton | 0 |
| `cargo test --locked --manifest-path workspace2/planes/compiler/Cargo.toml -p nudox-compile-registry` | isolated `CARGO_TARGET_DIR` | 0; 4 dispatch tests and 1 actual-rlib subset test passed |
| `cargo clippy --locked --manifest-path workspace2/planes/compiler/Cargo.toml -p nudox-compile-registry --all-targets -- -D warnings` | same isolated target | 0 |

Toolchain: `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6,
`aarch64-apple-darwin`; `cargo 1.97.1 (c980f4866 2026-06-30)`.

This is a frozen-skeleton calibration gate only.  It does not authorize a production edit and does not
substitute for the fresh R7 four-role calibration, hostile review, or later real candidate gates.
