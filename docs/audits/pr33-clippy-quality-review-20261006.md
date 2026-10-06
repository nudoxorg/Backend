# PR 33 Rust quality review, 2026-10-06

This review is isolated from the user's dirty `tools.nix`. The quality branch is `codex/quality-pr33-20261006`, based on PR 33 source `b6049f4d2528bba8ef3932b73cf211e04d67c3bb`; the reviewed source checkpoint is `c35c6801b9121205abb3e24e38e1c98b5783a9ae` (tree `65c22129867e1c20e79a1d28ff9fc6427545b10e`).

## Changes

- `9bb66d64b941138cabefc56b63ed3c10753b1b78` makes file-size, PID, and Mach-O integer conversions checked, removes panic paths from malformed Mach-O parsing, modernizes pointer casts, and adds malformed-word coverage.
- `6a7ad142e83abf308c7167e8e9078d42f395cc3d` gives fixed-width journal fields typed `u16` sizes and retains compile-time assertions for the 32-byte header and 68-byte record.
- `c35c6801b9121205abb3e24e38e1c98b5783a9ae` removes a redundant advisory-map `expect` and sizes OSV index arrays with `usize`, checking conversion to the stored `u64` at accounting boundaries.

## Baseline and findings

The strict baseline Clippy run on `b6049f4d` failed in `backend-platform` before the selected semantic/library/store crates were reached. Its 98 denied warnings were: 44 `missing_errors_doc`, 15 `needless_return`, 10 `expect_used`, 9 `cast_possible_wrap`, 5 `doc_markdown`, 4 `cast_sign_loss`, 2 `cast_possible_truncation`, 2 `needless_continue`, and one each `similar_names`, `too_many_lines`, `ineffective_open_options`, `items_after_statements`, `ptr_as_ptr`, `borrow_as_ptr`, and `collapsible_if`.

The audit addressed the unsafe conversion/parser findings and the fixed wire-width issue. `cast_possible_wrap` findings are signed `OpenFlags` bit-mask conversions and were left unchanged because their signed bit interpretation is intentional. Broad documentation and pedantic-style cleanup remains separate from this bounded safety/type pass.

## Validation and limits

`cargo test --locked --all-targets -p backend-platform` passed 74/74 after the final source changes. Raw log: `.local/quality/followup-c35-platform-and-focused-clippy-20261006T2230.log`, SHA-256 `04b3c741fb8ccdf0333ad7ccf0322a65e3ffde295f16708200c11ad844b26e60`.

The focused Clippy run with strict conversion/panic/pointer lints advanced beyond the original parser findings, then reported existing `expect_used` debt across store and semantic library/test targets. It did not establish a clean lint result for those crates. The later six-crate default-warning audit stopped while compiling the Tokio dependency: the shared sccache process failed to open an input with `EMFILE` (`Too many open files`), before a complete result for the selected packages. Raw log SHA-256 is `8bf04c72985258e421dab45d295d26650afb6ffe762484a6e69fcab40c4a903e`.

Fleet admission evidence is preserved outside Git under `.local/quality/`. The complete 22:37 sample allowed remote scheduling for two jobs but was not used; the 22:41 sample denied scheduling because ILO already had a Cargo group exceeding the four-job ceiling. No build was started from the denied sample. No selected-crate full Clippy pass is claimed.

Receipt details:

- Baseline strict attempt: `.local/quality/clippy-semantic-library-store-20261006T2202.log`, SHA-256 `c983f45d31281d1a4bbd91b223299f73486dc375e9b1789feb16820f736817ec`.
- Final focused run: `.local/quality/followup-c35-platform-and-focused-clippy-20261006T2230.log`, SHA-256 `04b3c741fb8ccdf0333ad7ccf0322a65e3ffde295f16708200c11ad844b26e60`; its result record is `.local/quality/followup-c35-platform-and-focused-clippy-20261006T2230.result.json`.
- Six-crate interrupted audit: `.local/quality/clippy-sixcrate-audit-fd8192-20261006T2232.log`, SHA-256 `8bf04c72985258e421dab45d295d26650afb6ffe762484a6e69fcab40c4a903e`; result record is `.local/quality/clippy-sixcrate-audit-fd8192-20261006T2232.result.json`.
- Fresh but unused allowed sample: `.local/quality/fleet-admission-20261006T2237-unlaunched.json`, SHA-256 `5b69b154226fe7e6f72e219b2f9475e02eea04923ece9ca8c257e6dff175c2e0`.
- Most recent blocked sample: `.local/quality/fleet-admission-20261006T2241-blocked.json`, SHA-256 `74702b1e59626ed919c4fdedaa50f1c2c47115b3d4baae2f8de66864d5222365`.
- The private audit supervisor was prepared to isolate the next sccache daemon via per-attempt cache directory; helper SHA-256 `fa9e801cfdca5569450fc9b179f878c14e911fadda159b11766d8ced14ba4c41`. This setup was not run.
