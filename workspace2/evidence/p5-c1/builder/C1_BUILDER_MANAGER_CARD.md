# C1 builder — prepared caller-output writer for the existing two-lane envelope

## Capability and custody

This card owns one observable transition only: typed caller facts become the existing padding-free
two-lane wire bytes in a caller-provided buffer, then the existing validator once creates the correlated
borrowed `FragmentView` for those exact written bytes. The immutable production baseline is clean commit
`f1d0cc95b5728a1f341db02945d913a26a5fc7bf`; this card was introduced at
`74d35fd3c67f72630352ad7fecedb507e63dad69` and corrected at
`c4a19ca43938ce4ed55a6d00dd9d83ed3d6f32fe`, on `/private/tmp/nudox-prototype-real-compiler-ir`,
branch `codex/prototype-real-compiler-ir`. This complete card body at the clean current commit is the
only governing card document for this slice; older card/evidence commits are retained historical
checkpoints only. Production edits start only from this clean card commit, while `f1d0cc95` remains
the production source/hash ledger.

| commit | custody role |
| --- | --- |
| `f1d0cc95b5728a1f341db02945d913a26a5fc7bf` | independently reproduced C1-FORMAT production baseline |
| `74d35fd3c67f72630352ad7fecedb507e63dad69` | first writer control/card checkpoint; retained, superseded |
| `c4a19ca43938ce4ed55a6d00dd9d83ed3d6f32fe` | first calibration correction; retained, superseded |
| `558efd2b3c28c61dd4e1e0e6cce04def84d09de3` | ambiguity repair; retained, superseded |
| `2837a5d4ecda5160d61c9ae1ce3944931ac8bd3d` | V1 calibrated card; retained, superseded by authority simplification |
| `60df0c592fd34a03189cfdd986292434527235b9` | hostile-evidence hardening; retained, superseded by authority simplification |
| `a9094d4f200bb49d028e830904dd8df90c4a5e0d` | V2 writer authority/card and skeleton checkpoint; current-card predecessor |

The retained format validator is the sole wire authority. The writer has no staging owner, returns
only the exact written byte prefix, and never constructs a view, validates, or introduces an encoder/
parser authority. The caller validates that returned prefix exactly once.

`workspace2/evidence/p5-c1/format/C1_FORMAT_MANAGER_CARD.md` records an earlier NIRF/descriptors/BE
detached control and is explicitly non-governing for this child. The only format authority here is the
actual simple `0xc1` baseline source at `f1d0cc95` and its reproduced rescue evidence; no NIRF byte,
descriptor, or golden can enter this card.

## Allowed paths and frozen baseline

| path | baseline LOC | SHA-256 | allowed change |
| --- | ---: | --- | --- |
| `workspace2/domains/ir/crates/nudox-ir-format/src/lib.rs` | 146 | `cc5506ad173201b6cd7a4015757a52bf8e4f41b2fa8393814eca6919c8ef26ca` | prepared state, typed prepare/write errors, direct writer, `FragmentView` envelope `AsRef` only |
| `workspace2/domains/ir/crates/nudox-ir-format/tests/fragment.rs` | 322 | `94a02b1ccde791dacf4478779e3bbe839365869e4269360a2951be4c5af9b1cd` | public writer, negative API, mutation, layout, and actual-rlib evidence; replace former `PreparedFragment` E0432 absence fixture |
| `workspace2/domains/ir/crates/nudox-ir-format/tests/prepared_consumer.rs` | absent | absent | named release consumer and manual single-pass control only |
| `workspace2/evidence/p5-c1/builder/**` | absent from production baseline | n/a | card checkpoint, frozen skeletons, raw custody and closure evidence |

No manifest, lockfile, vocabulary, compiler, frontend, recipe, C2, scheduler, publication, or
other format lane changes are allowed.

## Exact public surface

The only added public items are `PreparedFragment<'facts>`, `PrepareError`, `WriteError`, and the
standard `AsRef<[u8]> for FragmentView<'_>` implementation. `AsRef` returns only the existing private
envelope borrow; it exposes no lane internals or raw constructor.
`PreparedFragment::prepare(&[EntityId], &[TypeId]) -> Result<PreparedFragment<'facts>, PrepareError>`
borrows both input slices, rejects a lane longer than existing `MAX_LANE_ITEMS` with exactly
`EntityCount { actual: usize }` or `TypeCount { actual: usize }`, and records one private exact
output length. Entity-count validation has first-error priority: if both lanes exceed the bound,
`EntityCount` is returned. Both errors derive only `Clone, Copy, Debug, Eq, PartialEq`; neither is an allocation
or source-carrying error because no rejected owner/source exists. `output_len(&self) -> usize` exposes
that independent fact and is not `const`. Its consuming exact signature is
`pub fn write_into<'output>(self, output: &'output mut [u8]) -> Result<&'output [u8], WriteError>`.
It emits only the required prefix and returns that exact immutable prefix; its caller obtains a view only
by calling existing `FragmentView::validate` once. `WriteError::OutputTooSmall
{ required, available }` is the only write error.

All prepared fields remain private; `prepare` is the sole public checked associated constructor. There
is no `Clone`, `Copy`, `From`, `TryFrom`, allocation convenience method, raw byte input, untyped ID input, output owner, or public view
constructor. The writer never constructs a `FragmentView`; existing `FragmentView::validate` remains
the only raw-input validator and sole view constructor. “Private construction” names only
`PreparedFragment` internals.

The old actual-rlib `PreparedFragment`-absence (`E0432`) fixture is deleted. Its earned replacements
are: a direct literal with every private prepared field that fails E0451, a raw `.into()`/`.try_into()`
fixture that fails E0277, and a source-scope fixture that fails E0597. The named validation success
fixture remains and the typed E0308 fixture uses explicit custodied vocab input.

The actual-rlib helper resolves exactly one `libnudox_ir_format-*.rlib` and one
`libnudox_ir_vocab-*.rlib` beside `current_exe`, invoking `shasum -a 256` for each and retaining each
path/cardinality/[u8; 64] digest in one `RlibCustody` pair. Its child has both explicit externs:
`--extern nudox_ir_format=<format path>` and `--extern nudox_ir_vocab=<vocab path>`. The E0308 source
imports both IDs from vocab and calls `PreparedFragment::prepare(&[TypeId::new(7)], &[TypeId::new(3)])`;
the legal mutant changes only the first `TypeId` spelling to `EntityId`, then compiles. The E0597 source
declares prepared outside an inner scope holding the typed arrays and uses `prepared.output_len()` after
that scope. Every child keeps the existing one-coded-primary/no-uncoded-primary custody predicate.
Existing absent-surface fixtures for `FragmentBuilder`, ID reexports, and `atom_ids` remain unchanged;
only the former `PreparedFragment` E0432 absence fixture is replaced because this slice earns that type.

## Wire and write authority

```text
&[EntityId] + &[TypeId]
        | prepare: counts + exact 4 + 4E + 4T
PreparedFragment<'facts> (private borrows, exact length)
        | full-prefix capacity check; no mutation on failure
caller &mut [u8] -- direct canonical header/lanes --> &'output [u8] exact prefix
        | existing sole validator, once
FragmentView<'output> (private correlated input/entity/type borrows; AsRef envelope only)
```

The byte order is the existing `MAGIC`, `SCHEMA`, entity count, type count, then LE entity and type
`u32` lanes. The exact writer order is: check complete required prefix; take that prefix; write all
four header bytes; write every entity; write every type; return the immutable exact prefix. No write occurs
before the capacity check. A suffix beyond `output_len`
is never borrowed mutably as a written region and remains byte-identical.

## Coverage and falsifiers

| law | evidence | falsifier |
| --- | --- | --- |
| canonical bytes | 0/1/2 typed fact cases equal fixed full-`u32` goldens, including entity `0x0102_0304` and type `0xa0b0_c0d0`; each returned prefix validates once with the existing public validator | constant, swapped, or low-byte-only writer differs from golden/view |
| capacity atomicity | every shorter output has exact `WriteError` and all sentinel bytes unchanged | partial header/body write mutates sentinel |
| suffix preservation | exact valid prefix changes and every extra byte retains sentinel | broad-slice fill/copy changes suffix |
| typed inputs | actual-rlib E0308 fixture with exactly one digest-recorded format rlib and one digest-recorded vocab rlib, passed by explicit `--extern`, shows `TypeId` cannot occupy entity input; one legal changed-ID mutant compiles | erased/raw slice signature accepts cross-kind input |
| private state | actual-rlib E0451 fixture names every private prepared field | public literal/constructor compiles |
| lifetime authority | actual-rlib E0597 fixture keeps prepared state after source arrays die | prepared owns/copies facts or lifetime is erased |
| view authority | public `FragmentView::as_ref()` pointer/range equals the returned caller prefix and public 0/1/2 cursors equal supplied typed IDs | validate a different backing or build independent wire authority |
| direct body | retained `mutants/constant-body.patch` and `mutants/partial-write.patch` apply to a copied exact candidate source and their named focused test fails | tests only inspect header/success |
| representation | exact host-specific `size_of` and alignment for prepared/view are asserted and recorded | non-ZST owner/field change alters the frozen host observation unnoticed |
| resource claim | named release consumer and same-input manual control are inspected for allocator, memcpy/staging, indirect, and panic paths | claim comparability or erasure without an inspectable artifact |

## Resource and codegen control

`tests/prepared_consumer.rs` defines two same-signature, `#[inline(never)]` functions over typed
fact slices and caller output. `prepared_whole_consumer` calls public prepare, write, one existing
validation, and consumes the view. `manual_single_pass_control` repeats the same entity-first count
checks, direct write order, one existing validation, and view consumption without a prepared owner.
Both have exact signature
`fn(&[EntityId], &[TypeId], &mut [u8]) -> Result<usize, ConsumerError>`, where the private test-only
`ConsumerError` has exact typed variants `Prepare(PrepareError)`, `Write(WriteError)`, and
`Validate(FragmentError)`. On success both consume both typed cursors and return their combined length;
for all inputs each returns `Prepare` after entity-then-type count checks, `Write` after the same
exact output-length check, or `Validate` after the one validator call. Their caller black-boxes equal
input slices, outputs, and results. The manual control is required to return exactly the same variant
and leave exactly the same output bytes for every 0/1/2 and rejected input/output case; this is the
comparability criterion. The release test executable is the
only codegen artifact; broad binary and rlib totals are inadmissible. Inspect its LLVM IR and assembly
for both callables, recording whether each has allocator calls, memcpy/memmove, stack/heap staging,
indirect calls, and panic/unwind paths. If stable attributable callables or comparable codegen cannot
be obtained, mark this row `UNVERIFIED` and do not make a zero-cost/erasure claim. The call graphs
are deliberately equivalent through one shared public validation; residual direct count/write calls
are recorded honestly rather than described as erased.

The frozen host is Rust 1.97.1, `aarch64-apple-darwin`, default Cargo release profile, no LTO or custom
target flags. The literal release artifact command is
`CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-release RUSTC_WRAPPER= cargo rustc --locked --manifest-path workspace2/domains/ir/Cargo.toml --release --test prepared_consumer -- --emit=llvm-ir,asm`.
The inspection record names the produced `deps/prepared_consumer-*.ll` and `*.s`, exact source hash,
and the matching callable definitions/call sites; it searches `__rust_alloc`, `memcpy`, `memmove`,
indirect AArch64 `blr`, and `panic`/`unwrap_failed`. Absence is reported only per attributable callable,
never as whole-binary erasure.

## Skeleton and budgets

Frozen manual skeletons are `skeleton/v2-lib.rs`, `skeleton/v2-fragment.rs`, and
`skeleton/v2-prepared-consumer.rs`; their bodies and inventory are the pre-edit ceiling authority.

| file | frozen baseline | net-delta forecast | net-delta cap | total-file cap | unused reserve |
| --- | ---: | ---: | ---: |
| `src/lib.rs` | 146 | 118 | 160 | 306 | 42 |
| `tests/fragment.rs` | 322 | 154 | 210 | 532 | 56 |
| `tests/prepared_consumer.rs` | 0 | 96 | 130 | 130 | 34 |
| evidence cards/results | 0 | 160 | 240 | 240 | 80 |

The phase caps are net-delta only: 160 production LOC for `src/lib.rs`, and 340 test LOC across the
two test files, excluding the 146/322 frozen source/test baseline. The test net forecast is 250
(`154 + 96`), leaving 90 net test LOC unused. The total-file caps above make that arithmetic reviewable;
all listed per-file reserves remain unused.
No allocation, `Vec`, `Box`, `String`, `alloc`, serde, `dyn`, macro definitions/invocations, unsafe,
or testkit is permitted in the format source or writer evidence. Ordinary Rust attributes required by
the language/test harness, including `#[test]` and `#[inline(never)]`, are not macro-based surface.
The inherited `compile_error!` text is permitted only inside the existing actual-rlib noisy-diagnostic
negative fixture; no new macro call or macro definition is authorized. The std-only test harness may keep its existing actual-rlib
process/custody code, but new writer fixtures use arrays and typed exact errors.

## Gates and stop conditions

The exact fresh gate command sequence is run twice, each command prefixed with
`CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-gate-one RUSTC_WRAPPER=` for gate one and with
`CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-gate-two RUSTC_WRAPPER=` for gate two:

```text
CARGO_TARGET_DIR=<fresh-target> RUSTC_WRAPPER= cargo fmt --manifest-path workspace2/domains/ir/Cargo.toml --all -- --check
CARGO_TARGET_DIR=<fresh-target> RUSTC_WRAPPER= cargo test --locked --manifest-path workspace2/domains/ir/Cargo.toml --workspace --all-targets -- --nocapture
CARGO_TARGET_DIR=<fresh-target> RUSTC_WRAPPER= cargo clippy --locked --manifest-path workspace2/domains/ir/Cargo.toml --workspace --all-targets -- -D warnings
git diff --check
git status --short
```

The exact representation assertions are `size_of::<PreparedFragment<'static>>() == 40`,
`align_of::<PreparedFragment<'static>>() == 8`, `size_of::<FragmentView<'static>>() == 48`, and
`align_of::<FragmentView<'static>>() == 8` on the frozen host. They are host/toolchain-specific
regression observations, not a Rust ABI or proof against zero-sized private state. The writer card stops before
any unplanned public item, dependency, allocation policy, unsafe, C2 surface, budget breach, missing
actual-rlib custody, non-comparable codegen claim, or a failing hostile falsifier.

On a host/toolchain other than frozen Rust 1.97.1 `aarch64-apple-darwin`, the exact layout assertions
are not run or generalized: record the observed layout separately as `UNVERIFIED` and do not use it for
promotion. The remaining semantic gates still run normally.

`AsRef<[u8]> for FragmentView` is exactly `&self.envelope`. It permits only pointer/length comparison
to the returned caller prefix, never lane exposure or alternate construction. After the checked
`usize` bound of two items, each count is narrowed only for the one-byte wire cell; no arbitrary
`usize` reaches the wire. The writer's prefix split is specified solely by observable behavior: every
byte below `output_len` is canonical on success and every byte at/after `output_len` is unchanged.

The next decision after this card is exactly: retain/reject/promote this isolated C1 writer experiment;
no merge or product/C2 decision follows.
