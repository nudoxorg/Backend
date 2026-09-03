# Card L4c: full lifecycle from PURL `npm:pkg@ver` (matrix row R7)

registered role: nudox_luna_implementer (expected `luna`/max)
baseline: canonical 58634ef11 (frozen per-file hashes in index.toml [baseline.files])
owned paths (no other file may be edited):
  - compiler/driver/tests/typescript_purl_lifecycle.rs (NEW)
  - compiler/driver/tests/typescript_support/mod.rs    (NEW)
  - compiler/driver/Cargo.toml                         (AT MOST one dev-dependency line:
    serde_json = { workspace = true }, needed for package.json/package-lock.json parsing;
    no other manifest change)
forbidden adjacent surface: everything else. No production code changes. The registry/extract/
build mechanics are TEST SUPPORT with typed errors, caps, and deadlines; the production pipeline
owns compile -> publish -> reopen -> index exactly as the python lane modeled it.

## Public terminal (matrix row R7 verbatim intent)

One famous single-package PURL and one monorepo/scoped-subpackage PURL travel end-to-end:
registry locate/fetch/extract -> (npm build ONLY when the package's types ship as build
artifacts) -> production authority -> fragments -> publish -> journal reopen -> index rows —
plus a second chained generation, and the old schema-1 golden fragment keeps validating beside it.

## Required behavior

Model `compiler/driver/tests/python_purl_lifecycle.rs` + `python_support/mod.rs` mechanically
(publish/reopen/index/generation choreography is proven there — copy the choreography, not the
python specifics). npm specifics for `typescript_support`:

1. `Purl::parse("npm:lodash@4.17.21")` and scoped `"npm:@babel/traverse@<pin>"` — typed rejection
   for malformed/missing version.
2. Registry locate/fetch: exact-version tarball URL convention
   `https://registry.npmjs.org/<name>/-/<tarball-base>-<version>.tgz` (scoped names keep the
   directory form `@scope/name/-/name-version.tgz`). Shared transport with a hard global timeout,
   byte cap, and deadline exactly like python_support's; integrity: the registry tarball's sha512
   from the packument is NOT required — but the downloaded bytes' digest is computed and pinned in
   the committed receipt. A second run of the same test must fetch deterministically (same URL);
   caching into a temp dir across tests inside one run is allowed, network flakiness is a typed
   `Support::Network` error, not a skip.
3. Extract: gzip + tar with npm's `package/` root prefix stripped; path-escape rejection identical
   in spirit to python_support::unpack; entry-type whitelist; bounded total unpacked bytes.
4. Package location laws (the R7 "monorepo/workspace package location, lockfile awareness"):
   - Read the unpacked root `package.json`; resolve the entry per its `types`/`typings` field, else
     `typesVersions`, else the conventional `index.d.ts` beside `main`/`exports` types — every step
     a typed decision, no silent fallback: when no declaration entry is found, that is a typed
     `Support::MissingTypes` terminal carrying the searched paths.
   - Monorepo case: the PURL names a SUBPACKAGE (e.g. `@babel/traverse`); support must locate the
     package directory by its own package.json inside the unpacked tree (handle the tarball's
     actual layout; some monorepo tarballs root the subpackage at `package/`, others nest). If a
     workspace `package-lock.json`/`npm-shrinkwrap.json` is present in the unpacked tree, parse it
     with serde_json and record the resolved dependency versions for the entry's imports in the
     receipt (lockfile awareness = read and use it, never required to exist).
5. npm build leg: when the resolved declaration entry does NOT exist in the tarball but the
   package builds declarations (build/prepare script + tsc), run the build with a deadline, caps,
   typed failure causes, and process cleanup, then re-resolve. The test names >= 0 packages for
   this leg; if the two chosen packages both ship types (likely), pick a third small package that
   demonstrably requires the build (worker scouts; candidates: packages whose tarball has no
   .d.ts but has typescript build scripts). If after TWO materially different attempts no package
   on npm can be found that requires the build leg, report the exact attempts; the leg's code stays
   and its falsifier is the typed `Support::BuildRequired`/build-error path proven by a unit test
   over a locally constructed fixture package (no network) that fails WITHOUT the build and passes
   WITH it.
6. Lifecycle choreography (mirror python): compile the package entry with the production pipeline
   (`compile` with `SemanticAuthorityInput` absent -> checker runs; toolchain selection TypeScript),
   assert fragments, `publish_compiled` into a fresh `DurablePublisher` journal, `reopen`,
   `open_published`, decode the fragment from the opened publication, `seal_compilation_index` ->
   `plan_index_pack` -> `encode_index_pack`, assert index rows (lexical/exact) contain the
   package's exported declaration names.
7. Second chained generation: recompile with a modified entry (append one exported const) and
   publish into the SAME journal; reopen newest; assert generation advanced, the new row exists,
   and the generation-1 fragment (kept beside) still decodes + validates (old-fragment law).
8. The committed schema-1 golden (`typescript_lower.rs` TRANSCRIPT) must still decode through
   `Checker::default().decode` inside one lifecycle test assertion (backwards-compat law).
9. Receipt: the test prints (and you commit under
   `.codex/evidence/capabilities/typescript-checker-authority/receipts/L4c.md`) the exact commands,
   pinned tarball digests, resolved entry paths, lockfile facts, and index row counts. Raw, honest,
   no self-reported "passed" prose.

## Exact focused commands

- cargo test -p compiler-driver --test typescript_purl_lifecycle -- --nocapture
- cargo check -p compiler-driver --tests
- cargo test -p compiler-driver --test typescript_lower  (regression watch)

## Bounds and stop decisions

- Test-support error type mirrors python_support::Error: every cause typed, `#[source]` retained,
  thiserror. No `unwrap`/`expect`/`panic` in support or test bodies (clippy lints are denied in
  this crate's tests; follow python_purl_lifecycle.rs's `#![deny(...)]` header).
- Caps and deadlines are named constants with doc comments; no magic numbers.
- No production file may change to make this pass. If `compile()` genuinely cannot lower a located
  package entry without a production change, report the exact missing public input — that is Terra
  work (likely R10's package-root plumbing), not yours.
- Network/registry unavailability after one retry is `EVIDENCE_BLOCKED: registry unreachable`
  with the raw error; never a skip, never a weakened assertion.

## Checkpoint protocol

Commit owned paths only, message prefix `test(typescript):`. Return: commit sha, focused command
outputs, the two PURLs + third build-leg package (or the two attempts), index row counts, and the
smallest remaining red row.
