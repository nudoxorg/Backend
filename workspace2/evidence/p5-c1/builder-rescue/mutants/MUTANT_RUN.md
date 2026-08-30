# C1 builder rescue mutant custody

Exact production source: `workspace2/domains/ir/crates/nudox-ir-format/src/lib.rs`, SHA-256
`7e31c1b17c52c0d077300d6820f85267fce1ba684ee22cbda2084403987ad5cc` at candidate
`6be0bd2c6aa752a3d51ff7b09e1e9b955a26feff`. Each patch was applied only to that copied source state,
its named focused test was run, then the patch was reversed; the committed source was restored before this
receipt was written.

| mutant | patch SHA-256 | command | status | retained red result |
| --- | --- | --- | ---: | --- |
| constant body | `81005d3d2e9d44f921ac184009cb0435d8d99ead0254838414f6fd9c21ba1a87` | `CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-mutant-constant RUSTC_WRAPPER= cargo test --locked --manifest-path workspace2/domains/ir/Cargo.toml -p nudox-ir-format --test fragment prepared_writer_zero_one_two_full_width_goldens_and_cursors` | 101 | red at `tests/fragment.rs:66`: actual `[193,1,1,1,68,51,34,17,68,51,34,17]`, expected full-width canonical `[193,1,1,1,4,3,2,1,208,192,176,160]`. |
| partial write | `59556103152f3f175f42f506aa6982d410d78cb685433c791c95fab1518bcb4d` | `CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-mutant-partial RUSTC_WRAPPER= cargo test --locked --manifest-path workspace2/domains/ir/Cargo.toml -p nudox-ir-format --test fragment prepared_writer_all_short_outputs_are_unchanged` | 101 | red at `src/lib.rs:166`: index out of bounds for zero-length caller output before capacity check. |

Pristine focused tests were included in the locked all-targets run at
`/private/tmp/p5-c1-builder-rescue-verify2`, status 0. The old magic-only mutant remains a named rejected
counterexample and was not used.
