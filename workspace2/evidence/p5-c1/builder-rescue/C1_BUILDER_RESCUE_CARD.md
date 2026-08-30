# C1 builder rescue — prepared caller-output writer for the existing two-lane format

## Capability and custody

This is one isolated prototype boundary: typed entity and type fact slices are prepared once, then
written directly into a caller-provided buffer in the existing two-lane envelope. The writer returns
only the exact borrowed written prefix. The caller invokes `FragmentView::validate` exactly once on
that prefix; validation remains the sole wire proof and view constructor.

The production source baseline is `f1d0cc95b5728a1f341db02945d913a26a5fc7bf` on branch
`codex/prototype-real-compiler-ir`. The pre-rescue handoff HEAD `263a2f82` contained rejected
builder-card-only history and no format production edit. This card's frozen commit will be recorded
only by the *new* calibration receipt written after the fresh four-role deck; the presently checked-in
R9 receipt is an explicitly rejected historical falsifier, not custody for this card. All material under `workspace2/evidence/p5-c1/builder/**` and
`/private/tmp/p5-c1-builder-v2-skeleton` is a rejected counterexample corpus. In particular, its
magic-only mutant is not a causal writer mutant and earns no evidence.

`f1d0cc95b5728a1f341db02945d913a26a5fc7bf` is the sole source-edit, diff, LOC, and production-digest
baseline. A commit that stores this card or calibration evidence is card custody only and is never a
second production baseline.

The committed executable control is
`workspace2/evidence/p5-c1/builder-rescue/skeleton/executable-control`, an excluded Cargo workspace
with the exact public writer API, full fixed-array writer suite, and paired release consumer. Its
locked format/test run passed from committed custody before this card update. It is a pre-card
feasibility specimen, not production authority: its short test names are representative only; the
named production test inventory below is mandatory and independent.

The frozen formatted skeleton ledger is:

| skeleton path | formatted LOC | SHA-256 |
| --- | ---: | --- |
| `skeleton/lib.rs` | 101 | `3e7646c134c0d8b4931ffc912ccdd1a7572f56f67e3fd9d596affb5d8328dcc6` |
| `skeleton/fragment.rs` | 37 | `6eca6e68abbe7508d5a0103999011cbdd914e38552c1476f96f86144077e142f` |
| `skeleton/prepared_consumer.rs` | 31 | `53422dadbde32f37fe8ddf456808d9c61739d7d68f280e0a4492ad7a0a9cea8d` |

The complete executable-control ledger is:

| control path | formatted LOC | SHA-256 |
| --- | ---: | --- |
| `skeleton/executable-control/Cargo.toml` | 10 | `eb2b65cdbf86f6d7cdb06bfa98293452dc8c6ca047c2e7660de99ee5e1aa70ab` |
| `skeleton/executable-control/Cargo.lock` | 14 | `00e736c9ec04d05db57b963b08272ed59183edf4ede56e272389b8fa1bacad77` |
| `skeleton/executable-control/CONTROL_RUN.md` | 13 | `b2d89f74c904f3cbe8a90973d49d9c307884e3ac512e41269eb4538caa58cee0` |
| `skeleton/executable-control/src/lib.rs` | 319 | `d771b0e3c38bc221947b20ade61029c4b2e8e35e698bc07b5d650fa3cc700b76` |
| `skeleton/executable-control/tests/fragment.rs` | 139 | `ff67c560183c668eff86a246339f0446129f3c4348200b5417364ca95964ee31` |
| `skeleton/executable-control/tests/prepared_consumer.rs` | 162 | `4474b81ee45428065683b0c1b8136dbe1930417c8f96b7d3904fb27a943b91c8` |

## Allowed paths and frozen baseline ledger

| path | baseline formatted LOC | SHA-256 | allowed change |
| --- | ---: | --- | --- |
| `workspace2/domains/ir/crates/nudox-ir-format/src/lib.rs` | 146 | `cc5506ad173201b6cd7a4015757a52bf8e4f41b2fa8393814eca6919c8ef26ca` | prepared state/errors/direct writer and `AsRef<[u8]> for FragmentView` only |
| `workspace2/domains/ir/crates/nudox-ir-format/tests/fragment.rs` | 322 | `94a02b1ccde791dacf4478779e3bbe839365869e4269360a2951be4c5af9b1cd` | writer, actual-rlib, mutation, pointer, layout tests; replace only the former prepared-absence fixture |
| `workspace2/domains/ir/crates/nudox-ir-format/tests/prepared_consumer.rs` | absent | absent | named paired manual/prepared release consumer only |
| `workspace2/evidence/p5-c1/builder-rescue/C1_BUILDER_RESCUE_CARD.md` | absent at `f1d0cc95` | absent at `f1d0cc95` | canonical card only |
| `workspace2/evidence/p5-c1/builder-rescue/skeleton/lib.rs`, `fragment.rs`, `prepared_consumer.rs`, and `executable-control/{Cargo.toml,Cargo.lock,CONTROL_RUN.md,src/lib.rs,tests/fragment.rs,tests/prepared_consumer.rs}` | absent at `f1d0cc95` | absent at `f1d0cc95` | frozen skeleton/control only |
| `workspace2/evidence/p5-c1/builder-rescue/calibration/CALIBRATION.md`, `calibration/HOSTILE_PREEDIT.md`, `mutants/constant-body.patch`, `mutants/partial-write.patch`, `mutants/MUTANT_RUN.md`, `codegen/CODEGEN.md`, and `closure/CLOSURE.md` | absent at `f1d0cc95` | absent at `f1d0cc95` | human-authored role, mutation, codegen, and closure receipts only |
| `workspace2/evidence/p5-c1/builder-rescue/codegen/{prepared_consumer.ll.gz,prepared_consumer.s.gz,nudox_ir_format.ll.gz,nudox_ir_format.s.gz}` | absent at `f1d0cc95` | absent at `f1d0cc95` | deterministic generated code artifacts only |

No manifest, lockfile, vocabulary, format lane, view constructor, recipe, frontend, compiler, C2, or
orchestra path is writable. No `unsafe`, `Vec`, `Box`, `String`, `alloc`, serde, `dyn`, macro
definition/invocation, `unwrap`, `expect`, or `map_err` is permitted in production writer logic or
new public testkit surface. Fixed stack arrays are required for new writer corpus. Private std
process/diagnostic custody plumbing may use existing necessary string/process facilities but may not
become a public testkit or writer authority.

## Exact public inventory

The only new public items are:

```rust
pub struct PreparedFragment<'facts> { /* private borrowed slices, checked counts, exact length */ }
pub enum PrepareError { EntityCount { actual: usize }, TypeCount { actual: usize } }
pub enum WriteError { OutputTooSmall { required: usize, available: usize } }
impl<'facts> PreparedFragment<'facts> {
    pub fn prepare(&'facts [EntityId], &'facts [TypeId]) -> Result<Self, PrepareError>;
    pub fn output_len(&self) -> usize;
    pub fn write_into<'output>(self, &'output mut [u8]) -> Result<&'output [u8], WriteError>;
}
impl AsRef<[u8]> for FragmentView<'_>;
```

`PreparedFragment` borrows exactly the supplied typed fact slices and retains private checked one-byte
counts plus exact length. It has no public fields, `Clone`,
`Copy`, raw constructor, `From`/`TryFrom`, owned-output helper, writer/view constructor, or validation
method. Both errors derive only `Clone`, `Copy`, `Debug`, `Eq`, and `PartialEq`. `AsRef` returns
only the existing private `envelope` borrow and exists solely for public returned-view/caller-prefix
containment comparison. No lane getter or new wire authority is earned.

## Wire and authority order

The fixed bytes are `MAGIC`, `SCHEMA`, entity count, type count, then little-endian `u32` entity lane
then little-endian `u32` type lane. `prepare` validates entity count before type count, each against
`MAX_LANE_ITEMS`, and calculates exactly `4 + 4E + 4T`. When both exceed, it returns
`PrepareError::EntityCount { actual }`.

The inherited production constants are literal: `MAGIC = 0xc1`, `SCHEMA = 1`, `HEADER_BYTES = 4`,
`ENTITY_BYTES = 4`, `TYPE_BYTES = 4`, and `MAX_LANE_ITEMS = 2`. They remain existing format authority;
this child adds no replacement constant or wire geometry.

Preparation first performs `match u8::try_from(actual)` entity-first and returns the same exact lane
error on conversion/range rejection; it stores those private checked counts. Writing copies only those
proved count bytes into the two one-byte cells, so no arbitrary `usize` is narrowed at write time.
The prepared writer and paired manual control name private offsets for all four header cells.

`write_into` first checks full output capacity. On failure it returns the exact required/available error
and changes no output byte. On success it takes only `output[..output_len]`, writes canonical header,
entity bytes, and type bytes directly, leaves every suffix byte unchanged, and returns that same immutable
prefix. Each successful whole-consumer invocation may then call the existing `FragmentView::validate`
once and only once; standalone writer tests may validate their independently written prefix for their
own assertion and are not whole-consumer validation counts. A successful writer itself never produces a
view.

## Required tests and falsifiers

The exact `fragment.rs` test names are `prepared_writer_zero_one_two_full_width_goldens_and_cursors`,
`prepared_writer_entity_first_prepare_errors`, `prepared_writer_all_short_outputs_are_unchanged`,
`prepared_writer_exact_prefix_and_suffix`, `prepared_writer_public_view_matches_returned_prefix`,
`actual_rlib_writer_surface_is_typed_private_and_borrowing`, and
`prepared_writer_layout_is_frozen_host_observation`. The consumer test is
`prepared_and_manual_whole_consumers_are_equivalent`. The two retained mutant commands target,
respectively, `prepared_writer_zero_one_two_full_width_goldens_and_cursors` and
`prepared_writer_all_short_outputs_are_unchanged`.

| law | named evidence | exact falsifier |
| --- | --- | --- |
| wire bytes/cursors | 0/1/2 full-width goldens include entity `0x0102_0304` and type `0xa0b0_c0d0`; each write validates once and both cursors are exhausted/fused | fixed payload body, swapped lane, or low-byte-only write |
| entity-first preparation | both-over-limit, entity-only over-limit, type-only over-limit assert exact errors | type check before entity check or generic limit error |
| capacity atomicity | every `available` in `0..output_len` returns exact `OutputTooSmall` and a complete sentinel array is unchanged | any header/payload write before preflight |
| exact prefix/suffix | success changes exactly canonical prefix and preserves three sentinel suffix bytes | broad output fill/copy |
| public containment | `FragmentView::as_ref()` equals and has the same pointer/length as returned prefix | alternate backing/view constructor |
| type/private/lifetime surface | actual-rlib harness: exact one format rlib plus one vocab rlib, external SHA-256 path/cardinality/hash custody, explicit two `--extern`s; E0308 cross-kind and legal mutant; E0451 separately names every stored field; genuine source-scope lifetime error, expected E0597 when stable | raw slices, public fields, copied/lifetime-erased prepared state |
| prepared provenance | private unit test compares stored entity/type slice pointers and lengths to caller slices; source tripwire isolates the `PreparedFragment` declaration and requires exactly its five listed fields, including both borrowed slices, while rejecting embedded ID arrays or `PhantomData` provenance | padded copy-backed fields, hidden typed caches, or lifetime-only marker |
| conversion range | fixed `[EntityId; 256]`, `[TypeId; 256]`, and both-invalid entity-first cases preserve exact `actual: 256` errors | cast-before-check wraps 256 to zero |
| direct writer body | digest-pinned `constant-body.patch` and `partial-write.patch` apply to copied exact source; pristine passes and each named test is red | header-only magic mutation or non-causal test |
| paired release consumer | identical black-boxed typed input/output uses `#[inline(never)]` manual and prepared whole callables; each writes then validates exactly once and consumes equivalent facts; a source-scope lexical tripwire counts exactly one direct `FragmentView::validate` invocation in each callable body | manual control skips, aliases, duplicates, or differs on failures/output |
| per-callable codegen | retained LLVM and AArch64 assembly scans attribute allocator, memcpy/memmove, staging, panic/unwind, direct residual writer/validator calls, LLVM indirect calls, and `blr` calls to each callable | whole-binary totals or a zero-cost/erasure statement |

The actual-rlib E0308 fixture imports `EntityId` and `TypeId` from `nudox_ir_vocab` and calls
`PreparedFragment::prepare(&[TypeId::new(7)], &[TypeId::new(3)])`. Its legal mutant changes only the
first spelling to `EntityId`. The E0451 fixture separately names `entities`, `types`, `entity_count`,
`type_count`, and `output_len`, and retains one private-field diagnostic for each. The
lifetime source declares a prepared value outside a nested scope holding the typed arrays, then uses
`output_len` after that scope; retain the raw diagnostic and report `E0597` only if the toolchain emits
it. The pre-existing `PreparedFragment` E0432 absence fixture is replaced, not retained.

All actual-rlib child sources are explicit byte constants embedded only in
`actual_rlib_writer_surface_is_typed_private_and_borrowing` in `tests/fragment.rs`; no fixture file or
testkit path is added. Its child command is exactly
`<RUSTC> --edition 2024 --crate-type lib --emit=metadata=- -L <current_exe_parent> --extern nudox_ir_format=<one-format-rlib> --extern nudox_ir_vocab=<one-vocab-rlib> -`.
It records child status, null stdout, captured stderr, both explicit rlib paths/cardinality/SHA-256, and
the exact coded-primary predicate. The source-scope fixture is one of those embedded children.

## Consumer and codegen custody

`tests/prepared_consumer.rs` defines `prepared_whole_consumer` and `manual_single_pass_control` with
the same signature `fn(&[EntityId], &[TypeId], &mut [u8]) -> Result<usize, ConsumerError>`. Their outer
test black-boxes equal typed inputs, outputs, and results. Both count entities then types, preflight the
complete output, direct-write the same bytes, call `FragmentView::validate` exactly once, consume both
cursors using iterator consumption, and return the same count/error/output state for all 0/1/2,
entity/type/both preparation rejection, and exact output-capacity rejection cases. That same named
test source-slices each callable body and asserts exactly one direct `FragmentView::validate` call;
aliases, helper indirection, and discarded second validations are test failures rather than inferred
from equivalent output.

Use a fresh target for the release test executable:

```text
CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-release RUSTC_WRAPPER= cargo rustc --locked --manifest-path workspace2/domains/ir/Cargo.toml --release --test prepared_consumer -- --emit=llvm-ir,asm
```

Record exact source digest, toolchain/target, produced consumer and owner-crate LLVM/assembly, callable
definition ranges, and target-aware scans. The consumer pair is built with the command above; the owner
pair is built in the same toolchain/profile with `CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-owner
RUSTC_WRAPPER= cargo rustc --locked --manifest-path workspace2/domains/ir/Cargo.toml -p nudox-ir-format
--release --lib -- --emit=llvm-ir,asm`. On AArch64 the assembly scan includes `blr`; LLVM includes
indirect `call`. Inspect both caller bodies plus the full owner definitions of
`PreparedFragment::write_into` and `FragmentView::validate` for allocator, direct canonical
memcpy/stores, hidden staging, panic/unwind, indirect calls, and residual calls. Distinguish the observed
direct four-byte/output memcpy from hidden staging. The per-callable output schema is `callable |
allocator | memcpy/memmove/stores | staging | panic/unwind | direct writer/validate | LLVM indirect call |
AArch64 blr | status`; every cell is either an exact matching line/range or `none found in callable
range`. Do not claim zero cost or erasure. If source attribution or comparability fails, mark the codegen
row `UNVERIFIED`.

Generated LLVM/assembly is not human-authored receipt LOC. Compress each artifact with exactly
`gzip -n -9 -c <raw> > <named>.gz`; record raw and compressed SHA-256 and byte lengths, toolchain, and
the replay command `gzip -n -d -c <named>.gz | shasum -a 256` plus a byte-count comparison. The active
generated-artifact inventory is exactly the four `.gz` paths above; no raw generated text is retained in
the active evidence tree. Commit `1e12d696` and its uncompressed artifacts remain the rejected
evidence-packaging counterexample in history, not active custody.

## Layout, budgets, and gates

On the frozen Rust 1.97.1 `aarch64-apple-darwin` host only, record—not generalize—
`PreparedFragment<'static>` size/alignment `48/8` and `FragmentView<'static>` `48/8`. Another target
records an observation as `UNVERIFIED`; it cannot satisfy this target row.

| file | baseline | expected total | total-file cap | net-delta cap | unused reserve |
| --- | ---: | ---: | ---: | ---: | ---: |
| `src/lib.rs` | 146 | 300 | 340 | 194 | 40 |
| `tests/fragment.rs` | 322 | 480 | 540 | 218 | 60 |
| `tests/prepared_consumer.rs` | 0 | 140 | 170 | 170 | 30 |
| card/skeleton executable-control custody | 0 | 1,000 | 1,100 | 1,100 | 100 |
| calibration/mutant/codegen/closure human-authored receipts | 0 | 340 | 420 | 420 | 80 |
| deterministic generated LLVM/assembly `.gz` artifacts | 0 bytes | 43,153 bytes observed | 65,536 bytes | 65,536 bytes | 22,383 bytes |

The phase production net cap is 194 and test net cap is 388. Human-authored evidence and deterministic
generated artifacts are separate, explicitly capped classes: the 420-line cap covers card/skeleton,
control, calibration, mutation, codegen, and closure prose; the 65,536-byte cap covers exactly four gzip
artifacts. The generated cap is derived before the recard from same-toolchain/profile probes: consumer
LLVM+assembly `26,417 + 12,212 = 38,629` bytes and owner LLVM+assembly `3,065 + 1,459 = 4,524` bytes,
for 43,153 observed compressed bytes and 22,383 protected bytes of reserve. Reserves are not rounding.
Any unplanned public item, missing replay/hash custody, either class cap breach, manifest change, new
owner, non-comparable consumer, or failed mutant stops the build.

Run each clean gate with a distinct fresh target:

```text
CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-gate-one RUSTC_WRAPPER= cargo fmt --manifest-path workspace2/domains/ir/Cargo.toml --all -- --check
CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-gate-one RUSTC_WRAPPER= cargo test --locked --manifest-path workspace2/domains/ir/Cargo.toml --workspace --all-targets -- --nocapture
CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-gate-one RUSTC_WRAPPER= cargo clippy --locked --manifest-path workspace2/domains/ir/Cargo.toml --workspace --all-targets -- -D warnings
git diff --check
git status --short
CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-gate-two RUSTC_WRAPPER= cargo fmt --manifest-path workspace2/domains/ir/Cargo.toml --all -- --check
CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-gate-two RUSTC_WRAPPER= cargo test --locked --manifest-path workspace2/domains/ir/Cargo.toml --workspace --all-targets -- --nocapture
CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-gate-two RUSTC_WRAPPER= cargo clippy --locked --manifest-path workspace2/domains/ir/Cargo.toml --workspace --all-targets -- -D warnings
git diff --check
git status --short
```

## Closure matrix

| phase | required custody | authority effect |
| --- | --- | --- |
| executable control | committed control, fixed-array tests, and control run receipt | feasibility only; no production authority |
| fresh calibration | two Luna readers, one Luna misreader, one Terra reviewer, all explicit non-inheriting | authorizes only hostile pre-edit review when clear |
| hostile pre-edit | separate explicit Terra review of the frozen card/control | authorizes one Luna exact-path builder only when clear |
| implementation/postbuild | Luna checkpoint, manager reproduction, separate Terra postbuild | authorizes mutant/codegen/closure review only when clear |
| closure | separate Terra closure review and two clean gates | permits exactly the isolated-prototype verdict |

## Exact next decision

After calibration, hostile pre-edit review, implementation, postbuild Terra review, closure Terra review,
and two clean gates, return exactly one isolated-prototype verdict: `RETAIN BASELINE`,
`REJECT PROTOTYPE`, or `PROMOTE FOR FUTURE INTEGRATION REVIEW`. No merge, self-score, product claim,
or C2 decision follows.
