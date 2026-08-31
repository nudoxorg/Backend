# P5 C0-IR capability card

## Terminal

The only active terminal is the inward IR vocabulary proof: `EntityId` and `TypeId` remain distinct
four-byte, four-alignment `DenseId` coordinates with private phantom brands, and an ordinary public
consumer proves the compiler rejects `EntityId` where `TypeId` is required. This card neither changes
compiler dispatch nor creates a fragment, frontend, registry, release executable, builder, recipe,
driver, scheduler, sandbox, bundle, publication, or C1 surface. C0-COMPILER is a later separately
calibrated capability, not an implied follow-up edit.

This is direct typed-use safety only: the pinned baseline exposes `raw` and a public `new(u32)`, so a
consumer can deliberately rebrand a copied raw coordinate. That existing limitation is a preserved
counterexample, not a claim this zero-production-delta terminal can repair or disprove.

## Frozen baseline and ownership

- Source candidate baseline: `44c22154fd5238e4769562590420371979306050`.
- Rejected compiler churn remains inspectable in commits, while the active compiler paths are exactly
  restored to the source candidate by `ebb81a9f`; they grant no compiler edit authority in this C0-IR
  card.
- Builder-scoped paths, exactly:

```text
crates/nudox-ir-vocab/src/lib.rs (read-only digest-pinned comparison path)
crates/nudox-ir-vocab/tests/coordinates.rs (only writable builder path)
```

`src/lib.rs` must remain byte-identical to its recorded baseline digest; any diff is an immediate stop
even if formatted production LOC remains 34. The test path is the sole implementation write for this
zero-production-delta checkpoint.

- Manager-only artifacts: this card, `P5_C0_CALIBRATION_RAW.md`, and `evidence/p5-c0/ir/**`.
- Digest custody: after this card's pre-calibration commit, the manager records the SHA-256 of the
  complete card in `P5_C0_CALIBRATION_RAW.md`; every calibration prompt supplies that digest and every
  return names it. A different digest is a stale deck.

No compiler path, manifest, lockfile, workspace root, dependency, unsafe code, allocation policy,
shared crate, central identity registry, conversion API, or unlisted path is writable. Every
calibration role is spawned with its stated model and `fork_turns: none`.

| Baseline path | SHA-256 | formatted LOC |
| --- | --- | ---: |
| `crates/nudox-ir-vocab/src/lib.rs` | `2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78` | 34 |
| `crates/nudox-ir-vocab/tests/coordinates.rs` | `7ca6d45b5fbeb9b0961d5726eea0249e574eb3f76d6b3d9f37fdc17e2aefdc43` | 14 |

## Preserved representation and negative space

`DenseId<Owner>::new(u32)` is the existing local dense-coordinate construction. `EntityId` and `TypeId`
retain private brands, while the baseline raw-rebrand limitation above remains explicitly out of scope.
New `From`/`TryFrom`, raw wire-decode/rebrand APIs, public traits/getters or tuple fields, a phase bit,
later-phase values, `dyn`, `Box`, `Vec`, `Arc`, `Rc`, `String`, serde, async, unsafe, SIMD, macros,
allocation, fallback/default state, or runtime type tags are forbidden.

| Law | artifact and falsifier | hard stop |
| --- | --- | --- |
| Exact layout | `coordinates.rs` asserts literal `4` for both `size_of` and `align_of` of `DenseId<Entity>`, `EntityId`, and `TypeId` | any non-four-byte/four-alignment coordinate |
| Static kind impossibility | one dependency-free current-toolchain compiler-process fixture imports the actual public `nudox_ir_vocab` artifact and contains exactly one `EntityId`-where-`TypeId` forbidden expression; it counts literal `error[E` markers exactly once, asserts `E0308`, `EntityId`, and `TypeId`, and changes only `EntityId::new(7)` to `TypeId::new(7)` in otherwise identical source for a legal mutant that compiles and makes the expected-diagnostic predicate fail | local/shadow types, extra fixture error, doctest-only/runtime-only proof, noncausal mutant, or missing retained stderr |
| Boundary | the fixture is an ordinary `tests/coordinates.rs` public consumer; no production conversion or compiler dependency is introduced | compiler path/API edit, conversion, manifest, or unlisted path |

The test must use no `panic!`, `unwrap`, `expect`, `unreachable!`, source-dropping conversion, or
discarded cleanup. A compiler invocation failure remains causal rather than being converted to green
success. The fixture may use only the standard library already available to the test target; it adds
no crate or test-only workspace member.

## Numeric budgets and skeleton

| resource | C0-IR limit | reserve / accounting |
| --- | ---: | --- |
| net production-source delta | 0 formatted LOC | no production representation change authorized |
| net test delta | 79 formatted LOC forecast | 17 literal unused lines below 110 hard cap |
| dependencies / unsafe / production allocations | 0 / 0 / 0 | any is a stop trigger |
| dense-ID layout | 4 bytes / 4 alignment | measured on host only |

The normally formatted skeleton is the unchanged 34-line vocabulary module and the 93-line public
consumer at `evidence/p5-c0/ir/skeleton/coordinates.rs`
(`e214f0c821d2e209ae775cd153fa30422170b6654ede13b92958f68c6df04a0c`), which compiles as the actual
test target before edit authority and becomes `coordinates.rs` verbatim at the builder checkpoint
(79 added lines over baseline). It uses standard-library stdin to avoid temporary fixture files,
requires exactly one resolved public rlib before passing it through `--extern`, captures stderr, and
uses `--emit=metadata=-` so the legal mutant leaves no output to clean up. Its predicate counts all
coded compiler errors, then requires the sole error to be `E0308` with source-visible `EntityId` and
`TypeId`; the legal `TypeId` mutation must itself compile successfully before its false diagnostic
predicate is accepted. Its local `Option` records only zero-or-one artifact discovery and fails
immediately on a second candidate; it is not a public or semantic terminal. The manager pins every
Cargo gate to the root `target`, while the fixture resolves the parent of the running test executable;
those are the same dependency directory under the advertised gates. The two Luna readers and the
plausible-misreader receive only this card and the governing skills. The Terra reviewer calibration
additionally receives this exact frozen skeleton artifact, at the path and digest above, so it can
return a location-bearing literal tripwire table without receiving a prior report or builder rationale.
It derives the dependency directory from the current test executable's parent, so the compiler-process
fixture follows Cargo's active target directory rather than assuming the workspace default.

Stop and re-card if the test forecast grows by more than 20% or 25 lines above its current 79-line net
delta, a public item appears, or the 17-line reserve would be consumed.

## Commands and evidence terminal

Run every command below from `workspace2` with the literal target directory shown. Before the focused
test, the manager must prepare one clean vocabulary artifact and retain the output:

```text
CARGO_TARGET_DIR=target RUSTC_WRAPPER= cargo clean --manifest-path Cargo.toml -p nudox-ir-vocab
CARGO_TARGET_DIR=target RUSTC_WRAPPER= cargo test --locked --manifest-path Cargo.toml --workspace --all-targets
test "$(find target/debug/deps -maxdepth 1 -type f -name 'libnudox_ir_vocab-*.rlib' -print | wc -l | tr -d ' ')" = 1
find target/debug/deps -maxdepth 1 -type f -name 'libnudox_ir_vocab-*.rlib' -exec shasum -a 256 crates/nudox-ir-vocab/src/lib.rs {} \;
```

The clean preparation must report exactly one candidate. Zero or multiple candidates are causal
failure; the builder fixture independently repeats that exact-one check before it invokes `rustc` from
the current test executable's same `CARGO_TARGET_DIR` dependency parent. The manager compares the
source digest to the pinned card digest before accepting the rlib digest/path, records both literal
fixture inputs, and retains toolchain and compiler-process command. These bind raw stderr to the
freshly built exported artifact rather than an older profile or shadow source.

```text
CARGO_TARGET_DIR=target RUSTC_WRAPPER= cargo fmt --manifest-path Cargo.toml --all -- --check
CARGO_TARGET_DIR=target RUSTC_WRAPPER= cargo test --locked --manifest-path Cargo.toml --workspace --all-targets
CARGO_TARGET_DIR=target RUSTC_WRAPPER= cargo clippy --locked --manifest-path Cargo.toml --workspace --all-targets -- -D warnings
git diff --check && git status --short
```

The manager independently retains the exact toolchain, compiler-process command, original and legal
mutant fixture inputs, source digest, resolved public rlib path and SHA-256, and raw stderr at
`evidence/p5-c0/ir/static-fail-kind.txt`; it records the legal-mutant command/result in
`evidence/p5-c0/ir/commands.txt`. The test is terminal only when it proves the actual exported crate
rejects the exact kind mismatch; a local recreation, a second deliberate error, a changed matcher, or
a rustdoc-only result is inadmissible.

## Candidate and stop decision

The sole candidate is the existing private-brand representation; no stronger representation is under
test. This card authorizes one exact-path IR builder checkpoint, hostile review, and one
falsifier-bound repair only. Stop for a permanent wire choice, dependency/unsafe/SIMD approval, or a
second materially different observable terminal. Otherwise close only C0-IR with evidence and one
prototype verdict; do not begin C0-COMPILER or C1.

## Calibration topology

Before an IR builder or repair worker, two independent Luna cold readers, one Luna plausible-misreader,
and one separate Terra reviewer calibration receive the frozen custody tuple plus governing Rust skills.
The two Luna readers and plausible-misreader receive only this full card: no source, skeleton, candidate,
earlier report, or intended design. The Terra calibration additionally receives the exact skeleton
artifact named above and no source, history, prior report, or builder rationale. All are read-only and
non-inheriting: the Luna roles use `gpt-5.6-luna`, and the reviewer uses `gpt-5.6-terra`. Any semantic
card change or blocker/major ambiguity invalidates the deck and restarts all four roles. The Terra
calibration returns the literal `review-rust-gem` tripwire table over the supplied skeleton and marks
the deliberately unopened source-pinned comparison path unverified, explicitly distinguishing the
known baseline raw-rebrand limitation from any new raw conversion, and rejecting shadow fixtures,
noncausal doctests, panic terminals, unlisted compiler edits, and reserve breach. Calibration grants
no implementation acceptance.

## Exact next decision

Run the complete fresh four-role calibration deck against this exact card digest. No builder or repair
worker receives edit authority until the two Luna readers, Luna plausible-misreader, and separate Terra
reviewer calibration agree on this 93-line skeleton, 110-line hard cap, 17-line unused reserve, the
known raw-rebrand limitation, and the source-pinned, one-test-file write boundary.
