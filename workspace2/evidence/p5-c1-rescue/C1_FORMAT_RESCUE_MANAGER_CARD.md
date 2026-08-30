# C1 format rescue — fixed two-lane borrowed envelope only

## Frozen capability and custody

This card starts only after detached control `f40e269e` passed. It rescues one observable terminal:
one caller-provided byte slice is validated as a fixed, padding-free envelope, then lends two and only
two typed dense-ID cursors. It does not create bytes, prepare bytes, build entities, own scratch, or
describe a later fragment model.

```text
source baseline: 527de5bbc37e3157ad254a401aeecd70d1910c0c
control checkpoint: f40e269e196a8a19b59683888b7ebe9ba8d4fe79
allowed production paths:
  workspace2/domains/ir/Cargo.toml
  workspace2/domains/ir/Cargo.lock
  workspace2/domains/ir/crates/nudox-ir-format/Cargo.toml
  workspace2/domains/ir/crates/nudox-ir-format/src/lib.rs
  workspace2/domains/ir/crates/nudox-ir-format/tests/fragment.rs
allowed evidence path: workspace2/evidence/p5-c1-rescue/**
```

`nudox-ir-vocab::{EntityId, TypeId}` remains the unchanged identity authority. C1 production source
does not modify it, reexport either alias, or define any raw identity conversion.

Out of scope and prohibited: builder, prepared output, output buffer, type DAG, atoms, strings, lists,
external references, third lane/tag/name, frontend, compiler registry/driver, recipes, stages,
scheduler, sandbox, publication, serde, macro, unsafe, `alloc`, `dyn`, `Box`, `Vec`, `Arc`, `Rc`, test
crate, process JSON, and any global/caller ownership transfer.

## Literal wire law and validator

```text
byte 0: magic  0xc1
byte 1: schema 1
byte 2: entity count, inclusive range 0..=2
byte 3: type count, inclusive range 0..=2
byte 4..(4 + 4 * entity_count): entity lane, LE u32
remaining exact bytes: type lane, LE u32
encoded length: 4 + 4 * entity_count + 4 * type_count
```

There are no descriptor bytes, offsets, alignment cells, padding cells, or optional/unknown lanes.
The sole literal validator order is: (1) `len < 4`, (2) magic, (3) schema, (4) entity count, (5) type
count, (6) exact encoded length. The mutation corpus derives expected errors mechanically from this
order and never provides a second parser/priority table.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentError {
    TruncatedEnvelope { actual: usize },
    Magic { actual: u8 },
    Schema { actual: u8 },
    EntityCount { actual: u8 },
    TypeCount { actual: u8 },
    Geometry { expected: usize, actual: usize },
}
```

## Exact public and private inventory

Only these public types/constants/methods exist:

```rust
pub const HEADER_BYTES: usize = 4;
pub const ENTITY_BYTES: usize = 4;
pub const TYPE_BYTES: usize = 4;
pub const MAGIC: u8 = 0xc1;
pub const SCHEMA: u8 = 1;
pub const MAX_LANE_ITEMS: u8 = 2;
pub enum FragmentError { /* exactly above */ }
pub struct FragmentView<'fragment> { /* all fields private */ }
pub struct EntityCursor<'fragment> { /* all fields private */ }
pub struct TypeCursor<'fragment> { /* all fields private */ }
impl<'fragment> FragmentView<'fragment> {
    pub fn validate(envelope: &'fragment [u8]) -> Result<Self, FragmentError>;
    pub fn entity_ids(&self) -> EntityCursor<'fragment>;
    pub fn type_ids(&self) -> TypeCursor<'fragment>;
    pub fn input_len(&self) -> usize;
}
impl Iterator for EntityCursor<'_> { type Item = nudox_ir_vocab::EntityId; fn next(&mut self) -> Option<Self::Item>; fn size_hint(&self) -> (usize, Option<usize>); }
impl ExactSizeIterator for EntityCursor<'_> { fn len(&self) -> usize; }
impl core::iter::FusedIterator for EntityCursor<'_> {}
impl Iterator for TypeCursor<'_> { type Item = nudox_ir_vocab::TypeId; fn next(&mut self) -> Option<Self::Item>; fn size_hint(&self) -> (usize, Option<usize>); }
impl ExactSizeIterator for TypeCursor<'_> { fn len(&self) -> usize; }
impl core::iter::FusedIterator for TypeCursor<'_> {}
```

The only `FragmentView` fields are private correlated borrows:
`envelope: &'fragment [u8]`, `entity_lane: &'fragment [u8]`, and `type_lane: &'fragment [u8]`.
`validate` alone proves and creates them. Each cursor stores only its private checked lane; `next`
splits exactly four bytes then uses `u32::from_le_bytes` and the existing typed ID's `new` function.

No `pub use nudox_ir_vocab::{EntityId, TypeId}`, public field, raw constructor, `From`, `TryFrom`,
`AsRef`, `Deref`, builder/prepared type, or `atom*`/`list*`/`external*` API is allowed. Any raw input
must pass named `FragmentView::validate`.

## Formatted skeletons and manifest implication

`nudox-ir-format/Cargo.toml` has package metadata, edition 2024, a normal lib target, and exactly one
local dependency: `nudox-ir-vocab = { path = "../nudox-ir-vocab" }`. Its `src/lib.rs` is `#![no_std]`
and implements the inventory above in one module. `domains/ir/Cargo.toml` gains the glob-resolved crate
only; its lockfile adds no registry package. No other manifest changes are approved.

`tests/fragment.rs` contains five complete families: literal 0/1/2 golden bytes; every prefix of every
golden; one mutation of each four header cells plus one trailing geometry byte; exact-size/fused cursor
behavior at 0/1/2; and actual-rlib API fixtures. It imports IDs from `nudox_ir_vocab`, not format.
Its only shared helpers are `assert_error`, a pointer-range check, exact rlib resolver, and one compiler
process invocation helper; no encoder, builder, parser, or test support crate.

## Mandatory evidence and reviewers' tripwires

The test must assert each legal value for both lanes: `len`, `size_hint`, exact item sequence, repeated
`None`, then `next` again after fusion. It must mutate every strict truncation prefix and every header
cell; each expected error is the literal ordered table above. Pointer containment compares each private
validated lane's pointer/range against the original input allocation in a unit test. The integration
test proves only public behavior.

Allocation/copy evidence is explicitly limited: a dedicated test allocator counts allocations around
validated golden and complete cursor consumption after setup; it must record zero allocations. The
copy witness is source-level: validator creates subslices only; cursor reads four scalar bytes into a
stack array. The postbuild reviewer must inspect all consumers and report no broader "zero cost" claim.

Actual-rlib tests follow C0's proven process mechanics: select exactly one fresh
`libnudox_ir_format-*.rlib` beside `current_exe`, reject zero/multiple, call `RUSTC`/`rustc` with
explicit `--extern`, pipe source through stdin, bind stdout to `Stdio::null()`, capture stderr, and
assert child status plus exact one primary diagnostic/symbol predicates. Named independent child cases
must reject: `FragmentView` struct literal/private correlated fields; `.into()` raw conversion;
`.try_into()` raw conversion; `EntityId` import; `TypeId` import; `FragmentBuilder` import; and
`atom_ids` third-lane call. A distinct legal source calls `FragmentView::validate`. Do not bundle absent
name probes if compiler diagnostics add a second primary error.

Review stops on any: unplanned public item; public conversion; ID reexport; third lane; copied/owned
input; missing test cardinality; uncoded/coded primary cardinality mismatch; rlib selection other than
one; stdout not null; raw-byte priority helper; padding/offset field; validator error order drift;
cursor lacking exact/fused semantics; ignored allocation/copy measurement; nonlocal dependency;
formatting suppression; `unwrap`/`expect` in scenario-style tests; or scope crossing into C2.

## Budget and gates

All LOC use `wc -l` after `cargo fmt`, docs and fixtures included.

| file | forecast | ceiling | unused reserve |
|---|---:|---:|---:|
| format manifest | 12 | 20 | 8 |
| `src/lib.rs` | 150 | 210 | 60 |
| `tests/fragment.rs` | 225 | 300 | 75 |
| IR workspace manifest/lock | 10 | 18 | 8 |

The builder must stop/re-card before 60% of a per-file ceiling is consumed if the remaining forecast
cannot fit. Required gates: fresh-target `cargo fmt --check`; locked domain tests; format focused tests;
locked Clippy with warnings denied; `git diff --check`; public inventory grep; actual-rlib fixture run;
all truncation/header/geometry mutants; allocation counter; pointer containment; and two clean final
status/diff gates. Release-text, non-host platforms, Miri, and whole-product consumer performance are
`UNVERIFIED`.

The only possible terminal is an isolated prototype verdict. No merge, product completion, C2 claim,
or builder/prepared follow-on is authorized.
