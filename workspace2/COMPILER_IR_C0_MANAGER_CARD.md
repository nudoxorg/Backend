# Compiler and IR C0.1 manager card

This is the executable contract for the first C0 child only. This cycle changes documentation and
skills only: no production, manifest, test-fixture, ROADMAP, or legacy-compatibility edit is authorized.
A later builder starts from this card only after fresh cold calibration accepts it.

## Frozen specimen

- Repository and branch: `/private/tmp/nudox-autonomous-compiler-contract`,
  `autonomous-compiler-contract`.
- Baseline: `f9419673f451ec8796d3f9462f3bace667c13435`; formatting and `git diff --check` were clean.
- Toolchain: `workspace2/rust-toolchain.toml`, Rust `1.97.1`, `clippy`, and `rustfmt`.
- Baseline absence: the nine package files listed below and this card are untracked/absent. The root
  workspace and shared lockfile already exist and remain read-only.

| Frozen input | SHA-256 | LOC |
| --- | --- | ---: |
| `COMPILER_IR_GREENFIELD_PLAN.md` | `8eb47464d98c6ea6c74034e5e0b2b70c39cfef9f49ab309a4576282983703a28` | 327 |
| `steward-greenfield-rust-program/SKILL.md` | `839177af0ae7d21d5550c02726c0df0df183825f3c37a97dfba16246b9ed47a9` | 130 |
| `deliver-reviewed-rust-slice/SKILL.md` | `812088beb50ecd1c9dd3973ace8a39692852c39ba4bd404af110351f628d32d3` | 423 |
| `manage-rust-swarm/SKILL.md` | `7ccd529b1dba37a5def845668afa4867aa4b3664a1dfffa44d4454596506f6b3` | 227 |
| `calibrate-rust-agent-contract/SKILL.md` | `fd19aa8c8017b1203b0b648e74b33841b28d6d28ebdab96b2ab82e8c60637d5c` | 98 |
| `review-rust-gem/SKILL.md` | `095cd443ed0c776b3de86c8d6c44977777ffc709a7d1fe4fc0485e78ab89469e` | 122 |
| `write-evidence-rubric/SKILL.md` | `ad3d520e9148889b943831ce8c6342e45da2dded1ad2afadf004fe2b23d52a8c` | 98 |
| `build-greenfield-compiler-ir/SKILL.md` | `c5d27a9cb2430e3939946b87b0a7f47f7aa968ec2e5ea3cee6cfc753cf7e6212` | 141 |
| `design-zero-cost-dispatch/SKILL.md` | `667c2e324e4affa544d25d9fe19b9a0a2dfd40ee3a124f43da39085357b33189` | 148 |
| `PACKED_COLLECTIONS.md` | `32d1b9f63718b96c38fb82a967d3fbea621a6e53a52ae2e783152ba47ba26a1b` | 135 |
| `TESTING.md` | `1d24abcd200346c65944ca33b1283e49b68f059a33c30aee1eb17f08ac2033b6` | 104 |

The table is the immutable pre-documentation specimen, not a claim about the candidate documentation
commit. This card and the plan closure deliberately amend that specimen. The re-frozen calibration pair
is `COMPILER_IR_GREENFIELD_PLAN.md` SHA-256
`0a4ac3b30d0965b78b9164126d78bd81f3f2d1b95095d62c5100260c1c181e31` (361 lines) and this card as it
exists at each cold trial; the calibration transcript records each card digest. A reader must reject a
claim that the original 327-line plan is unchanged, but must not mistake this declared documentation-only
amendment for an unapproved production-scope change.

## C0.1 terminal and settled decisions

An external consumer selects one of two closed synthetic frontend rows at the application boundary and
gets that row's concrete synchronous borrowed result. `RustSubset` accepts `Parse` and `LowerIr`;
`TypeScriptSubset` accepts `Parse` and rejects `LowerIr` with exact typed
`UnsupportedStage { language, stage }`. A concrete `RustSubsetOnly` registry cannot name a TypeScript
route. The only `Language` variants are `RustSubset` and `TypeScriptSubset`; the only `Stage` variants
are `Parse` and `LowerIr`; `FrontendError` has only that one variant and both fields have those enum
types. The manual frontend contract lends the caller's source as
`fn drive(stage: Stage, source: &[u8]) -> Result<&[u8], FrontendError>`. Public tests drive both full
rows, prove successful paths return the identical borrowed region, assert the exact rejection, and use
compile-fail doctests for owner mixing and the missing subset member. This is one boundary dispatch proof, not
lowering, fragment construction, canonical bytes, recipe/job identity, scheduling, bundles, or a real
frontend.

The complete permitted public Rust surface is exactly: `pub enum Entity {}`, `pub enum Type {}`,
`pub struct DenseId<Owner>` with named public `raw: u32`, a private brand, and only
`pub const fn new(raw: u32) -> Self`, type
aliases `EntityId` and `TypeId`, `pub enum Language { RustSubset, TypeScriptSubset }`,
`pub enum Stage { Parse, LowerIr }`, `pub enum FrontendError { UnsupportedStage { language: Language,
stage: Stage } }`, `pub struct FullRegistry` with only the value method
`pub fn dispatch(self, language: Language, stage: Stage, source: &[u8]) ->
Result<&[u8], FrontendError>`, and `pub struct RustSubsetOnly` with only the value method
`pub fn parse(self, source: &[u8]) -> &[u8]`. All necessary derives are
restricted to `Clone`, `Copy`, `Debug`, `Eq`, and `PartialEq`; every other item is private. In particular
there is no `DenseId` accessor, conversion, public trait, re-export, frontend type, or additional
constructor.

- Tags/capability difference: `RustSubset` supports `LowerIr`; `TypeScriptSubset` does not. They are
  greenfield synthetic names, not compatibility names.
- Registry mechanism: one normally readable manual closed expansion. No macro/delegation crate is earned:
  no two independent production macro invocations exist. The manual expansion is the future macro
  control. A later C0 child may earn a private declarative macro only by deleting measured repetition,
  retaining the expansion, and adding source-spanned compile-fail/text evidence.
- Subset mechanism: one concrete `RustSubsetOnly` registry type, not Cargo features. It proves absence
  without locking a permanent feature-selection product/build contract.
- `RustSubsetOnly` exposes only a Rust-specific entry point; it must not expose any method taking the
  full `Language` enum, delegate to the full registry, or return a runtime TypeScript rejection. The
  compile-fail witness attempts `RustSubsetOnly.parse_typescript(&[])`, the actual absent subset entry
  point; its only supported method is `RustSubsetOnly.parse(&[u8]) -> &[u8]`, not a decorative
  constructor. Both registry tokens are first-class values rather than associated-function namespaces.
- Coordinates: `DenseId<Entity>` and `DenseId<Type>` are transparent, owner-branded `u32` values. No
  central identity registry edit, unresolved/import/wire bits, or cross-kind conversion is authorized.
- Text measurement: `/usr/bin/size -m` on named release test executables plus `rustc -vV`. A missing
  host utility is recorded exactly and cannot be masked by a dependency/tool change.

## Exact later write ownership

Every listed package file is absent at the frozen base (digest `absent`, LOC `0`). No wildcard is
authority.

```text
workspace2/crates/nudox-ir-vocab/Cargo.toml
workspace2/crates/nudox-ir-vocab/src/lib.rs
workspace2/crates/nudox-ir-vocab/tests/coordinates.rs
workspace2/crates/nudox-compile-vocab/Cargo.toml
workspace2/crates/nudox-compile-vocab/src/lib.rs
workspace2/crates/nudox-compile-registry/Cargo.toml
workspace2/crates/nudox-compile-registry/src/lib.rs
workspace2/crates/nudox-compile-registry/tests/dispatch.rs
workspace2/crates/nudox-compile-registry/tests/subset.rs
```

The root lockfile is the committed build input and every terminal Cargo command uses `--locked`;
the worktree must remain clean after a gate. Forbidden: root manifest/lockfile edits; central registry;
existing crate; dependency; real frontend SDK;
macro crate; test-support crate; fragment/wire/builder/view; recipe/job/driver; cache/scheduler/sandbox/
process/object-store/server/bundle; async/runtime; `serde`; `async-trait`; public `dyn`; `Box<dyn Error>`;
`Arc`; unsafe; SIMD; legacy alias/behavior; all unlisted public surface.

## Evidence and stop matrix

| Law | Exact artifact | Falsifier / cap |
| --- | --- | --- |
| Closed dispatch | Full manual two-row match calls concrete generic paths once at boundary. | Any `dyn`, erased error, registry scan, plugin lookup, default/fallback, or traversal tag match is `BLOCKER`. |
| Distinct rows | External test observes zero-copy Rust parse/lower and TypeScript parse borrows plus TypeScript lower exact operands. | Ignoring/not forwarding source, two tags to one type, nominal no-op rows, or no supported-stage difference is `BLOCKER`. |
| Static subset | `RustSubsetOnly` omits TypeScript; compile-fail doctest cannot name it. | Runtime tag admission or compiling absent member is `BLOCKER`. |
| Branded dense IDs | Real `size_of`/alignment and owner-mixing negative witness. | Cross-owner conversion, phase bits, or forgeable coherent witness is `BLOCKER`. |
| Inward topology | IR vocab is `no_std`; compiler vocab depends inward; registry depends only on compiler vocab. | SDK/runtime/serde/async/reverse dependency/root-manifest edit is `BLOCKER`. |
| Public terminal | `tests/dispatch.rs` drives both rows and exact error. | Private-only test, `is_err()` only, or integer shadow model is `BLOCKER`. |
| Layout/text proof | real layout assertions, `rustc -vV`, and `size -m` raw output. | A claim without raw output is `MAJOR`; no approval. |

After every file, count normally formatted net new physical lines in the four production groups and two
test groups above. The next file is forbidden when written lines plus the unmodified remaining group
forecast exceed 200 production or 120 test lines: those are the forecast ceilings after reserve, not the
225/140 hard caps. A file is also forbidden at more than 20% or 25 lines over its group forecast, on an
unlisted path/public item, or whenever its change would consume the 25/20-line reserve. No packed
formatting or docs omission creates capacity.

## Formatted skeleton and budgets

A temporary normally formatted three-crate control was compiled outside future writable paths. Its
manual full-registry `match` had two descriptive concrete generic parameters; both synthetic frontend
tests passed and TypeScript `LowerIr` returned the typed error. With `RUSTC_WRAPPER=`,
`cargo fmt --check`, `cargo check --workspace --all-targets`, and `cargo test --workspace --all-targets`
passed (two integration tests). The ambient `sccache` first failed with `EPERM`; clearing only that
wrapper reproduced the checks. Formatted control LOC: 26 IR vocabulary, 25 compiler vocabulary, 23
registry, 46 integration test, total 120. It deliberately lacks subset, coordinate, docs, and full
negative evidence, so it is a forecast control, not a candidate.

| File group | kind | forecast LOC |
| --- | --- | ---: |
| IR vocabulary source and its crate manifest | production | 54 |
| compiler vocabulary source and its crate manifest | production | 52 |
| registry source and its crate manifest | production | 60 |
| shared root workspace manifest/lock integration | integration (not charged) | 0 |
| IR coordinate test | test | 32 |
| public dispatch and subset tests | test | 88 |
| **production forecast / hard cap / unused reserve** | production | **200 / 225 / 25** |
| **test forecast / hard cap / unused reserve** | test | **120 / 140 / 20** |

Caps: zero added dependencies, unsafe blocks, `alloc` dependency closure, payload copies, retained owners,
and allocation sites on dispatch; one boundary selection, one concrete call, no scan; exact four-byte/
four-alignment dense ID on this target; release text delta no more than 8 KiB against the empty
root-workspace control. C0.1 makes no allocator-performance claim: its `#![no_std]`, no-`alloc`,
zero-dependency closure plus source audit prove allocation is unavailable on the selected path; the later
allocator-instrumented IR builder claim belongs to C1.

## Commands and first builder card

```text
cd /private/tmp/nudox-autonomous-compiler-contract/workspace2
RUSTC_WRAPPER= cargo fmt --manifest-path Cargo.toml --all -- --check
RUSTC_WRAPPER= cargo check --locked --manifest-path Cargo.toml --workspace --all-targets
RUSTC_WRAPPER= cargo test --locked --manifest-path Cargo.toml --workspace --all-targets
RUSTC_WRAPPER= cargo test --locked --manifest-path Cargo.toml -p nudox-compile-registry --doc
RUSTC_WRAPPER= cargo clippy --locked --manifest-path Cargo.toml --workspace --all-targets -- -D warnings
rustc -vV
RUSTC_WRAPPER= cargo test --locked --release --no-run --manifest-path Cargo.toml --workspace --all-targets
find target/release/deps -type f -perm -111 -name 'dispatch-*' -print -exec /usr/bin/size -m {} \;
find target/release/deps -type f -perm -111 -name 'nudox_compile_registry-*' -print -exec /usr/bin/size -m {} \;
git diff --check
```

One fresh builder turn may create exactly the nine package files above. It must deliver the compileable
root-workspace package skeleton, two branded coordinates, full/manual and `RustSubsetOnly` registries, and external
two-row/exact-error tests; commit only these paths after root format/check/test gates. It stops before
a macro, compile-fail harness dependency, feature mechanism, real frontend, or C1 type. Its adversarial
test replaces both frontend implementations with the same behavior or admits TypeScript through
`RustSubsetOnly`; the named tests must fail. It records raw layout, dependency-closure, and text output
and its commit; C0.1 does not request allocator instrumentation.

## C0 decomposition, calibration, and next decision

`C0.1` is this vocabulary/manual-dispatch proof. `C0.2` may add an earned declarative registry after two
independent production invocations and code-size/diagnostic proof. `C0.3` closes the remaining C0
vocabulary/compile-fail matrix without fragment bytes. C1 begins only after all C0 cards close; C5 is
bundle acquisition and C6 is the published incremental consumer terminal.

The first fresh child was spawned with explicit `model: gpt-5.6-luna` and `fork_turns: none`; the runtime
accepted task `/root/autonomous_compiler_c0_manager/c0_reader_one`. It independently found the two-row
terminal, absent paths, root commands, and no-root-manifest boundary, but over-escalated synthetic tags,
subset mechanism, manual/macro choice, central registry, and text tool. This card settles those facts.

The remaining cold deck is mandatory before a builder: second reader, plausible misreader, reviewer, then
post-rewrite reader. Their raw reports are preserved beside this card. The reviewer must reject: shared
no-op adapter rows; a runtime TypeScript route in `RustSubsetOnly`; macro/`trybuild` dependency; owner-ID
mixing; root/central/legacy edits; `is_err()`-only or absent raw text evidence; and zero-cost claims that
hide allocation or `dyn`. Missing any rejection is a blocker.

The second fresh `gpt-5.6-luna` reader (`fork_turns: none`) reproduced every C0.1 field and found one
contract ambiguity: the baseline plan digest in the table no longer matched the amended plan. The
re-freeze paragraph above is the narrow rewrite; it changes no capability, path, cap, or evidence row.

Exact next decision: **commission the first builder card only if all four fresh trials reproduce this
rewritten C0.1 terminal, nine package paths, prohibited surface, caps/reserve, commands, and no authority
question with zero blocker/major ambiguity.**
