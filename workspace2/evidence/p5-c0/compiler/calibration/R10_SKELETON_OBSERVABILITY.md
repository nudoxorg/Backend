# R10 frozen subset observability gate

The R10 frozen `subset.rs` SHA-256 is
`ebca09d89eaf5a4b4ec438ea3b5cc4df8671f535803576e81d69ec0bad5da6e9`.
In a detached scratch copy of the R10 custody commit, the candidate test path was made byte-identical to
that frozen source and run with `--nocapture` so any inherited child stdout would be visible.

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=<isolated-target> cargo test --locked \
  --manifest-path planes/compiler/Cargo.toml -p nudox-compile-registry --test subset -- --nocapture

running 1 test
test typescript_lower_is_a_causal_absent_member ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The terminal contains only ordinary Cargo/test text; no binary rustc metadata precedes the named test.
The child still asks rustc for `--emit=metadata=-`, but `compile` gives its stdout
`Stdio::null()` and pipes stderr for the exact E0599 predicate. Therefore metadata is neither emitted to
the terminal nor written to a filesystem artifact. This gate is a frozen-skeleton observation only; it
does not grant production edit authority before R10 calibration and hostile pre-edit review.
