# C1 format rescue R3 compliance closure

This receipt supersedes `R2_CLOSURE.md` for compiler-process evidence only. It retains all prior history
and API results, while replacing R2's permissive process fixture predicate.

## R3 repairs

`08ff2fd4` replaced `RUSTC.unwrap_or_else` with an explicit `match`, constructs `--extern` by
`OsString` append, requires zero uncoded primaries in addition to exactly one coded primary, and makes
the legal downstream function return `entities + types`. A real child with E0277 plus
`compile_error!("extra uncoded error")` is asserted to fail the predicate.

`d1f856b6` narrows the abort-summary exemption to exactly `1 previous error`, with only an exact numeric
`warning`/`warnings emitted` rustc trailer allowed. Its direct test accepts the one-primary forms and
rejects zero, two, plural, textual, and malformed forms. The non-semantic LOC ledger is now
`src/lib.rs` 146/210 and `tests/fragment.rs` 322/360, leaving 38 lines of reserve.

Fresh Terra review task `c1_r3_terra_review` (explicit `gpt-5.6-terra`, `fork_turns=none`) returned
amended CLEAR on `d1f856b6`, including focused actual-rlib replay and full crate gates.

## Fresh manager gate and custody

From a fresh target, `cargo fmt --check`, focused `cargo test --locked --test fragment -- --nocapture`,
full `cargo test --locked`, and `cargo clippy --locked --all-targets -- -D warnings` all passed. The
full run reported 1 format unit, 5 format integration, 0 vocabulary unit, 2 vocabulary integration,
0 format doctest, and 1 vocabulary doctest. Diff and status checks were clean.

```text
rlib custody:
path=/private/tmp/p5-c1-r3-final-one.DCOyJ3/debug/deps/libnudox_ir_format-b61fc910457ad02a.rlib
cardinality=1
sha256=cddad9ca9375d7e0bdec5f8ef2a45f05ee66762238d7f4e3e8a4d1949a68df0f
```

The rlib test itself runs external `shasum -a 256`; the temporary path is retained as required rather
than discarded. Allocation, copies, optimized call paths, and whole-consumer codegen remain
UNVERIFIED. No builder/prepared output implementation, C2 surface, merge, or product-closure claim is
made.
