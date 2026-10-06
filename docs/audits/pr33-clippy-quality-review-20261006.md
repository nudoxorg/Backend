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

### Six-crate follow-up

The later six-crate audit used a fresh complete fleet admission and a private per-attempt sccache directory/socket. The all-target run without extra lint overrides reached the selected crates but failed on 31 existing test-only `unwrap_used` diagnostics: 25 in `backend-store` and 6 in `backend-semantic`. Those tests remain enabled; the next bounded change will replace the unwraps with contextual expectations or explicit assertions.

The production-library-only command, `cargo clippy --locked --lib -p backend-semantic -p backend-library -p backend-store -p backend-engine -p backend-local-service -p backend-extension-turso`, exited 0 in 42.3 seconds. It emitted existing warnings (semantic 991, library 925, store 145, engine 1,863, local-service 1,014, Turso 156); this is a successful command, not a warning-free result. The all-target attempt with `-D clippy::undocumented_unsafe_blocks` failed on 16 existing FFI warnings in `backend-frontend-clang`; that extra deny was removed for the ordinary all-target audit.

The all-target log is `.local/quality/clippy-sixcrate-normal-lints-20261006T2255.log`, SHA-256 `605412eb5f25c67834763c2b956ac29db5845bb0812d42e7819cc82c3f1102a7` (result `ac82ee2db72831876de019c46791942a2ce17d4cc9125480372e3a3b5d8d456f`). The production-only log is `.local/quality/clippy-sixcrate-lib-lints-20261006T2258.log`, SHA-256 `7d2b80c730f316ba4e5f02e08c3ce11052c1c6940b11aef9684a36fd0f2efbe0` (result `e8c68af8a03dfdd498b5839dca2c37ec3a10ccc48320d1bb7a9189edc18b153f`). The stricter all-target retry log is `.local/quality/clippy-sixcrate-private-sccache-20261006T2251.log`, SHA-256 `763fa703e3093c8df0239b1a724b3e6c97ed9efea0d9ab34575f9513ed23c07a` (result `9fecb555801f2b193e8057c8573487f6b525a09aa5412980f944b7470a1e60ee`). Fresh complete fleet admissions for these runs are preserved as `fleet-admission-20261006T2251.json`, `fleet-admission-20261006T2255.json`, and `fleet-admission-20261006T2258.json` in `.local/quality/`.

Warnings in changed production files were checked against `git blame`. The only new Clippy warning identified in the three quality commits is `similar_names` for `expected_pid` beside `expected_pgid` in `macos_process.rs`; the remaining warnings in touched advisory and Mach-O files predate this branch. No broad pedantic cleanup was included.
