# compiler-ir

`compiler-ir` is the single semantic representation shared by compiler
frontends, renderers, graph/index adapters, and IR-VCS.

## Representation

- Dense typed `u32` IDs keep entity, type, atom, external, link, and typed-list
  coordinate spaces statically distinct.
- Entities are true structure-of-arrays column families rather than a `Vec` of
  structs. Name/kind/visibility/parent/type scans touch only their respective
  lanes. Variable-length members, attributes, documentation, tuple elements,
  object members, template pieces, and generic parameters live in contiguous
  hash-consed list arenas.
- Optional dense coordinates reserve `u32::MAX` as an absence niche, so parent,
  type, and source-file lanes cost four bytes per entity instead of eight.
  Large optional extensions use a four-byte entity-aligned ordinal lane into a
  dense side arena; TypeScript is the first such extension.
- Types are hash-consed directly into their final eight-byte `TypeHeader`
  directory; there is no temporary `TypeExpr` arena and no finish-time
  transposition. Common unary and leaf nodes are entirely inline. Multi-operand
  nodes use exact-arity 8/12/16-byte cold lanes instead of a padded twenty-byte
  catch-all. `GuardedType<State>` is a discriminator-free transparent term and
  `TypedTypeId<State>` preserves the concrete/computed/unknown proof in a
  four-byte coordinate.
- Atoms are arbitrary bytes. Only documentation uses proof-carrying `TextId`
  values and therefore pays UTF-8 validation.
- Local and external graph edges share columnar 4/8/1/1-byte hot lanes. Optional
  source spans use a four-byte sparse ordinal rather than widening every edge.
  Forward and reverse CSR indices make neighbor traversal allocation-free;
  kind posting lists make Trustfall root/type coercion cheap.
- Runtime `EntityId` values are deliberately generation-local. Stable entity
  IDs and payload hashes are cold aligned columns used for canonical VCS order,
  binary lookup, structural diffs, and stable external link identity.
- Raw-byte name, item-kind, stable-entity, stable-link, and both graph directions
  are frozen once as canonical indices. Exact lookup, Trustfall, graph, and VCS
  never sort or rebuild them in a view.

No semantic row contains `Box`, `String`, `Vec`, `Arc`, or a trait object.
Ownership exists only at the arena/image level. Entity/version lanes, graph
lanes, atom bytes/directories, and all frozen ordering/CSR indices use audited
typed slabs, so each column family has one allocation without becoming AoS.

The literal `Infallible`-guarded enum still retains a discriminator on the
workspace's stable Rust toolchain. Instead, the construction boundary takes the
stronger stable form: `GuardedType<State>` is `repr(transparent)` over
`State::Node`, has private representation, and is compile-time/ABI-proven to be
exactly the selected node size. The heterogeneous stored arena remains tagged
because arbitrary types coexist and must be traversed at runtime. We do not
depend on nightly specialization, custom `FnOnce`, unsafe same-type casts, or
`#[inline(always)]` for that proof.

## Building

Frontends reserve an entity range before lowering types. This admits forward
and recursive nominal references without placeholders or self-referential
owners. They then submit their original slice-backed tree through
`TreeBuilder::commit`; condensation copies each distinct atom/list once and
hash-conses semantic types.

Frontends whose native AST is not already a `TreeItemInput` slice implement
`FrontendTree`. Its re-iterable `impl Iterator` methods are statically
dispatched, so the frontend can synthesize one borrowed row on the stack at a
time. The 120-byte compatibility rows are never collected, while the cheap
first pass still gives every arena an exact reservation.

The `compiler_driver::compile_ir` terminal returns this representation directly.
New pipelines should pass `&Ir` or `ItemView`/`LinkIter` views onward. The older
fragment byte API remains temporarily for durable-publication compatibility and
must not be used as an intermediate by new semantic, graph, rendering, or VCS
code.

`Ir::storage_columns()` exposes the actual typed backing slices for atoms,
lists, entity/source/language columns, cold semantic-authority facts, graph
CSR, render data, and VCS order. `Ir::image_provenance()` retains a compiled
source/recipe/package-scope header when one authority transaction built the
image; manually assembled images state `Unavailable` explicitly.
It is not a second wire schema. `IrVectorColumn` in `server-index-graph-vector`
adds model coordinates through the same `EntityId`-aligned ordinal pattern and
queries them without constructing point or segment rows. `embedding_text()`
streams signatures, docs, and typed graph context directly into any
`fmt::Write` tokenizer without an intermediate string.

The Qdrant adapter accepts `IrVectorColumn` directly as well. It does not build
`VectorFact`, `VectorPoint`, or segment projections; JSON exists only at the
unavoidable out-of-process HTTP boundary.

## TypeScript

`TypeExpr` makes evaluation state explicit:

- `ConcreteType` contains already-known shapes, including literal, tuple,
  structural object, function, nominal, union, and intersection forms.
- `ComputedType` contains type-level programs: `keyof`, `typeof`, indexed
  access, conditional/distributive types, mapped types with independent
  modifier algebra, `infer`, template literals, import types, and `Awaited`.
- `PropertyKey::Computed(TypeId)` cannot be confused with a static byte name.
- Function parameters use tuple-element rows, retaining names, optionality,
  and rest position. TypeScript `any`, checked `unknown`, `void`, `number`,
  `bigint`, `symbol`, `unique symbol`, `null`, and `undefined` remain distinct
  primitives instead of being collapsed into language-neutral approximations.
- `TypeScriptFacts` retains declared and computed lanes independently, so an
  evaluator cannot overwrite what the author wrote.

All variants render through `fmt::Display` wrappers without callback resolvers
or intermediate strings.

## VCS

`Snapshot` is a generation ID plus `&Ir`. `Diff::between` merge-walks the two
existing stable entity/link indices and yields borrowed changes. There is no
model-to-wire lowering, archive payload, deserialization, or raise step. If a
snapshot must be persisted, storage should page the canonical columnar image;
it must not introduce another semantic schema.
