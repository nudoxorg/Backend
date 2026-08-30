# C1-FORMAT — calibrated two-lane immutable fragment envelope

## Capability and custody

The only C1 terminal is `tests/fragment.rs::golden_fragment_lends_two_typed_lanes_from_caller_bytes`.
It validates one static caller-owned fragment, yields exact `EntityId` and `TypeId` sequences, and
proves both typed lane borrows lie inside the same supplied bytes. It owns neither those bytes nor an
allocation.

```text
immutable source baseline: 527de5bbc37e3157ad254a401aeecd70d1910c0c
post-ceiling card checkpoint: this card commit
worktree: /private/tmp/nudox-prototype-real-compiler-ir
branch: codex/prototype-real-compiler-ir
coordinate authority: unchanged nudox-ir-vocab::{EntityId, TypeId}
wire authority: new nudox-ir-format/src/lib.rs only
```

Allowed paths: `workspace2/domains/ir/{Cargo.toml,Cargo.lock}`, new
`workspace2/domains/ir/crates/nudox-ir-format/{Cargo.toml,src/lib.rs,tests/fragment.rs}`, and
`workspace2/evidence/p5-c1/format/**`. C0 vocabulary is read-only. No compiler/root-registry path,
dependency other than local vocab, ID reexport, unsafe, serde, macro, dyn, boxed/owned entity data,
builder, prepared output, atom/list/type/external-ref lane, frontend, recipe, stage, scheduler,
sandbox, publication, testkit, process JSON, or third lane is authorized.

## Exact bytes and public surface

```text
0..4    magic b"NIRF"       fixed word
4       schema 1
5       entity tag 1
6..10   entity BE start
10..14  entity BE count
14      type tag 2
15..19  type BE start
19..23  type BE count
23..    entity records: count × BE u32
...     type records: count × BE u32
```

Entity starts at 23; type starts at entity end; final type end equals input end. This compact chain
rejects header alias, every gap, overlap, reversed range, truncated body, and trailing byte. Tests use
one immutable 35-byte golden (`EntityId` [7, 11], `TypeId` [3]) plus separate static zero/one entity
goldens; no test encoder exists.

Only these public items are allowed: closed `FragmentLane::{Entity, Type}`, closed `FragmentError`,
`FragmentView::{validate, entities, types}`, `EntityLane::iter`, `TypeLane::iter`, `AsRef<[u8]>` for
both lanes, and typed `Iterator + ExactSizeIterator` cursors. Tests import IDs from `nudox_ir_vocab`;
format never reexports them. All fields/constructors are private. `FragmentView` has one private
input borrow plus private proved ranges; lanes are derived borrows; cursors retain only private
`ChunksExact<u8>` plus a proved remaining count.

`validate` is the sole raw-to-closed conversion. New format types have no public raw constructor,
`From`, or `TryFrom`. `MAGIC_WORD_BYTES` names every fixed `[u8; MAGIC_WORD_BYTES]` decode buffer;
all other numbers are named wire constants, enum tags, count-derived widths, or immutable golden bytes.

```text
InputTooShort { available: usize }
Magic { observed: [u8; MAGIC_WORD_BYTES] }
Schema { observed: u8 }
DescriptorTag { lane: FragmentLane, observed: u8 }
LaneByteLengthOverflow { lane: FragmentLane, count: u32 }
LaneStart { lane: FragmentLane, expected: u32, observed: u32 }
LaneEnd { lane: FragmentLane, expected: u32, available: usize }
FinalEnd { expected: u32, available: usize }
```

## Literal validator decision table

This is the only first-error authority. Each corpus row names an input mutation and its first failed
decision-table row. A private test helper derives the expected typed operands from named golden
descriptor facts; it is not a second parser or hand-maintained priority table.

| Row | Decision | Error if false |
| ---: | --- | --- |
| 1 | input has `HEADER_BYTES` | `InputTooShort` |
| 2 | magic equals `MAGIC` | `Magic` |
| 3 | schema equals `SCHEMA` | `Schema` |
| 4 | entity tag is exact | `DescriptorTag(Entity)` |
| 5 | type tag is exact | `DescriptorTag(Type)` |
| 6 | entity count width checks; entity start is `HEADER_BYTES` | `LaneByteLengthOverflow` / `LaneStart(Entity)` |
| 7 | entity end is in input | `LaneEnd(Entity)` |
| 8 | type count width checks | `LaneByteLengthOverflow` |
| 9 | type end is in input | `LaneEnd(Type)` |
| 10 | type start equals proved entity end | `LaneStart(Type)` |
| 11 | proved type end equals input length | `FinalEnd` |

Entity-count-three reaches row 10: `LaneStart(Type, expected: 35, observed: 31)`. Type-start-35
reaches row 9: `LaneEnd(Type, expected: 39, available: 35)`. The executable corpus covers every
prefix (0..22 row 1, 23..30 row 7, 31..34 row 9), magic/schema, both tags and reorder, header alias,
gap/overlap/reverse starts, count 0/3/MAX, and one trailing byte. Each case has
an adjacent legal control and derives expected result from its named decision-table row.

## Negative API and ownership evidence

The ordinary public integration test resolves exactly one fresh `libnudox_ir_format-*.rlib` beside the
current executable, rejects zero/multiple, and returns a typed selected-artifact result carrying the
path and hash. It passes that result through `--extern`; the child invocation explicitly uses
`--emit=metadata=-` and binds stdout to null. It retains fixture source hash, status, stderr, compiler
version, selected artifact path/hash, and cardinality.
A forged private view literal must fail with E0451 and `bytes`, `entity`, `ty`; `[u8; HEADER_BYTES].into()`
must fail with sole E0277; legal named `FragmentView::validate` succeeds; changing only the raw
conversion fixture to named validation makes the E0277 predicate false. Shadow/same-crate fixtures,
stale rlibs, unused constants, binary metadata, or a conversion trait are blockers.

The crate is `#![no_std]` with no alloc. Source/dependency tripwires, pointer containment, and no
owner prove no hidden copied backing. Legal 0/1/2 goldens assert exact `len`, `size_hint`, items, and
fused `None`; `next` cannot access header/ranges/validate, proved by the private representation and
literal source scan, never test-only instrumentation.

## Skeleton, budget, and gates

Fresh formatted skeletons: `skeleton/v4-lib.rs`, `skeleton/v4-fragment.rs`, `skeleton/v5-mutant-corpus.rs`,
`skeleton/v5-rlib-custody.rs`, `skeleton/v5-Cargo.toml`, and `skeleton/v5-workspace-Cargo.toml`. The
mutant skeleton directly mutates the canonical golden, calls public `validate`, compares exact closed
errors, and retains legal controls for the two former priority conflicts. The custody skeleton fixes
each downstream fixture source, one-rlib resolver, compiler process shape, and forbidden public-name
fixture. Pre-edit tripwire inventory must be zero for panic, source-drop,
ambiguous conversion, sentinel arithmetic, dyn/Box/Vec/Arc/Rc, public tuple field, unit namespace,
public local trait, one-letter generic, test discard, unsafe/SIMD/allocator/dependency, and public item
without a consumer/falsifier. Named wire words are accepted; golden byte positions are the sole raw
byte literals.

| Path | Estimate | Ceiling | Reserve |
| --- | ---: | ---: | ---: |
| new format manifest | 10 | 18 | 8 |
| `src/lib.rs` | 220 | 290 | 70 |
| `tests/fragment.rs` | 235 | 305 | 70 |
| workspace manifest/lock | 8 | 15 | 7 |

LOC is `wc -l` after `cargo fmt`, including docs and fixture helpers. Retained/live bytes are only
caller input; allocation/copy after setup is zero; validation is fixed header/two descriptors; cursor
work is one four-byte decode/item. Report stack size/alignment. Latency/release text are UNVERIFIED.

Run fmt, locked workspace tests, locked Clippy `-D warnings`, `git diff --check`, and status from two
fresh target directories. A new public item has no consumer/falsifier row and stops. The v7 evidence
repair is represented by `skeleton/v7-lib.rs`, `skeleton/v7-fragment.rs`,
`skeleton/v7-rlib-custody.rs`, and `v7-control-reproduction.md`: its mutation deck uses fixed arrays,
and its actual-rlib helper/test has one compiling signature. After closure, the only next decision is
whether this envelope merits a separate prepared-output builder card.
