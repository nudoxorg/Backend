# Graph Architecture: IR → TerminusDB

How the compiler IR becomes the terminus graph, why the previous mapping was
not graph-native, and the redesigned model that is. The implementation lives in
`workspace/compiler/graph/` (`model.rs`, `link.rs`, `from_ir.rs`); the query
surface it serves is `workspace/runtime/graph/`.

## 1. The relationship today (pre-redesign)

The IR (`ir` crate) is already graph-shaped at the symbol level:

- a flat index (`Index.entries_by_path: HashMap<NudoxPath, Entry>`) — nodes;
- symbol→symbol references by absolute path (`members`,
  `implemented_protocols: Option<Vec<NudoxPath>>`) — edges;
- structural payload (signatures, fields, generics) nested inline — node data.

Both graph projections discard that shape at the door:

| | legacy (`source/document` + `schema.json`) | workspace (`compiler/graph/model.rs`, pre-redesign) |
|---|---|---|
| symbol→symbol edges | `Set of xsd:string` URIs | `BTreeSet<String>` paths |
| structural payload | opaque `sys:JSON` blobs | random-keyed subdocument tree |
| type usage (`TypeReference`) | inside the JSON blob | `identifier: String` — a name, not a link |
| body references (`ResolvedReference`) | dropped | dropped |
| trait impls | strings in payload | `TraitRef{name}` strings in payload |

The consequences, all observable in `source/server`:

1. **TerminusDB holds zero real edges.** A string set is not a link; the DB
   cannot traverse it, enforce it, index it, or expose it in GraphQL/WOQL.
2. **Traversal is client-side.** `/expand` (`source/server/src/search/graph.rs`)
   is a BFS of up to breadth^depth separate `get_document(unfold=true)`
   round-trips, discovering "edges" by sniffing JSON for URI-looking strings
   and labelling them with whatever JSON key they sat under.
3. **The densest relations never reach the DB.** Type usage in signatures
   (the "what takes/returns/mentions `T`" queries) is a name string inside a
   blob; call/use references from function bodies are parsed by tree-sitter
   into `ResolvedReference { target, span, kind }` and then dropped by the
   projection; `Function.type_links` (pre-resolved param→entry ids) likewise.
4. **The runtime's query surface is unimplementable.** Every `todo!()` in
   `workspace/runtime/graph/` — `members`, `structure`, `implemented_by`,
   `get_references`, `get_occurrences`, `are_related`, `expand`,
   `resolve_across` — needs edges the store does not have.
5. **No package/version node.** Package identity lives only inside URI
   strings, so nothing can be scoped or diffed per package version.

## 2. Design principles

**P1 — Everything you want to traverse is a link.** Symbol→symbol relations are
`TdbLazy<Symbol>` fields. In the derived schema that is a class-typed property
(a real triple to an IRI), so WOQL `triple`/`path`, GraphQL traversal, and
reverse lookups all work server-side. `EntityIDFor<T>` is *not* used for edges:
it renders as `xsd:string` in the schema — a fake link.

**P2 — Everything you only want to read stays a subdocument.** The full
structural payload (signatures, fields, generics, const exprs) remains an
inline subdocument tree under the symbol: it is the document of record for
rendering and needs fidelity, not traversal. But the tree is no longer a dead
end — every `TypeReference`/`TraitRef` leaf carries `resolves_to:
Option<TdbLazy<Symbol>>`, so a path query can walk *through* a signature and
land on a named node.

**P3 — Edges that carry attributes are reified.** TerminusDB edges cannot hold
properties, so attributed relations are small top-level nodes with two links:
`Implementation { subject, interface, via, is_blanket, is_negative }` and
`Reference { source, target, kind, span }`. They are `value_hash`-keyed —
content-addressed, so re-emission dedups for free.

**P4 — Derived adjacency is precomputed at emit time.** Alongside the precise
structure, the projection flattens each symbol's signature into direct edge
sets on the node: `mentions` (every named symbol in the declaration), and for
functions `takes`/`returns`. Neighbor expansion becomes one triple/path query
instead of a client-side crawl. Storage is cheap; query time is what we buy.

**P5 — Resolution is a first-class projection stage.** A `Linker` builds a
symbol table from the IR index (paths + aliases + unique-suffix names) and
resolves every name-string to a symbol IRI. Names that don't resolve get a
**stub node** (`resolved: false`, kind `Unresolved`) under a deterministic IRI,
so links never dangle and edges cross package boundaries: when the dependency
is later ingested under the same identity scheme, its real node upserts over /
sits beside the stub at a knowable address.

**P6 — Identity is computed, never discovered.** This preserves the invariant
the whole store fan-out is built on (`source/document/src/schema.rs`,
`source/identity`): any referencing node can compute the IRI of its referent
offline. Symbol IRIs are client-minted (`Symbol/{lang}/{package}/{fq_path}`),
which is also why `Symbol` is a single concrete class rather than a
class-per-kind hierarchy — the IRI must be derivable from a path alone, and a
stub's kind is unknown. Kind lives as an enum field; filtering by kind is one
triple.

## 3. The node & edge catalog

Top-level document classes (everything else is a subdocument):

```
Package         key: client-minted   Package/{lang}/{name}
PackageVersion  key: client-minted   PackageVersion/{lang}/{name}@{version}
Symbol          key: client-minted   Symbol/{lang}/{package}/{fq_path}
Implementation  key: value_hash      (reified subject-implements-interface)
Reference       key: value_hash      (reified source-refers-to-target, spanned)
```

### Symbol

```
Symbol {
  id: EntityIDFor<Self>            // client-minted, see P6
  uri, fq_name, name: String       // uri = {lang}/{package}/{fq_path}
  path: Vec<String>                // segments
  aliases: BTreeSet<String>        // alternate fq spellings
  symbol_id: Option<String>        // instance-salted UUIDv5; stamped by the
                                   // uploader (which knows the instance), not
                                   // the compiler — the tantivy/qdrant join key
  kind: SymbolKind                 // Module | Function | ... | Unresolved
  visibility, documentation
  resolved: bool                   // false ⇒ stub / external placeholder

  package:    TdbLazy<Package>          // every symbol is package-scoped
  member_of:  Option<TdbLazy<Symbol>>   // single containment parent
  implements: Vec<TdbLazy<Symbol>>      // traits/protocols/interfaces
  extends:    Vec<TdbLazy<Symbol>>      // supertypes / supertraits
  mentions:   Vec<TdbLazy<Symbol>>      // P4: everything named in the decl
  takes:      Vec<TdbLazy<Symbol>>      // functions: named input types
  returns:    Vec<TdbLazy<Symbol>>      // functions: named output types

  shape: Option<Shape>             // P2: full structural payload, inline
}
```

Containment is stored **child→parent** (`member_of`), not parent→members: it
is O(1) per document, insert-order friendly (parents sort before children),
and the reverse direction (`members`) is a free reverse-triple query. Storing
both directions would be redundant write amplification.

### Shape (subdocument union)

The old `KindData` minus the marker variants (the `kind` enum covers those):
`Record | Function | TraitDef | TraitImpl | Sum | Union | Alias | Info`, each
carrying the same subdocument tree as before. Two leaves change:

```
TyReference { identifier: String, resolves_to: Option<TdbLazy<Symbol>>, generic_args }
TraitRef    { name: String,       resolves_to: Option<TdbLazy<Symbol>>, args }
```

### Reified relations

```
Implementation { subject, interface: TdbLazy<Symbol>,
                 via: Option<TdbLazy<Symbol>>,       // the TraitImpl symbol
                 is_blanket, is_negative: bool }

Reference { source, target: TdbLazy<Symbol>,
            kind: ReferenceKind,                     // FunctionCall | MethodCall |
                                                     // TypeReference | VariableUse |
                                                     // MacroInvocation | FieldAccess | Import
            span_start, span_end: Option<i64> }      // byte range in source body
```

`Reference` is fed by the tree-sitter `ResolvedReference`s that the old
projection dropped — this is the call graph, and the data was already being
computed.

The direct `implements` edge and the reified `Implementation` node coexist
deliberately: the edge is for cheap path traversal, the node is for the
attributes (`via`, blanket-ness). Same information, two access costs.

## 4. The Linker (resolution pass)

Built once per `ir::Index` (`link.rs`):

1. **Exact table** — every entry's fq path, plus every alias, → IRI.
2. **Suffix table** — last path segment → IRI iff unambiguous. Resolves bare
   names like `Serialize` in signatures produced from surface syntax.
3. **Stub minting** — anything else gets
   `Symbol/{lang}/~extern/{identifier}` and a recorded stub node. `~extern` is
   a reserved pseudo-package for names whose owning package is unknown;
   `NudoxPath::External { dependency, .. }` references (where the package *is*
   known) go under the real package name instead.

Resolution is total: every name becomes an IRI, so every edge is insertable.

## 5. Emission ordering

TerminusDB enforces referential integrity per commit, and uploads are chunked
(100 docs/commit today), so an edge may not point at a document that lands in
a later chunk. Waves:

1. `Package` / `PackageVersion` nodes (no symbol links yet).
2. All `Symbol`s, **bare**: scalars + shape, edge fields empty. Order is
   irrelevant within the wave.
3. All `Symbol`s again, **full replace** with edge fields populated — every
   target now exists. (Optimization once payloads allow it: a single
   transaction per package collapses waves 2–3 into one.)
4. `Implementation` / `Reference` nodes.
5. `PackageVersion.declares` patch (see §7).

The projection emits one `GraphCorpus { package, version, symbols, implementations,
references }`; the uploader owns wave scheduling.

## 6. Query cookbook

What each `workspace/runtime/graph` stub becomes. `@X` = variable, all
predicates are real schema properties now.

| Query | WOQL shape |
|---|---|
| `members(s)` | `triple(@m, member_of, s)` |
| `structure(pkg)` | `triple(@s, package, pkg)` + `member_of` edges, one query |
| `implemented_by(t)` / who implements `t` | `triple(@x, implements, t)`; details via `Implementation` nodes |
| `get_occurrences(item)` | `triple(@s, mentions, item)` — single triple |
| `get_references(item)` | `triple(@r, target, item), isa(@r, Reference), triple(@r, source, @s)` |
| `are_related(a, b)` | `or(...)` over the edge predicates, both directions |
| `expand(origin, depth, breadth)` | `path(origin, "(member_of|mentions|implements|extends|~member_of|~mentions|~implements|~extends){1,d}", @t, @p)`; breadth capped per level by the caller |
| what takes / returns `T` | `triple(@f, takes/returns, T)` |
| signature-level walk | path *through* the shape tree via `resolves_to` triples |

Every one is a single server round-trip (expansion: one per level at worst),
replacing the b^d document fetches of `source/server/src/search/graph.rs`.
GraphQL comes for free from the same schema: class-typed properties are
traversable fields, and the fork's ORM (`{Model}Filter`/`{Model}Ordering`)
covers the filter queries.

## 7. Versioning

Symbol identity stays **version-agnostic** (stable `SymbolId` across versions
is a runtime requirement — see `resolution.rs`). Per-version facts hang off
`PackageVersion`:

- `declares: Vec<TdbLazy<Symbol>>` — membership of that version.
- `resolve_across(prev, next)` = set-diff of two `declares` sets, with the
  suffix table scoring moved/renamed candidates.

Signature *drift* within a stable symbol id (same path, changed type) is
deliberately out of scope for v1; candidates are TDB branches per version or
content-addressed `SignatureRevision` nodes (pairs naturally with the deferred
content-addressed-blob work). Do not bolt version qualifiers onto `Symbol`
IRIs — it breaks the offline-computable-identity invariant (P6).

## 8. What stays, what goes

- **Stays:** the subdocument fidelity tree; client-minted identity; the
  `Entry/{lang}/{package}/{path}` coordinate scheme (now `Symbol/...`);
  blob store as root of truth with TDB as a derived, re-emittable store.
- **Goes:** string edge sets; `sys:JSON` blobs (legacy); the Entry/Kind
  document split (legacy) — kind is a field, shape is inline; client-side BFS
  with URI sniffing; dropping body references and `type_links` on the floor.
- **Legacy `source/` stack:** untouched. It is replaced wholesale when the
  workspace stack goes live; do not retrofit.

## 9. Known risks / open questions

- **Derive coverage:** `Vec<TdbLazy<T>>` / `Option<TdbLazy<T>>` container
  impls in the vendored fork are exercised by this model; if a gap appears,
  the fallback is a small hand-impl'd `SymbolRef` newtype (schema class
  `"Symbol"`, instance `RelationValue::ExternalReference`) — ~50 lines, same
  semantics.
- **Schema-walker cycles:** `Symbol → Shape → TyReference → TdbLazy<Symbol>`
  is a class-reference cycle (legal in TDB); the derive's dedup walker must
  terminate on it. The model keeps the existing newtype-variant convention
  that makes the walker terminate on `Type`'s self-reference.
- **`PackageVersion.declares` cardinality:** one large set-valued doc for big
  packages. Acceptable as triples; if insert payloads balloon, reify as
  `Declares { version, symbol }` value_hash nodes.
- **TdbLazy is not `PartialEq`:** model types holding links derive
  `Debug + Clone` only.
