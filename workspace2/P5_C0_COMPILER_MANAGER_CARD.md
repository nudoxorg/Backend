# P5 C0-COMPILER capability card

## Terminal

The only active capability is a manual closed compiler-dispatch control.  It has two distinct concrete
frontend rows: Rust accepts `Parse` and `LowerIr`; TypeScript accepts `Parse` and rejects `LowerIr` with
the exact typed `FrontendError::UnsupportedStage { language: Language::TypeScriptSubset, stage:
Stage::LowerIr }`.  `drive` is the only stage decision and the full registry makes one closed language
match into `drive<ConcreteFrontend>`; there is no bool/index capability matrix or second runtime table
lookup.

The concrete rows are bound through private closed capability-row types.  A consumed public
`TypeScriptSubset` value exposes `parse` and has no `lower` member; Rust's two supported stages remain
exercised through the full registry.  A dependency-free compiler-process fixture must compile against the actual
exported registry rlib and prove that `TypeScriptSubset.lower(&[])` has exactly one coded compiler
error total, whose sole code is `E0599` and whose stderr names both `TypeScriptSubset` and `lower`;
changing only that call to `TypeScriptSubset.parse(&[])`
must compile.  This is a C0 compiler vocabulary/control experiment only: it creates no fragment,
builder, real SDK frontend, recipe, driver, scheduler, sandbox, bundle, publication, C1 surface,
dispatch/capability macro, proc-macro, tagless encoding, or GADT.  The forwarding bodies are intentional
synthetic C0 controls, not a claim that either language has real frontend semantics.  The observed
capability distinction is exactly TypeScript `LowerIr` absence in the static subset and its exact typed
rejection in the full registry.

## Frozen custody and writable boundary

- Source candidate: `fac5b709d4596f889735a773a6dbd6a9c7c822f5` on
  `codex/prototype-real-compiler-ir`, clean before this card.
- Predecessor C0-IR is closed at `461f802faa660b419fede3bdbd86b48c09fcfac2` and independently
  reproduced at the source candidate.  Its card, evidence, source, and tests are read-only.
- Manager artifacts: this card, `P5_C0_COMPILER_CALIBRATION_RAW.md`, and
  `evidence/p5-c0/compiler/**`.
- Builder-owned paths, and no others:

```text
crates/nudox-compile-registry/src/lib.rs
crates/nudox-compile-registry/tests/dispatch.rs
crates/nudox-compile-registry/tests/subset.rs
crates/nudox-compile-registry/examples/release_consumer.rs
```

- Existing compiler manifests and lockfile are read-only.  The root workspace is already the
  only justified manifest boundary; no workspace or dependency change is authorized.

| frozen path | SHA-256 | formatted LOC |
| --- | --- | ---: |
| `crates/nudox-compile-registry/src/lib.rs` | `965eaabadaf3178cce0103fe4f9d8bc03ce55468ba0bf9ad3572282b33d47cb4` | 67 |
| `crates/nudox-compile-registry/tests/dispatch.rs` | `2835cffd057b88cd1c0914e77fd5660f6106e3ce3f9bbbd44bfd95e4d56f0df4` | 30 |
| `crates/nudox-compile-registry/tests/subset.rs` | `4bb540991a5189fa65a932cbacf12379eb6060a72f6e0e624d29006d177af843` | 8 |
| `crates/nudox-compile-registry/examples/release_consumer.rs` | absent | 0 |

## Laws, falsifiers, and negative space

| law | public evidence and falsifier | hard stop |
| --- | --- | --- |
| three valid cells forward real input | ordinary `tests/dispatch.rs` separately checks pointer and length identity for Rust Parse, Rust LowerIr, and TypeScript Parse; replacing any concrete body with a constant or removing its input forwarding makes that named check fail. | shared input, pointer-only, length-only, success-only, or unexercised valid cell |
| exactly typed unsupported cell | the same public journey compares the full `FrontendError::UnsupportedStage` value with the literal TypeScript/LowerIr operands | generic/erased error, wrong operand, or a panic/fallback |
| type-bound rows; one decision | skeleton `lib.rs` has private `RustCapabilities` and `TypeScriptCapabilities`, one `Frontend::Capabilities` association, and only `drive` matches `Stage`.  The distinct-row falsifier is the full-registry TypeScript LowerIr exact-error test plus the public TypeScriptSubset LowerIr absence test: Rust LowerIr must succeed and forward, while TypeScript LowerIr must reject dynamically and be structurally absent in the subset.  A shared identity-forwarding helper is permitted only if those proven differences remain and it deletes rather than adds representation.  The literal scan permits ordinary test syntax but rejects dispatch/capability macros, bool arrays, indexes, duplicate stage matches, runtime matrix lookups, and added selection tags. | row metadata, erased Rust/TypeScript lower distinction, duplicate stage decision, dispatch/proc macro/tagless/GADT |
| structural TypeScript subset | normal compiler-process test links exactly one fresh registry rlib with `--extern`; its rustc stdout is explicitly `Stdio::null()` so `--emit=metadata=-` cannot leak binary metadata to the test terminal; one forbidden `TypeScriptSubset.lower` expression produces one primary `error[...]` header, whose sole code is `E0599`, contains both required symbols, and allows only rustc's literal one-previous-error summary as an uncoded `error:` line; then the `parse`-only legal mutant compiles and makes the diagnostic predicate false | shadow types, doctest-only proof, a test-only crate, an extra coded or uncoded primary error, stale/multiple rlib, noncausal mutant, or compiler-process binary stdout |
| release consumer/codegen | the named `release_consumer` example calls the full registry separately for each valid cell and verifies pointer plus length without panic.  Candidate and baseline are compiled from the byte-identical consumer source, with the same crate name, three callable names, inputs, target, release flags, and toolchain; raw LLVM/assembly is retained per callable and the concrete `FullRegistry::dispatch`/`drive` body is inspected.  A predeclared no-regression row requires each candidate callable and those two manual-control bodies to introduce no new indirect call, tag test, or panic path relative to the same-source baseline row.  A residual direct call is recorded as baseline-equivalent cost and forbids erasure/zero-cost language. | rlib metadata total, broad binary total, changed behavior comparison, missing cell, input optimized away, uninspected path, or unavailable per-callable text evidence |

Forbidden: any path outside the list above; any dependency, manifest/lockfile change, unsafe, `dyn`,
`Box`, `Vec`, `Arc`, `Rc`, `String`, serde, async, framework, process JSON, testkit crate,
dispatch/capability code-generating macro or proc-macro, panic consumer, source SDK,
fragment/builder/recipe/driver/scheduler/sandbox/bundle/publication/C1 surface, or outward dependency
from IR/compiler vocabulary.  `Language` and `Stage` remain the existing application-boundary closed
enums; no new production runtime selection tag is introduced.  Ordinary `#[test]`, `assert!`, and
`assert_eq!` test syntax is not a dispatch abstraction.  The private example-only `ConsumerError` is a
typed result diagnostic, not a language/stage/capability selection tag.

## Skeleton, forecast, and resource ledger

The complete normally formatted candidate skeleton is committed under
`evidence/p5-c0/compiler/skeleton/`.  It is the only source skeleton provided to the Terra calibration
reviewer.  It contains no hidden build fixture, generated code, or test-only crate.

| frozen skeleton artifact | SHA-256 | formatted LOC |
| --- | --- | ---: |
| `skeleton/lib.rs` | `bbe0152ce639ee7f92a9b72e26dd6b040133a28f3700f04aee302abd1c4fe867` | 80 |
| `skeleton/dispatch.rs` | `588ee2b4c5ec3ad7847970ff316640a7ca05343ab66e75aea890b8880acdd9ab` | 48 |
| `skeleton/subset.rs` | `05bc6f455335106bac3812f4cde299d97890592fddd1c07a6ec1043213c77b8a` | 111 |
| `skeleton/release_consumer.rs` | `b86ecd9c1c15f855e8523883250b89ab96f042708b54e4ccd21f01edfd4a76c8` | 70 |
| `seeded/constant-typescript-lower.patch` | `c1592276e49d23c6a3cab572c2b7fcf96cb0ea64a3347a01b1348659838152af` | 13 |
| `seeded/rust-parse-constant.patch` | `c2612584edbc6a5a42f23823b8cffa2fed8ea9c2f531eb58b61d6eeeb8deb37b` | 6 |
| `seeded/rust-lower-constant.patch` | `bd809ed6a5dad78650615a04611f30f60b3f24b984bd6189a7192b7de5709c7a` | 6 |
| `seeded/typescript-parse-constant.patch` | `927a320e824b5ed63f75b42af2c4b6fc8d7eff2e45a52ee0efcb0527f02b1378` | 6 |
| `seeded/run-valid-cell-mutant.sh` | `4d30f95619e37c64fc5fa9d94fc9175b3a9f1b344220f0396a5e51e2494564d4` | 62 |
| `seeded/run-typescript-lower-mutant.sh` | `bc692fc9e0aca831280ce3eb2cd0b625637665891b52e2978215de03fef89801` | 27 |
| `seeded/run-same-source-control.sh` | `6cc31be5688e9381bc3138babba6da6d06394ab763e6f4bc9fa152218eb12715` | 91 |

Every cap below is a **net added formatted-LOC cap from its frozen baseline**, never a total-file cap.
The skeleton total is baseline plus forecast delta; total-file cap is baseline plus net-delta cap.

| file | items and cases | baseline | forecast net delta | net-delta cap | skeleton total | total-file cap | unused delta capacity |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `src/lib.rs` | two private rows/frontends, two private traits, one `drive`, full registry, consumed TypeScript subset | 67 | 13 | 41 | 80 | 108 | 28 |
| `tests/dispatch.rs` | three named valid pointer+length cases and exact unsupported operands | 30 | 18 | 43 | 48 | 73 | 25 |
| `tests/subset.rs` | actual-rlib compiler process, null-bound metadata stdout, primary/uncoded-error E0599 predicate, legal `parse` mutant | 8 | 103 | 110 | 111 | 118 | 7 |
| `examples/release_consumer.rs` | three nonpanic named callable cells and pointer/length verifier with observable inner frontend error | 0 | 70 | 92 | 70 | 92 | 22 |

The production phase (`lib.rs` plus example) has baseline 67, forecast net delta 83, net-delta cap
133, skeleton total 150, total-file cap 200, and 50 unused lines (28 plus 22).  The test phase has
baseline 38, forecast net delta 121, net-delta cap 153, skeleton total 159, total-file cap 191, and 32
unused lines (25 plus 7).  These aggregate reserves are exactly the per-file unused capacities and are
not extra capacity that can be double-counted.

The measured baseline has zero registry allocations/copies and no production dependencies beyond the
existing inward compile-vocab edge.  Candidate target is zero allocations and copies, borrowed output
identical to each input, one language match plus one `drive` stage match per full request, no matrix
lookup, and no retained owner beyond the caller slice.  The release text comparison is limited to the
same `release_consumer` executable and its three same-named `#[inline(never)]` cell callables under the
same toolchain/profile.  No aggregate `.rlib` or whole-program byte delta is a benefit claim.

Before release comparison, the candidate example must be byte-identical to the frozen consumer.  This
binds the control/candidate comparison without freezing an unearned implementation helper choice:

```text
cmp -s crates/nudox-compile-registry/examples/release_consumer.rs evidence/p5-c0/compiler/skeleton/release_consumer.rs
```

## Commands and raw evidence

Run from `workspace2`.  The manager first removes only the dedicated C0 compiler target's registry and
compile-vocab package outputs, then retains toolchain identity, target path, both source SHA-256 values,
both resolved rlib SHA-256 values, and all command output under `evidence/p5-c0/compiler/`.  An old rlib
never supplies the public compiler-process or direct-release proof:

```text
rustc -Vv
cargo -V
RUSTC_WRAPPER= CARGO_TARGET_DIR=target/c0-compiler \
  cargo clean --manifest-path Cargo.toml -p nudox-compile-registry -p nudox-compile-vocab
test "$(find target/c0-compiler/debug/deps -maxdepth 1 -type f -name 'libnudox_compile_registry-*.rlib' -print 2>/dev/null | wc -l | tr -d ' ')" = 0
test "$(find target/c0-compiler/debug/deps -maxdepth 1 -type f -name 'libnudox_compile_vocab-*.rlib' -print 2>/dev/null | wc -l | tr -d ' ')" = 0
RUSTC_WRAPPER= CARGO_TARGET_DIR=target/c0-compiler \
  cargo test --locked --manifest-path Cargo.toml --workspace --all-targets
test "$(find target/c0-compiler/debug/deps -maxdepth 1 -type f -name 'libnudox_compile_registry-*.rlib' -print | wc -l | tr -d ' ')" = 1
test "$(find target/c0-compiler/debug/deps -maxdepth 1 -type f -name 'libnudox_compile_vocab-*.rlib' -print | wc -l | tr -d ' ')" = 1
shasum -a 256 crates/nudox-compile-registry/src/lib.rs \
  crates/nudox-compile-vocab/src/lib.rs \
  target/c0-compiler/debug/deps/libnudox_compile_registry-*.rlib \
  target/c0-compiler/debug/deps/libnudox_compile_vocab-*.rlib
RUSTC_WRAPPER= CARGO_TARGET_DIR=target/c0-compiler \
  cargo fmt --manifest-path Cargo.toml --all -- --check
RUSTC_WRAPPER= CARGO_TARGET_DIR=target/c0-compiler \
  cargo clippy --locked --manifest-path Cargo.toml --workspace --all-targets -- -D warnings
git diff --check && git status --short
```

The subset test obtains its `debug/deps` directory from the running test executable, resolves exactly
one `libnudox_compile_registry-*.rlib` (the rlib suffix excludes metadata-only artifacts), verifies the
successful zero-after-clean/one-after-rebuild terminal above in that same target directory, passes the
resolved path using `--extern`, pipes compiler stderr for the exact diagnostic predicate, and explicitly
binds rustc stdout to `Stdio::null()`.  Its text parser requires one `error[...]` header with E0599 and
the required symbols, rejects every uncoded `error:` header except exactly `error: aborting due to 1
previous error`, and rejects a second coded header.  Thus `--emit=metadata=-` creates no file and cannot
emit binary metadata to the normal test terminal.  The manager captures both sources/rlibs' SHA-256 values and
`rustc -Vv`/`cargo -V` adjacent to that command; a different path, missing hash, nonzero/multiple
cardinality, or compiler-process binary stdout fails custody.  It retains both forbidden and legal-mutant
source text/commands/stderr; it demonstrates a clean textual compiler test terminal.  The manager runs a
constant-body mutation of every valid capability body reached by the named release callables, plus the
separate rejected-cell error contract mutation.

The deliberately relevant retained control is mechanical: detached `fac5b709` and candidate worktrees
each clean dedicated release targets, prove zero registry **and** compile-vocab rlibs before build and
exactly one fresh rlib of each kind afterward, then hash each path.  The candidate's actual
`examples/release_consumer.rs` must be byte-identical to the committed skeleton before that one source
is directly compiled twice as crate `release_consumer`, once with the control pair and once with the
candidate pair.  `seeded/run-same-source-control.sh <control-worktree> <candidate-worktree> <evidence-dir>`
is the literal required mechanism: both direct `rustc` calls use `--edition 2024`, `-C opt-level=3`, the
same target triple and `-L dependency` directory, plus explicit `--extern nudox_compile_registry=<one
fresh rlib>` and `--extern nudox_compile_vocab=<one fresh rlib>`, and emit LLVM plus assembly under
`control/` and `candidate/`.  It also directly compiles each registry `src/lib.rs` as
`nudox_compile_registry` in clean isolated `control/registry/` and `candidate/registry/` output
directories, with the same edition, target, optimization, toolchain, dependency path, and its own
explicit fresh compile-vocab `--extern`; source, vocab rlib, command, and emitted LLVM/assembly are
hashed and retained.  `release_consumer.rs` names compile-vocab directly; re-exporting it to simplify
this harness, allowing a duplicate/stale artifact, or replacing this direct control with a Cargo-example
artifact is a hard stop.  The consumer call-site family is compared per named `rust_parse`, `rust_lower`,
and `typescript_parse`; the registry owner-body family is compared at concrete `FullRegistry::dispatch`
and any retained monomorphized `drive` definition, never a broad executable or `.rlib` total.  At
opt-level 3, `drive` may be fully inlined; symbol absence is reported and its realized branch/calls are
then inspected inside `dispatch`, not misreported as a standalone body.  The manual control may retain
direct dispatch calls; that baseline-equivalent residual earns no erasure/zero-cost claim.  A new
indirect call, tag test, or panic path blocks codegen comparability.  If this host cannot produce stable
per-symbol text evidence, release text is `UNVERIFIED`, no binary-total substitute is permitted, and the
terminal cannot be promoted on codegen evidence.

The literal dispatch tripwire scans `skeleton/lib.rs` and later the candidate `src/lib.rs`: exactly one
`match stage` in `drive`, exactly one `match language` in `FullRegistry::dispatch`, zero
`CAPABILITY_MATRIX`, `CAPABILITY_ROW`, `[bool;`, `[true`, `[false`, `macro_rules!`, `proc_macro`, and
additional public language/stage/capability enums.  A wrapper/helper moving either match is a stop.

## Calibration and closure

Before edit authority, two independent cold `gpt-5.6-luna` readers and one `gpt-5.6-luna` plausible
misreader receive only this card plus the governing skills.  A separate non-inheriting
`gpt-5.6-terra` reviewer receives this card, the four frozen skeleton artifacts, both seeded executable
runners, and seeded executable defects from rejected commits `916789c6`, `328f890a`, and `788499a5`
(bool/index matrix, shadow subset fixture, broad behavior-mismatched release consumer).  The
digest-pinned `run-typescript-lower-mutant.sh` applies
`evidence/p5-c0/compiler/seeded/constant-typescript-lower.patch` to a temporary frozen `lib.rs`; it
changes only TypeScript LowerIr from its exact `Err` to `Ok(source)`, requires
`typescript_lower_has_exact_typed_operands` to exit red, and retains its output separately.  The
reviewer must name that test as its causal falsifier.
The parameterized `run-valid-cell-mutant.sh` applies the three other digest-pinned patches in a separate
supplied worktree: `rust-parse`, `rust-lower`, and `typescript-parse`.  Each replaces only its valid
source-forwarding body with a constant, requires the matching named pointer-and-length test to exit red
and show its assertion failure, then retains the corresponding release-consumer LLVM artifact.  The
three red named tests establish valid-cell source dependence.  The runner does **not** mechanically
compare the relevant callable IR or prove codegen input-removal: the consumer's intentional pointer and
length verifier still reads its input after a constant mutation.  Its LLVM output is raw inspection
material only; no input-removal codegen claim is allowed without a focused before/mutant callable-IR
comparison.  Those residual verifier uses do not rescue the frontend forwarding claim.

The R6 deck and `evidence/p5-c0/compiler/mutants/R6_EXECUTION.md` are rejected churn: its hostile review
found the frozen consumer's Debug implementation discarded the contained `FrontendError`, which fails
the required warnings-denied gate.  The R7 skeleton instead consumes and presents that error through
`formatter.debug_tuple("Frontend").field(error).finish()`; it has passed frozen-skeleton formatting,
tests, and warnings-denied Clippy in a detached target.  This semantic skeleton/card correction restarts
all four calibration roles.  R7 has re-executed the valid forwarding and rejected-cell seeds at
`9225fa3bd192ea1f0c6a69f1a57a8afe66cdaf36`; `mutants/R7_EXECUTION.md` retains all four raw red outputs,
their `101` status artifacts, and three successful mutant release-codegen logs.  The mutation logs warn
only because each constant-body mutation intentionally drops its `source`, while the unmutated R7 gate
is warnings-clean.  The eventual LLVM evidence remains limited: no input-removal, erasure, or zero-cost
language is allowed without focused before/mutant callable-IR comparison.

The separate hostile pre-edit Terra reviewer, postbuild Terra reviewer, and closure Terra reviewer each
return the literal `review-rust-gem` tripwire table for the object they review. Every table has exactly
the columns `tripwire`, `count`, `exact locations`, `disposition`, and `evidence or finding ID`.
The hostile table reviews the frozen skeleton; the postbuild table reviews the real candidate plus fresh
postbuild artifacts; the closure table reviews the real candidate, its diff, and all current-digest
custody. A missing column, review-object mapping, or table is a blocker. Any semantic card/skeleton edit, blocker, or
major ambiguity changes the digest and restarts all four roles.  A second separate Terra hostile review
must clear the calibrated skeleton before a Luna builder receives one exact-path checkpoint.  Separate
Terra postbuild and closure reviews follow; Luna repairs may address only accepted falsifiers.  Finish
with two clean full gates.

Pre-edit authority is intentionally over the digest-pinned skeleton and its detached executable replay,
not over an already-applied production candidate.  Until the hostile pre-edit review clears, the four
authorized destination paths remain at their prior accepted state; a difference between a newly repaired frozen
skeleton and the still-unedited destination path is expected and is not a calibration blocker.  The Luna
checkpoint names exactly one destination path; only that named destination must become byte-identical to
its matching skeleton at that checkpoint, while every other authorized destination path remains unchanged
until its own separately authorized checkpoint.
R9 production/control/codegen evidence is superseded history for this digest: it may explain the control
shape but cannot support R11-or-later production, release-text, or codegen authority, all of which must
be regenerated after the authorized repair.

Accepted/rejected history is ledgered rather than imported: `916789c6` is the bool/index counterexample;
`328f890a` is an unproven type-row and shadow-fixture counterexample; `788499a5` adds a noncomparable
broad consumer and panic/process handling.  Their useful clues are respectively the three-cell matrix,
typed-row direction, and causal external compilation; none grants source reuse or authority.

## Exact next decision

Commit this canonical card and its complete skeleton, record its custody digest, then run the complete
fresh four-role deck.  No production edit, builder, repair, codegen claim, or C1 work is authorized
until that deck and the separate hostile pre-edit review clear the exact frozen digest.
