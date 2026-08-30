# P5 C1 type-DAG lane card

Status: REJECTED AS COMBINED SLICE. The normally formatted direct specimen at `41fbb3ff` compiled but
required 389 production lines against this card's 240-line ceiling. Commit `cda6a396` removes it from
the active tree. The active replacement is `P5_C1_TYPE_DAG_READ_CARD.md`; readers must not merge these
contracts.

## Capability and terminal

This card owns only a separately encoded `TypeDag` section in `nudox-ir-format`.
It is a padding-free canonical dense `TypeId` table for primitive, reference,
and ordered-product shapes. A caller provides fixed preparation scratch and
output; validation uses caller-provided fixed typed scratch and creates the one
proof consumed by a borrowed `ExactSizeIterator + FusedIterator` cursor.

`FragmentView` and `PreparedFragment` are the retained control: their four-byte
`0xc1, 1, entity_count, type_count` header and two u32 lanes must remain exact
and unedited. The TypeDag section is not a fragment third lane.

The public terminal is a cross-crate caller that constructs
`TypeNode::Reference(TypeId::new(0))`, prepares exact bytes into its own
output, validates them once with its own typed scratch, and consumes the cursor.
The same actual-rlib source with `EntityId` instead of `TypeId` must fail.

## Baseline and allowed paths

Baseline: `6bfb0cf4b02d22f596b5fb659194d5ef5345800b`, branch
`codex/prototype-real-compiler-ir`, clean before this card.

| path | LOC | SHA-256 or state | disposition |
|---|---:|---|---|
| `workspace2/domains/ir/Cargo.toml` | 4 | `ebda83dc3ec8c97ce407a3b26c6fba3d5cd45cea1403b35c15fe4f61b44afb38` | unchanged |
| `workspace2/domains/ir/Cargo.lock` | n/a | `784e273c39b4e56de4c8f4eaffff7ea525abeadcd094e7cd4fceb0bac3c04bc7` | unchanged |
| `workspace2/domains/ir/crates/nudox-ir-vocab/src/lib.rs` | 34 | `2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78` | unchanged identity authority |
| `workspace2/domains/ir/crates/nudox-ir-format/Cargo.toml` | 12 | `afeb037e49f08d479c39a866e06ec6ca8ffbbaeace5a52978682615bfcc1cb9c` | unchanged |
| `workspace2/domains/ir/crates/nudox-ir-format/src/lib.rs` | 296 | `7e31c1b17c52c0d077300d6820f85267fce1ba684ee22cbda2084403987ad5cc` | private module declaration and named reexports only |
| `workspace2/domains/ir/crates/nudox-ir-format/tests/fragment.rs` | 526 | `b75d5fd6d683b9d127d770a781959cdf50df7d45b92b9350353dafb5fda28f1a` | actual-rlib fixture addition only |
| `workspace2/domains/ir/crates/nudox-ir-format/tests/prepared_consumer.rs` | 162 | `36c694eafe94a29662688d1b385c3f23dca86f2094f069c4e9a4e0e117e29837` | unchanged retained consumer |
| `workspace2/domains/ir/crates/nudox-ir-format/src/type_dag.rs` | 0 | absent | new sole production module |
| `workspace2/domains/ir/crates/nudox-ir-format/tests/type_dag.rs` | 0 | absent | new public runtime/mutation test |
| `workspace2/evidence/p5-c1/type-dag/**` | 0 | absent | manager evidence only |

No manifest, lockfile, vocabulary, dependency, feature, macro, unsafe,
`alloc`, `dyn`, `serde`, `Vec`, `Box`, `String`, `Arc`, atom, list, external
reference, fragment-header, frontend, recipe, stage, scheduler, publication,
or test-support crate edit is allowed. `EntityId` and `TypeId` stay imported
only from `nudox_ir_vocab`; `nudox-ir-format` must not reexport them.

## Exact wire and recursion policy

```text
byte 0: 0xc2 TypeDag magic
byte 1: schema 1
byte 2: node count, inclusive 0..=2
byte 3: exact variable node-body byte count
body: node-count consecutive records; no offset, alignment, descriptor,
      padding, or trailing byte
primitive: tag 0, primitive code (two bytes); bool=0, i32=1
reference: tag 1, target TypeId coordinate as u8 (two bytes)
product: tag 2, left TypeId coordinate, right TypeId coordinate (three bytes)
```

Entry ordinal `i` is canonical `TypeId::new(i as u32)`; node ordering and
product side ordering are significant. No sorting, hash-consing, deduplication,
or interning belongs here. Wire u8 target values widen only after target bounds
validation into `TypeId` values.

`Reference` and `Product` links may target backward, forward, or their own
ordinal: self-reference is legal and required. The recursive edges are
non-owning coordinates; the caller node table and its scratch have no owner
cycle. Therefore a cycle error is forbidden and an out-of-range target is the
exact `Edge` error. Cycle detection and interning are future cards.

Validation priority is: short header; magic; schema; node count; complete
declared geometry; ordinal tag/record-width; primitive code; first bad edge in
node/left/right order; trailing node bytes. Preparation uses the same target
bound before it can write its scratch proof or output.

## Public inventory and ownership proof

```rust
pub const TYPE_DAG_HEADER_BYTES: usize = 4;
pub const TYPE_DAG_MAGIC: u8 = 0xc2;
pub const TYPE_DAG_SCHEMA: u8 = 1;
pub const MAX_TYPE_DAG_NODES: u8 = 2;
pub enum PrimitiveType { Bool, I32 }
pub enum TypeNode {
    Primitive(PrimitiveType),
    Reference(nudox_ir_vocab::TypeId),
    Product { left: nudox_ir_vocab::TypeId, right: nudox_ir_vocab::TypeId },
}
pub enum TypeDagPrepareError {
    NodeCount { actual: usize },
    ScratchTooSmall { required: usize, available: usize },
    Edge { source: nudox_ir_vocab::TypeId, target: nudox_ir_vocab::TypeId, node_count: u8 },
}
pub enum TypeDagWriteError { OutputTooSmall { required: usize, available: usize } }
pub enum TypeDagError {
    TruncatedEnvelope { actual: usize }, Magic { actual: u8 }, Schema { actual: u8 },
    NodeCount { actual: u8 }, Geometry { expected: usize, actual: usize },
    TruncatedNode { ordinal: nudox_ir_vocab::TypeId, required: usize, actual: usize },
    Tag { ordinal: nudox_ir_vocab::TypeId, actual: u8 },
    Primitive { ordinal: nudox_ir_vocab::TypeId, actual: u8 },
    Edge { source: nudox_ir_vocab::TypeId, target: nudox_ir_vocab::TypeId, node_count: u8 },
    NodeBytes { expected: usize, actual: usize },
    ScratchTooSmall { required: usize, available: usize },
}
pub struct PreparedTypeDag<'facts> { /* private borrowed nodes and length */ }
pub struct TypeDagView<'bytes, 'scratch> { /* private input and proof */ }
pub struct TypeNodeCursor<'scratch> { /* private remaining typed proof */ }
impl<'facts> PreparedTypeDag<'facts> {
    pub fn prepare(nodes: &'facts [TypeNode], scratch: &mut [u8])
        -> Result<Self, TypeDagPrepareError>;
    pub fn output_len(&self) -> usize;
    pub fn write_into<'output>(self, output: &'output mut [u8])
        -> Result<&'output [u8], TypeDagWriteError>;
}
impl<'bytes, 'scratch> TypeDagView<'bytes, 'scratch> {
    pub fn validate(bytes: &'bytes [u8], scratch: &'scratch mut [TypeNode])
        -> Result<Self, TypeDagError>;
    pub fn nodes(&self) -> TypeNodeCursor<'scratch>;
    pub fn input_len(&self) -> usize;
}
impl AsRef<[u8]> for TypeDagView<'_, '_> {}
impl<'scratch> Iterator for TypeNodeCursor<'scratch> { type Item = &'scratch TypeNode; }
impl ExactSizeIterator for TypeNodeCursor<'_> {}
impl core::iter::FusedIterator for TypeNodeCursor<'_> {}
```

`TypeNodeCursor` is deliberately distinct from the retained fragment `TypeCursor`; changing or
shadowing the control cursor is forbidden. `BodyBytes` is not a public preparation error: with at
most two nodes and at most three bytes per record, the prepared body is at most six bytes and the
conversion to its one-byte wire cell is infallible after the node-count check.

Preparation validates all cardinality, scratch, edge, and body-byte facts
before writing one body-width byte per node into caller scratch. It borrows
only nodes. `write_into` checks full capacity before byte zero, then writes
only the returned exact prefix. Short output changes no output byte.

Validation decodes every record exactly once into the caller `&mut [TypeNode]`
scratch and returns a private pairing of that initialized prefix with input.
`nodes()` is only a slice iterator over that typed proof: no raw tag/coordinate
decode, fallible conversion, allocation, omission, or revalidation may occur
in `next`.

## Falsifiers and measurements

| law | executable evidence | hard failure |
|---|---|---|
| retained control | existing fragment/prepared goldens plus source inventory | changed `0xc1` byte/API or third lane |
| wire/recursion | empty, primitive, self-reference, forward/back product goldens | padding/trailer, wrong count, rejected legal recursion |
| bad references | source and wire mutations for every target coordinate | accepted out-of-range `TypeId` or wrong first edge |
| one proof/cursor | cursor returns `&TypeNode`, reports exact len, and fuses; source tripwire excludes raw decode in `next` | second decode, early `None`, or nonfusion |
| scratch/output atomicity | every insufficient scratch and every short output sentinel case | partial scratch/output or owned fallback |
| pointer containment | `AsRef` lies in input; returned node references lie in supplied typed scratch | copied/owned backing or dangling range |
| malformed geometry | every golden prefix, tag/code, short record, declared body mismatch, trailer | wrong exact variant/operands/priority |
| static kind boundary | actual fresh rlib fixture gets exactly one E0308 for `EntityId` in `Reference`; only replacing it with `TypeId` compiles and falsifies the predicate | shadow/same-crate/noncausal fixture |
| input use | type-DAG whole consumer distinguishes primitive and self-reference bytes; temporary constant-body and input-removal mutants make it fail | input-free implementation still green |
| charged claims | raw record names exact bytes, retained/live caller scratch, zero owned allocation, scalar emit copy, one prepare/write/validate scan, and named release control | whole-product/performance/platform claim |

Measure host `size_of`/alignment for public scratch/view values, but treat only
wire bytes as portable. Production owns no allocation or output; caller live
state is nodes, one u8 preparation scratch byte per node, exact output, and
one `TypeNode` validation scratch entry per node. Miri, other hosts, fuzz, and
fragment integration are `UNVERIFIED`.

## Budgets, gates, and negative space

| file | forecast | ceiling | unused reserve |
|---|---:|---:|---:|
| `src/type_dag.rs` | 190 | 240 | 50 |
| `src/lib.rs` addition | 8 | 20 | 12 |
| `tests/type_dag.rs` | 250 | 330 | 80 |
| `tests/fragment.rs` rlib addition | 28 | 50 | 22 |
| evidence/commands/mutants | 140 | 210 | 70 |

Run focused tests, retained domain tests, format, Clippy denied warnings,
actual-rlib fixture, inventory tripwires, temporary constant-body/input-removal
mutants, named release consumer/manual control inspection, diff check, and two
clean distinct fresh-target gates. Stop and re-card before another wire version,
width, generic, allocation, interning/cycle-analysis policy, or public API.

Rejected before implementation: extending `0xc1`/third lane (control break);
fixed records with spare cells (not padding-free); sorting/hash-consing (new
identity/work policy); strict cycle rejection (fails recursive terminal);
`Vec`/`Box`/self-referential owner (caller scratch suffices); and macro,
serde, unsafe, or generic machinery (no earned consumer/cost win).

## Next decision

After fresh Luna calibration and a separate fresh read-only Terra pre-edit
review clear this exact digest, authorize one Luna builder checkpoint for this
card only. No production edit is authorized before that calibration.
