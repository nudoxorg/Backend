# Rust corpus lifecycle (2026-09-04)

Selection rule: enumerate registry package directories containing `Cargo.toml`
and `src/lib.rs`, sort by directory name, exclude named/workspace rows, then
take indexes `0, 101, 202, ...` from the 1614 eligible directories. The table
below is populated verbatim from the `--nocapture` test output.

| purl | kind | features | edition | layout qualification | entities | types | computed | occurrences | oracle | docs | ms | verdict |
|---|---|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---|
| cargo:serde@1.0.229 | Registry | default | 2021 | build.rs | 1 | 1 | 0 | 0 | 0 | 0 | 17631 | PASS |
| cargo:serde@1.0.229 | Registry | derive | 2021 | build.rs | 3 | 3 | 0 | 4 | 4 | 2 | 18559 | PASS |
| cargo:thiserror@2.0.20 | Registry | default | 2021 | build.rs | 4 | 4 | 0 | 0 | 0 | 0 | 21680 | PASS |
| cargo:ab_glyph@0.2.32 | Registry | default | 2021 | [lib] section | 13 | 13 | 0 | 14 | 14 | 0 | 19341 | PASS |
| cargo:block2@0.6.2 | Registry | default | 2021 | [lib] section | 18 | 24 | 0 | 15 | 15 | 6 | 22221 | PASS |
| cargo:cookie_store@0.7.0 | Registry | default | 2018 | plain lib | 17 | 20 | 0 | 13 | 13 | 0 | 21173 | PASS |
| cargo:dragonbox_ecma@0.1.12 | Registry | default | 2021 | build.rs | 122 | 130 | 46 | 437 | 437 | 11 | 20971 | PASS |
| cargo:futures-executor@0.3.34 | Registry | default | 2018 | [lib] section | 10 | 10 | 0 | 10 | 10 | 0 | 20264 | PASS |
| cargo:hashbrown@0.14.5 | Registry | default | 2021 | plain lib | 28 | 36 | 2 | 29 | 29 | 24 | 37111 | PASS |
| cargo:jni-sys-macros@0.4.1 | Registry | default | 2021 | plain lib | — | — | — | — | — | — | — | BOUNDED-OUT(lowering-unsupported:no-supported-declaration) |
| cargo:ndk-context@0.1.1 | Registry | default | 2021 | plain lib | 20 | 22 | 7 | 32 | 32 | 45 | 22656 | PASS |
| cargo:ownedbytes@0.9.0 | Registry | default | 2021 | [lib] section | 115 | 143 | 128 | 273 | 273 | 28 | 24298 | PASS |
| cargo:proptest@1.11.0 | Registry | default | 2021 | [lib] section | 18 | 18 | 0 | 6 | 6 | 0 | 21878 | PASS |
| cargo:rayon@1.11.0 | Registry | default | 2021 | [lib] section | 31 | 39 | 0 | 24 | 24 | 7 | 24026 | PASS |
| cargo:semver-parser@0.7.0 | Registry | default | 2015 | plain lib | 4 | 4 | 0 | 0 | 0 | 0 | 23424 | PASS |
| cargo:tantivy@0.25.0 | Registry | default | 2021 | [lib] section | 107 | 112 | 12 | 120 | 118 | 36 | 35198 | PASS |
| cargo:typed-arena@2.0.2 | Registry | default | 2015 | [lib] section | 100 | 139 | 169 | 457 | 456 | 167 | 72771 | PASS |
| cargo:wasmtime-internal-versioned-export-macros@47.0.3 | Registry | default | 2024 | [lib] section | 22 | 23 | 23 | 73 | 73 | 0 | 26637 | PASS |
| cargo:windows_x86_64_gnullvm@0.48.5 | Registry | default | 2018 | plain lib | — | — | — | — | — | — | — | BOUNDED-OUT(lowering-unsupported:no-supported-declaration) |
| cargo:compiler-ir-vocabulary@0.1.0 | Workspace | default | 2024 | [lib] section | 81 | 81 | 0 | 81 | 81 | 0 | 40654 | PASS |
| cargo:compiler-ir@0.1.0 | Workspace | default | 2024 | [lib] section | — | — | — | — | — | — | — | BOUNDED-OUT(fragment has 333 entities; shared segment capacity is 256) |

Serde feature-pair verdicts: default emits zero cfg-gated reexports; derive
emits both exact `Serialize` and `Deserialize` reexports.

Thiserror source-truth verdict: PASS on the universal floor (4 entities and 4
declared types). Its facade root has only `pub use thiserror_impl::*`; the glob
produces no Reexport fact. `Error` is re-exported in `src/private.rs`, a Foreign
file, so no root Trait/Reexport assertion is applicable.

Timing reference: the pre-shortcut log recorded 195–252 second walks; this
run records the per-profile-line `ms` values above. Timings are machine-load
sensitive and are not a performance ranking.

Provenance: base commit `8d66ca46`, corpus cache date 2026-09-04; registry
cache state and machine load can change elapsed milliseconds, but the pinned
directory list and typed terminal classifications are asserted by the test.

The test is the raw source of the performance profile; no hand-entered timing
or result is evidence.
