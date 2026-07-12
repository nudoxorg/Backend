# Backend: Buck2 → Cargo migration brief (shared contract)

You are one of several subagents migrating the Backend off Buck2 and onto a
Cargo workspace. **Read this whole file before editing.** All subagents share
this contract; diverging from it breaks other subagents' work.

## The end state

- Everything EXCEPT the `compiler` (and its Go/Java oracles) becomes a member of
  the Cargo workspace rooted at `Backend/Cargo.toml` (already created).
- The `compiler` stays on **Buck2** and becomes a **containerized HTTP daemon**.
  **Nothing in the Cargo workspace depends on the compiler.** `server` talks to
  the compiler daemon over HTTP using the shared `protocol` crate.
- Some crates are **dual-built**: they keep their existing `BUCK` file *and* get
  a `Cargo.toml`, because the Buck2 compiler build still compiles them from the
  same `.rs` source. Dual-built: `heart`, `ir`, `caching`, `cas`, `version`,
  `protocol`. Buck2-only: `sandbox`, `compiler` + oracles. Cargo-only:
  `telemetry`, `runtime`, `server` (which absorbs `registry`).

## How to write a Cargo.toml (deterministic — do not guess deps)

Every `crate("alias")` edge in a crate's `BUCK` file has been pre-resolved to a
Cargo dependency spec. Use these artifacts (paths relative to `Backend/`):

- `build/cargo-migration/resolved/<crate>.json` — your crate's resolved deps:
  `third_party` (alias → {package, version, features, source}) and
  `workspace_members` (first-party path deps).
- `build/cargo-migration/dep-map.json` — the full alias → cargo spec map, if you
  need to resolve something the per-crate file missed.

Rules:
- **Package + version** come straight from the resolved file. Use caret
  requirements at the given version (e.g. `serde = "1.0.210"` → `"1"` is fine,
  but prefer the exact minor: `"1.0"`). Match the version family in the map.
- **`package` rename**: if the alias's `package` differs from the Cargo key you
  want, use `foo = { package = "real-name", version = "..." }`. The Cargo dep
  KEY must be the crate's real name so `use foo::...` in the source keeps working
  — i.e. key by the crate's Rust import name. When in doubt, key by the
  `package` field with hyphens (Cargo maps `-`↔`_` for import names).
- **default-features**: Buck2 vendored each crate with an explicit feature set
  (`features_default_on` in dep-map). To stay faithful, set
  `default-features = false` and pass the union of (that base set ∪ the features
  the BUCK edge requested) ONLY IF the source relies on non-default features.
  Simplest safe default: keep `default-features = true` and add the extra
  features the BUCK edge listed. Prefer the simple path unless a crate is known
  to need `default-features = false` (e.g. `sqlx`, `reqwest` with rustls).
- **Version-specific aliases**: `futures-0_3` → `futures = "0.3"`;
  `http-1` → `http = "1"`. The resolved file already carries the right version.
- **git deps** (rare on the Cargo side; only `arborium_tree_sitter` etc. if any):
  `foo = { git = "<git>", rev = "<rev>", package = "<package>" }` from the map.
- **First-party (`workspace_members`)**: path deps to sibling crates, e.g.
  `heart = { path = "../heart" }`. Compute the relative path from your crate dir.
  Note `caching` lives at `workspace/util/caching`, `sandbox` at
  `workspace/util/sandbox`, `ir` now at `workspace/ir`.
- Use `edition.workspace = true`, `version.workspace = true`,
  `license.workspace = true` and `[package] name = "<crate>"`.
- Put dev-only deps (the `rust_tests` block's extra crates) under
  `[dev-dependencies]`.

## Dual-build crates

Keep the existing `BUCK` file untouched (the compiler still needs it). Just ADD
`Cargo.toml`. Cargo ignores `BUCK`; Buck2 ignores `Cargo.toml`. The `.rs`
sources are shared verbatim — do not fork them.

## The compiler-daemon wire contract (the `protocol` crate)

`protocol` is dual-built and depended on by both `server` (Cargo) and the
Buck2 `compiler`. It owns the HTTP boundary types. Definitive shape:

```rust
// protocol: deps = ir, heart, serde, postcard, bytes, smol_str, thiserror
pub struct FileBytes { pub path: String, pub bytes: Vec<u8> }

pub struct CompileRequest {
    pub coordinates: heart::package::Coordinates, // moved from registry::package
    pub toolchain: heart::Toolchain,
    pub files: Vec<FileBytes>,                     // shipped (daemon is remote)
}

pub enum CompileResponse {
    Ok {
        surface: Vec<u8>,          // serde_json of ir::entry::Index
        references: Vec<WireFile>, // per-file resolved references (see below)
        identifiers: Vec<String>,  // public symbol names for search facets
    },
    Err { kind: String, message: String },
}

// WireReference / WireFile: the postcard-serializable mirror of
// ir::syntax::ResolvedReference, LIFTED verbatim from registry/blob/mod.rs
// (the existing `WireReference`/`WireFile` codec + reference-kind mapping).
pub struct WireFile { pub path: String, pub references: Vec<WireReference> }
pub struct WireReference { /* ...as in blob/mod.rs... */ }
```

- `Coordinates` (a.k.a. `PackageCoordinates`) moves from `registry::package` into
  `heart::package`; `heart::identity::PackageCoordinates` re-exports it.
- Server decodes `surface` into `ir::entry::Index`, turns `references` into its
  own `blob::ReferenceSet` via the existing decode path, and stores them.

## LESSONS FROM WAVES 1-2 (read — these bit us)

1. **Flat layout needs `[lib] path`.** Every crate keeps its `.rs` files flat in
   the crate dir (NO `src/`). Cargo will NOT find `lib.rs` automatically — you
   MUST add:
   ```toml
   [lib]
   path = "lib.rs"
   ```
   Without it the crate silently has no library target and dependents fail with
   "unresolved module or unlinked crate".
2. **Use the nightly toolchain.** The code uses nightly features
   (`#![feature(...)]`). Stable `cargo` fails with E0554. Verify with:
   ```
   ~/.rustup/toolchains/nightly-aarch64-apple-darwin/bin/cargo build -p <crate>
   ```
3. **serde needs `derive`.** `serde = { version = "1.0", features = ["derive"] }`.
4. **Propagate `serde` (and friends) onto deps whose types you (de)serialize.**
   Buck vendored crates with a feature set recorded as `features_default_on` in
   dep-map.json. In particular these commonly need `features = ["serde"]`:
   `smol_str`, `compact_str`, `chrono`, `url`, `uuid` (add to v4/v5),
   `ordered-float`, `nonempty` (`serialize`). `tower` needs `util` for
   `ServiceExt::oneshot`. When a build error says "trait Serialize/Deserialize
   is not implemented for X" where X is from another crate, enable that crate's
   `serde` feature. Consult `features_default_on` in dep-map.json but add only
   the semantically-needed features (the vendored set contains unification noise
   like chrono's `wasm-bindgen`/`winapi`).
5. After authoring, build YOUR crate: `<nightly cargo> build -p <crate>`. If it
   fails only because a sibling member's manifest is momentarily incomplete,
   that's transient — make sure YOUR crate is correct and report.

## Constraints / hygiene

- Rust edition 2024. Do NOT edit `.rs` logic beyond what the migration needs
  (import paths, dep boundaries). No behavioral changes.
- Do not touch other crates' directories unless your task explicitly says so.
- After writing a Cargo.toml, sanity-check with
  `cargo metadata --no-deps --format-version 1 --manifest-path <your Cargo.toml>`
  if the tool is available. Network fetches (crates.io) may be BLOCKED in this
  sandbox — if `cargo` can't fetch, that is expected; report it and move on.
  Do not delete a manifest just because it can't build offline.
- Report back: what you created, any alias you couldn't resolve, and any
  `.rs` edits you made.
