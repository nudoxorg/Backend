# R11 frozen subset observability gate

The R11 frozen `subset.rs` SHA-256 is
`05bc6f455335106bac3812f4cde299d97890592fddd1c07a6ec1043213c77b8a`.
In a detached scratch copy of R11 custody, the candidate test path was made byte-identical to this source
and run with `--nocapture`; any inherited compiler stdout would therefore appear in the terminal.

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=<isolated-target> cargo test --locked \
  --manifest-path planes/compiler/Cargo.toml -p nudox-compile-registry --test subset -- --nocapture

running 1 test
test typescript_lower_is_a_causal_absent_member ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Only ordinary Cargo/test text appeared; no binary metadata preceded the named test. The child directs
metadata stdout to `Stdio::null()` and pipes stderr. Its text predicate requires exactly one coded
`error[E...]` header, the E0599 code and required names, and rejects any uncoded error header except the
literal one-previous-error summary. The metadata request creates no output file. This frozen-skeleton
gate is not production-edit authority; R11 still requires all four readers and hostile pre-edit review.
