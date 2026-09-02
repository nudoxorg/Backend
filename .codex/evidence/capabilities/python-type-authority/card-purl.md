# Card P — python-purl-lifecycle (digest-frozen specimen)

Registered role: nudox_luna_implementer.
Baseline: commit 2c0b26f86 on branch canonical; shared worktree; you never stage files outside
your owned paths.

## Owned paths (exhaustive)
- compiler/driver/tests/python_purl_lifecycle.rs (new)
- compiler/driver/tests/python_support/mod.rs (new) plus further files ONLY under
  compiler/driver/tests/python_support/

## Forbidden adjacent surface
Everything else. In particular: no production code, no compiler/ir/**, no compiler/languages/**,
no compiler/driver/lower.rs, no other test file, no workspace Cargo.toml. You MAY append to the
`[dev-dependencies]` section of compiler/driver/Cargo.toml EXACTLY these five lines and NOTHING
ELSE in that file: `ureq = { workspace = true }`, `flate2 = { workspace = true }`,
`server-index-build = { path = "../../server/index/build" }`,
`server-index-publish = { path = "../../server/index/publish" }`,
`server-index-core = { path = "../../server/index/core" }` — no feature flags, no version
overrides, no reordering, no edits to existing lines; any other diff in that manifest is a card
violation. The `python_support/` tree holds only helpers this test actually calls: no embedded
fixture archives, no vendored package content, no pre-generated PyPI metadata.

## One public terminal
`cargo test -p compiler-driver --test python_purl_lifecycle` is green on this machine, proving
the full PURL lifecycle: `pypi:six@<pinned>` → locate → fetch → unpack → workspace detection →
Ruff+pyrefly authorities (as the shipping lane already wires them) → compile_ir fragments →
durable publish → reopen → index round-trip → an older generation still validates after a
second publication.

## Required journey (one test or a small family; each step keeps typed operands on failure)
1. Parse the PURL `pypi:six@1.17.0` (pin this exact version) into ecosystem/name/version with a
   typed parse error for malformed input (prove one malformed-PURL rejection).
2. Locate through PyPI's JSON API (`https://pypi.org/pypi/six/1.17.0.json`): pick the sdist URL
   (`*.tar.gz`; when several exist, the one whose filename starts with `six-1.17.0`) and record
   the wheel filename (`*.whl`) without downloading it. Network access
   is restricted to hosts `pypi.org` and `files.pythonhosted.org` (redirects included).
3. Download the sdist with a hard wall-clock timeout (<= 60s) and a hard byte cap (<= 8 MiB).
   Both breach terminals must be EXECUTED in tests against the SAME downloader function the
   happy path uses (different parameters, same code path): the byte-cap terminal via a
   deliberately tiny cap (e.g. 1 KiB) against the real download, and the timeout path via an
   asserted-typed construction (a 1ns deadline) without waiting 60s. Each typed terminal
   carries its observed operand. sha256 of the downloaded bytes is recorded and must equal the digest of the file
   used by every later step (single lineage, no fabricated fixtures).
4. Unpack the downloaded tar.gz into a fresh temp fixture dir (flate2 + manual ustar header
   reading is acceptable; do NOT add a tar or zip crate). Handle only regular files and
   directory entries; anything else is a typed rejection.    Prove one corruption rejection on a mutated copy of the just-downloaded archive bytes (flip
   one byte inside the gzip stream), asserting the typed terminal, not just `is_err()`.
5. Workspace detection on the unpacked tree: locate `six.py` under the sdist's top-level
   directory and classify the distribution module-style. Detect `pyproject.toml` presence and
   classify build-needed only when the file declares a build backend; assert the classification
   matches the observed tree (do not run any package build in this card).
6. Compile the EXACT extracted `six.py` bytes through the shipping python semantic lane:
   `compiler_driver::compile_ir` with `LanguageProfile::Python(PythonVersion::Python314)`,
   `Stage::LowerIr`, `SemanticAuthorityInput::None`, and the toolchain variant
   `prepare()` accepts for the Direct python route (read compiler/driver/types/compile.rs and
   mirror compiler/application/tests/local_compiler/support.rs). The lineage assertion is
   mandatory: the sha256 of the compiled source equals the sha256 of the extracted `six.py`
   bytes from the downloaded archive. Decode the returned `CompiledIr` lanes through the
   shipping decoded-fact types (the `compiler_ir` API used by
   compiler/driver/tests/rust_semantic_lane.rs: entity rows, `DecodedOccurrence`,
   `DecodedDocFact`, `compiler_ir::ForeignKey`/`ForeignOrigin`, `OccurrenceConfidence`) and
   assert REAL six facts: the `PY2`/`PY3` constants, `MovesMetaclass`/`add_metaclass`/`u`/`b`
   callables exist, at least one import occurrence carrying a `pypi` package foreign key
   (ecosystem cell `pypi`), and docstring facts present.
7. Publish the compiled fragment durably: mirror
   server/index/publish/tests/compiler_snapshot/journey.rs and
   compiler/application/compiler.rs (`publish_compiled` + `DurablePublisher`), then
   `open_published` to reopen and validate the complete immutable closure.
8. Build the index from the opened compilation via `server_index_build::build` and
   `server_index_publish` encode/seal, mirroring the same journey file.
9. Second publication: compile a modified copy of the extracted six.py (append exactly one new
   function named `six_lifecycle_probe`), publish as the next generation, reopen: the journal
   selects the newest generation, `six_lifecycle_probe` is present, and generation 1's STORED
   FRAGMENT BYTES re-read from the reopened immutable store (never a retained in-memory copy)
   still validate and their sha256 equals the fragment bytes originally written for
   generation 1 (byte-identity of the immutable closure; the old generation keeps
   validating and cannot contain the new function).

## Proof-matrix rows this card owns
R2 (full PURL lifecycle with old-generation validation) — weakened implementations: vendored
fixture instead of real fetch; skipping reopen; skipping index; asserting "no error" instead of
named six facts. Every weakening above has one falsifier test in the file.

## Environment gates
Network and pypi.org are confirmed live on this machine. If a fetch fails transiently, retry
once; then fail the test with the typed terminal (do not silently skip). Tests must not create
files outside the system temp dir and the owned test paths, and must clean up their fixtures.

## Bounds
- No new crate. No unsafe. No async. No `unwrap`/`expect`/`panic` in test code (the workspace
  denies them); use typed test errors like the existing journey files.
- Download cap 8 MiB, timeout 60s, one retry. No parallel downloads.
- Test file(s) must be rustfmt-clean (`cargo fmt` check passes) and clippy-clean for the
  compiler-driver dev target.

## Exact commands
- cargo test -p compiler-driver --test python_purl_lifecycle
- cargo fmt -p compiler-driver -- --check
- cargo clippy -p compiler-driver --tests 2>&1 | tail -5

## Commit protocol
Commit ONLY your owned paths + the authorized Cargo.toml dev-dep lines in one commit:
`test(python): PURL lifecycle from PyPI fetch through publish, reopen, and index round-trip`.
Return: commit hash, exact test counts, the six facts asserted, and the smallest remaining red
row you observed.

## Plan closure
If a step is impossible because a shipping API lacks the needed public function, STOP, write the
typed observation into the return, and leave that one test `#[ignore]`-free but failing — never
fake the step with a vendored or weakened variant.
