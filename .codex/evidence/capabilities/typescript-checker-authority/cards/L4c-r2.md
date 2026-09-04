# Card L4c-r2: lifecycle green on a default test thread — full R7 terminal

registered role: nudox_luna_implementer (expected `luna`/max; resume of the
L4c-r1 session)
baseline: e1b1131a (L6a-r3 landed; the production pipeline now lowers the
real @babel/parser entry on all routes — Terra verified). Uncommitted
working set from L4c-r1 is in the worktree: typescript_purl_lifecycle.rs,
typescript_support/, serde_json dev-dep line, Cargo.lock — that is your
starting point; finish and commit it.

MANDATORY environment for EVERY cargo command:
  export PATH=/opt/homebrew/bin:$PATH
  export NODE_PATH=/Users/mileswirht/Downloads/backend/node_modules
  export CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-typescript/.local/target

## Terra diagnosis of your remaining blocker (verified, not speculation)

Your scoped lifecycle test DIES BY STACK OVERFLOW before lifecycle()'s
first statement executes on a DEFAULT (2 MiB) test thread: the function
frame carries fixed stack arrays — `manifest = [0_u8; 1 << 20]`, `m =
[0_u8; 1 << 20]`, plus scratch — well over 2 MiB total in debug. The
python model (python_purl_lifecycle.rs lines 325-393) uses HEAP vecs
(`vec![0_u8; 1 << 20]`, `vec![0_u8; 128]`) for every large buffer. With
RUST_MIN_STACK=256 MiB the test reaches `compile_source` and now fails
only on the (since-fixed) production bug — the pipeline itself is green.

## Required behavior

1. **Frame law.** Every buffer >= 64 KiB becomes a heap `vec![]`/`Box<[u8]>`
   exactly like the python model. FALSIFIER: run the whole lifecycle body
   inside `std::thread::Builder::new().stack_size(2 * 1024 * 1024)` — it
   must complete there (default test threads are that size; no
   RUST_MIN_STACK env allowed).
2. **Both PURLs travel (R7).** Currently only the scoped package runs.
   lodash@4.17.21 ships NO declaration files — keep it only in the
   URL-convention test and pick a famous single-package that SHIPS .d.ts in
   its tarball (scout: zod@3.x, axios@1.x, ms, Chalk v5 — verify the
   tarball contains the resolved entry file before pinning; pin exact
   versions + digests in the receipt). Both packages run the FULL journey:
   locate -> fetch -> extract -> entry -> compile -> publish -> reopen ->
   index rows -> second chained generation -> old-generation fragment still
   decodes+validates -> golden transcript decode assertion.
3. **Build-artifacts leg (L4c point 5).** Scout ONE npm package whose
   tarball has no .d.ts but has a typescript build script; if after two
   materially different scout attempts none qualifies, keep the two-attempt
   receipt AND prove the build leg with the local no-network fixture
   package (fails without the build, passes with it) exactly as L4c
   point 5 prescribes.
4. **Receipt.** Commit receipts/L4c.md: exact commands, pinned tarball
   sha256s (scoped + single-package + build-leg), resolved entry paths,
   lockfile facts, index row counts, generation numbers. Raw output, no
   "passed" prose.
5. Regression watch: typescript_lower (31), render (14), authority (9),
   package (1), crate (1+17+5), lib check.

owned paths: typescript_purl_lifecycle.rs, typescript_support/mod.rs (+ new
support submodules), compiler/driver/Cargo.toml (the one serde_json
dev-dep line already present; no more), receipts/L4c.md. Everything else
forbidden.

## Checkpoint protocol

One commit, prefix `test(typescript):`. Return: commit sha, focused outputs
verbatim, the two PURLs + build-leg outcome (or two attempts), index row
counts + generation numbers + pinned digests, the 2MiB-thread falsifier
receipt, and the smallest remaining red row.
