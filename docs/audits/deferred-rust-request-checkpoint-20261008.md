# Deferred Rust request authority checkpoint — 2026-10-08

Rust toolchain selection now belongs to the admitted Rust request rather than
workspace startup. A TypeScript, Python, or Go workspace therefore does not need
an inferred Cargo home or a Rust compiler simply to start its local owner.
Rust requests still bind the actual compiler, Cargo resolution, sysroot,
feature selection, workspace inputs, and final lockfile observation before
publishing a compilation recipe. Portable recipe identity remains separate
from host-local input identity.

The implementation keeps the existing compiler and Rust IR. Successful
toolchain probes can be reused; cancellation and transient failures do not
become permanent cached successes or poison subsequent attempts. Existing
Cargo homes remain subject to no-follow, ownership, and mode admission. The
change does not create or rewrite a previously admitted Cargo home.

## Reviewed source

The canonical base is `b07e51d809f018166eb144ebc8d5ae23a499b1f1`.
The reviewed replay before this documentation commit is
`9880ee42090d69828ef99c7858e96254873c6999`, tree
`d7acdd310c16bfd959990b4ebf801864db6ce485`.

Root independently compared every tracked tree entry: 15 files change and
17,920 unrelated entries remain exact. Cargo.lock and the prior virtual Cargo
workspace repair remain unchanged. Fourteen changed files match the reviewed
native predecessor chain byte for byte. The remaining compiler file retains
canonical's request-specific TypeScript selection and its existing regression
test instead of replacing them with the older donor's global selection.

## Actual validation and its limits

The source-bound native predecessor receipts contain 65 unique passing unit
tests and four distinct passing native runtime controls. The native Rust
cross-file control resolves the exact function owner, verifies the compiler
call edge and source span, and repeats the read after a cold owner restart.
These are real native results, not installed CLI/MCP acceptance.

Root independently checked all 118 entries in the raw evidence archive,
including the hashes of all 22 gate receipts and the explicit graph-return
record. The archive is 19,066,880 bytes with SHA-256
`a3b4a9228b337859c318bbca9bc897cb304531671acf636c5e4e571eac15fe57`.
The accompanying JSON records each gate's actual commit, artifact, exit, and
test summary; build-only gates are not counted as tests.

Two runtime controls remain open. Recovery after an oversized Rust source
reaches the existing unsupported `#[doc = concat!(...)]` preload path; a
separate compiler repair is being reviewed. The service add control stopped
before indexing because its test selected a Rust 2021 health profile while
the product advertises Rust 2024. This checkpoint corrects that selector but
has not rerun the service control. The existing dependency-installed
TypeScript SDK precedence test also remains ignored for its missing real
prerequisites. The strict host gate is consequently not described as green.

The exact canonical replay has not been rebuilt. Its native evidence belongs
to the predecessor commits named in the JSON, not to this new commit.
The current `c0016d4f` Linux release predates this Rust slice; its binaries do
not prove this change works through installed CLI, MCP, or Tantivy surfaces.

## Remaining acceptance work

Run the corrected service add control, repair and rerun literal doc-concat
recovery, and exercise installed CLI and both MCP transports on real Rust
applications. Preserve exact and Tantivy search, graph traversal, shape,
retry, cancellation, offline use, and cold restart checks. Keep TypeScript,
Python, and Go controls active while composing the release so request-local
Rust setup cannot regress their startup or provider selection.
