# Vendored Packages

This directory contains vendored dependencies that require special handling due to missing or large source files.

| Package | Sources Committed | Obtain With | What Breaks If Missing | Build Script Handling |
|---------|-------------------|-------------|----------------------|----------------------|
| `libpijul` | Yes | N/A (fork diff in `libpijul-fork.patch`) | Depth-1 structured record (DEPTH1 plan §2); all `libpijul` requirements resolve to this fork | Normal build |
| `qdrant-edge` | **Yes** (restored 2026-08-07) | `curl -sSLO https://static.crates.io/crates/qdrant-edge/qdrant-edge-0.7.2.crate` — the published crate carries `cpp/`, `src/segment/spaces/metric_f16/cpp/` and `tokenizer/bccwj-suw_c1.0.model` verbatim | Was L7: the SIMD kernels are the *only* definitions of six symbols the Rust FFI declares, so their absence broke the final link of `driver` and 3 of its integration tests. Cargo patch resolves `qdrant-edge 0.7.2` to this vendored fork. | **Hard error** naming the missing file (see below) |
| `doltlite` | No | Generate `doltlite.c` from upstream DoltLite source (~20 MB amalgamation) — see `doltlite/VENDORING.md` | ⚠️ **Does not break the build — it corrupts it silently.** See "The doltlite trap" below. | (No build script; linked via rusqdoltlite) |
| `rusqdoltlite` | Yes | N/A (Rust wrapper only; needs doltlite.c via sibling doltlite/) | DoltLite C bindings for catalog storage backend (behind `index::dolt-engine` feature) | Warns and skips C compilation if `doltlite.c` missing — **the link still succeeds**, wrongly |

## Workspace Exclusion

Both `qdrant-edge` and `rusqdoltlite` (and `doltlite`, which has no Cargo.toml) are **excluded** from the workspace `members` list in the root `Cargo.toml`.
- Exclusion prevents `cargo check --workspace --all-targets` from attempting to build them when their sources are missing.
- `qdrant-edge` remains active as a `[patch.crates-io]` entry: when `registry`'s `local` feature is enabled, the patch resolves and Cargo builds it; when disabled, it is skipped.
- `rusqdoltlite` is an optional `path` dependency of `index` (feature `dolt-engine`): only built if the feature is enabled.

## Self-Contained Manifests

Both excluded packages have explicit, self-contained `Cargo.toml` files with:
- Empty `[workspace]` table to stop Cargo from searching upward for workspace declarations
- Concrete `edition`, `version`, `license` instead of `.workspace = true` inheritance
- Explicit `[lints.rust]` and `[lints.clippy]` sections

This pattern decouples them from the root workspace so they can be built independently or excluded from workspace-wide operations.

## Build Script Behavior

When source files are missing:

- **qdrant-edge**: `build_common::require_vendored_source` **aborts the build**,
  naming the exact absent file, the symbols it is the only definition of, and the
  `curl` line that restores it. Both `build_quantization.rs` and
  `build_segment.rs` call it immediately before handing each file to `cc`, so a
  partially-vendored `cpp/` reports the one file it is short of.

  This used to be a `cargo:warning` and an early `return`, justified as "failing
  the build is too harsh, type-checking the crate is still useful". That
  reasoning does not survive contact with the facts:

  1. `qdrant-edge` is in the root manifest's `exclude` list, so
     `cargo check --workspace` never builds it. The only builds that reach this
     build script are ones that are about to *link* it (`registry/local`), and
     every one of those needs the kernels. Nothing benefits from the skip.
  2. `cargo check` never links, so nothing in the standard gate could observe
     the damage. The crate checked clean for days while `driver` could not
     produce a binary (doctrine §7 records the same trap).
  3. What the skip actually emits is an rlib with dangling references. The
     failure surfaces at the final link of every downstream binary, as six
     mangled symbol names that mention neither this crate nor `cpp/`.

  A build script that can prove its output will not link should say so, at the
  point it knows, in the vocabulary of the thing that is missing.

### The doltlite trap (read before trusting `index`'s `dolt-engine`)

`doltlite.c` is **still absent**, and unlike qdrant-edge its absence does *not*
produce a link error. `rusqdoltlite/sys.rs` declares the ordinary SQLite C API
(`sqlite3_open_v2`, `sqlite3_prepare_v2`, …) — because DoltLite is SQLite,
renamed — and declares it with no `#[link(name = …)]`. `index` separately
hard-depends on `rusqlite` with feature `bundled`, which statically links a
complete stock SQLite into the same binary.

So the externs resolve. The build is green. `index`'s `dolt-engine` feature runs
**plain SQLite**: no prolly-tree pager, no content addressing, no `dolt_*`
functions, no versioning. The only symptom is `dolt_commit` and friends failing
at runtime as "no such function", nowhere near the cause.

The fix is a typed guard, not a louder warning: `build.rs` should emit a cfg when
the amalgamation is absent, and `Connection::open` should refuse to open rather
than hand back a handle to the wrong engine. That changes `index`'s test surface,
which is owned by another workstream, so it is recorded here and in
`rusqdoltlite/build.rs` rather than done here.

### Divergence from upstream

Changes to vendored upstream files in this directory, none yet recorded as a
patch:

- `qdrant-edge/src/lib.rs` + `src/segment/common/anonymize.rs` — the dead generic
  `anonymize_collection_values{,_opt}` helpers are deleted; their
  `for<'a> &'a C: IntoIterator` HRTB overflows the trait solver whenever objc2's
  blanket `impl IntoIterator for &Retained<T>` is in the crate graph (it is, via
  iroh/netdev on macOS). Note that the root `Cargo.toml` still describes this
  divergence as `#![recursion_limit = "256"]` in `lib.rs`; that is not what the
  file contains.
- `qdrant-edge/build_quantization.rs`, `build_segment.rs`, `build_common.rs` —
  the missing-source guard described above.

`libpijul` establishes the convention these should follow
(`workspace/vendor/libpijul-fork.patch`): a vendored fork carries a regenerable
diff so the divergence is visible and can be replayed against a newer upstream.
`qdrant-edge` is now past "one change" and should get a `qdrant-edge-fork.patch`.

### Corrections to earlier notes in this file

This section previously claimed two things that were not true of the working
tree, both of which cost investigation time:

- "`build_segment.rs` was **not** modified." It was — it carried the same
  warn-and-skip guard as `build_quantization.rs`.
- "No stub tokenizer model was created. `qdrant-edge/tokenizer/` does not exist."
  It did exist, and it was a **4-byte file containing the ASCII text `STUB`**
  standing in for an 817,859-byte Vaporetto model that `japanese.rs` pulls in
  with `include_bytes!` and parses with `Model::read_slice(MODEL).unwrap()`.
  That compiles and panics on the first Japanese tokenization. It has been
  replaced with the genuine upstream artifact, verified against the crate's own
  `MODEL_CHECKSUM` (the sha512 in `japanese.rs`) — the file now matches it
  exactly. The model is `.gitignore`d, so it must be re-fetched per checkout.
