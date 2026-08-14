# IR-NORTH-STAR — the elegant IR, and the infrastructure that collapses around it

> Companion to `docs/IR-MIGRATION-PLAN.md`. That doc is *how we get to parity safely*.
> This doc is *what we should actually be aiming at* — the smallest set of abstractions
> the whole system can be rebuilt on, and everything each one lets us delete. Grounded in
> the current code's real seams (cited inline). Aligns with and sharpens the directions
> already recorded in `docs/GLOBAL-IR-GRAPH.md` (content-addressed graph), the resolution/IR
> unification brief (EDB/IDB, "never add structures beside the IR"), and `SEMANTIC-IR-VCS-PLAN.md`.

## 0. The thesis in one sentence

**The IR is a content-addressed property graph — a set of Merkle *nodes* joined by an
extrinsic *relation* table — and almost everything else in the system (wire format,
registry, VCS, index, views, occurrences, bodies, semver, security) is a *projection* of
it, not a parallel structure beside it.**

> ⚠ **Rev 2 (§0.5) corrects this thesis after adversarial review.** The model as built is a
> *name-keyed property graph with per-node content checksums* — **not** a Merkle DAG;
> content-addressing lives only at the CAS boundary. Identity is **three** layers, not two;
> the `Relation` unifies only *extrinsic* edges; and the `Node<R>` GAT is cut. Read §0.5 first —
> the Collapse 3/4/5 claims below are superseded by it.

Six core abstractions. Everything else derives:

| # | Abstraction | One-line definition |
|---|-------------|---------------------|
| A1 | **`Ref`** | a type-level choice of "how a node points at another node": `Local` (build), `Hash` (canonical), `Intro` (nominal). |
| A2 | **`Node<R>`** | one parameterized declaration/structural node. The *only* model type. Merkle-hashed in `R=Hash`. |
| A3 | **`Relation`** | the *one* extrinsic edge `(from, to, kind, confidence, span)`. Subsumes every bespoke edge type. |
| A4 | **`Fact`** | what a producer emits: `Declare(node)` / `Relate(edge)`. Producers never build trees. |
| A5 | **`Store`** | `CAS(Hash→Blob) + Names(Intro→Hash) + Manifest(Pkg→{Intro})`. Subsumes registry/apply/manifest. |
| A6 | **`Query`** | datalog / Trustfall over `Node`+`Relation`. Subsumes view/reflect/occurrences/semver/security. |

Everything below is the argument for why this collapses the current ~15 modules and ~33
wire twins into ~6 planes, and what's *essential* complexity we must keep.

---

## 0.5 — Rev 2: hardened by adversarial review (2026-07-24)

Three adversaries (strong-typing, abstraction-integrity, Rust-implementability) attacked this
doc against the code. Net: **the codec / side-table / fact-emission collapses are real wins and
should ship; the "content-addressed Merkle DAG", "identity is two things", "one Relation subsumes
everything", and "GAT `Node<R>` deletes the twins nearly free" claims were elegance standing in
for correctness.** Corrected here; this section supersedes Collapse 1/3/4/5 where they conflict.

1. **VERIFIED BUG — the `IntroId` disambiguator collides distinct impls/overloads (fix first).**
   `trait_impl_skeleton(of, self_ty)` and `function_signature_skeleton(inputs, outputs)`
   (`skeleton.rs:220,244`, both "frozen") hash only trait-ref+self-type / param-types — they
   ignore `generics`, `wheres`, and impl negativity; and `Type` has no generic-application node.
   So `impl<T: Copy> Foo for Bar<T>` vs `impl<T: Clone> Foo for Bar<T>` (and `impl`/`impl !`, and
   generic-differentiated overloads, and `Foo for Bar<u32>` vs `Foo for Bar<String>`) mint the
   **same `IntroId`** → silent overwrite in `PristineIntroTable`. **Fix:** fold
   generics/wheres/negativity (or an explicit `impl_index`) into the placement key, with the
   generics subsystem. This is a real bug, not a modeling nit.

2. **NOT a Merkle DAG — corrected.** The only per-node hash (`payload_hash`, `wire.rs:641`) is
   `blake3(postcard(sym, disc, kind, flags))` where child/type leaves are nominal
   `IntroId`/`StableRef` — *names, not child hashes*. So "children are already hashes / cheap
   Merkle subtree-diffs / global structural dedup" don't describe today. Honest core: **a
   name-keyed property graph with a per-node content checksum.** Content-addressing/dedup is a
   **CAS-boundary (iroh-blobs) property**, not an in-memory-build one; it is intra-package-optional
   (and needs **SCC-hashing** for recursive types — none exists) and **inter-package strictly
   nominal** (cross-package hashes would couple dependency hashes → cyclic re-hash).

3. **Identity is THREE layers, not two.** (a) content checksum (per-entry; changes on any edit —
   a dirty-bit, not an address); (b) placement/mint key (kind + path + *full* disambiguator incl.
   generics/wheres/negativity — see #1); (c) lineage (`IntroId` continuity — **undecidable** in
   general per [[global-ir-graph-direction]]: MinHash+APTED+never-merge). `IntroId` is *minted once
   deterministically, then carried by a matcher* — not a pure function that "survives edits." Keep
   the `change/hash` Hash①/② (generation-snapshot vs CAS-blob) discipline (K12); that split is real.

4. **The GAT `Node<R>` centerpiece is CUT.** `ir-vcs` pattern-matches concrete `KindWire` variants
   (13 arms in `diff/diff.rs`) and exposes twins in public struct fields + fn signatures across 37
   files / ~1,276 references; a phase-GAT infects all of them for a distinction that's really two
   inherent-impl blocks, and the decl-macro `Visitor` **cannot** do a type-changing `map_refs`
   (it calls methods in place; can't rebuild containers or branch Same/Foreign/unresolved).
   **Corrected:** keep **two concrete types** — a rich build `Entry`/`Kind` (EntryIdx-based) and the
   frozen canon `OwnedEntryPayload`/`KindWire` — and generate seal with a **stable
   `proc-macro derive(Seal)`** over the 12 kinds (`#[seal(with=…)]` for Foreign-passthrough /
   unresolved-`Err`). Freeze `Str = Box<str>` (byte-neutral in postcard). This delivers the real
   win (delete the 12 hand-written `to_wire`s) AND gives the compile-time safety for free: the build
   `Entry` has no `Serialize`/`payload_hash`, so serializing/hashing an unsealed node won't compile.

5. **Strong typing without the GAT (adopt all).** `const DISC` + `type Body` on the kind marker; a
   checked `downcast → Result` as the *only* narrowing (kills the `unsafe` `TypedEntry` cast +
   no-op `resolve_typed`; revives dead `KindMismatch`); per-kind `KindBody` types, enum only at the
   storage/wire boundary (deletes the `unreachable!()`s and the placeholder-`Kind` lie); a **total**
   derived `KindDiscriminant↔u16` (kill `from_u16→Option` drift); a `StrId`-typed collision key
   (kills the `(u16,String,String)` NUL-join footgun); single-source `parent` + derived `children`;
   `NonZeroU16` width; private index newtypes; role-newtypes for `inputs`/`outputs`.

6. **"One Relation" → extrinsic facts only.** Constructive edges (fields/params/variants/supers/
   self-ty) stay **inline** storage — hot path *and* identity-bearing. The unified `Relation` table
   covers only the already-separate **extrinsic** facts (occurrences, calls, mentions) as
   storage+view. **Undirected links** (`LinkRecord`/`LinkDomainKey`, symmetric) and
   **attribute-facts** (`sealed`, `dyn_compat`, decorators, unresolved calls with no `to`) are
   *distinct* — they are not `(from,to,kind)` rows.

7. **Body-merge is a span-indexed overlay, not a lattice join.** `merge_body` (`body.rs:379-400`)
   keeps both tiers and joins by *span overlap* (tree-sitter and oracle see the *same* call at two
   fidelities; often no `to` to key on), and re-running the oracle can *change* a target
   (non-monotone). Keep the two-tier overlay + merge-note; don't pretend it's a max-confidence join.

8. **Queries split in two.** Recursive-cheap views (`exported`/`moniker`/`usages`) are datalog —
   ship. But semver (variance-aware subtyping) and taint (interprocedural dataflow fixpoint) are
   **algorithm-backed analyses that produce facts** for the query plane, not rule sets; and the EDB
   is **cfg-parameterized** (a *family* of graphs).

9. **Node roles — unresolved decision.** §5 mints one `IntroId` *per* overload; the Java/TS/C#/
   Python producers *fold* overloads into one node with an `overloads` array (+ TS declaration-
   merging fuses same-named decls). Irreconcilable as written. Decide: overloads = N `Decl`s
   (N intros) OR a third `Group` role (1 nominal id + ordered member list).

**Prerequisite for all of it:** golden **postcard byte** snapshots don't exist yet (only round-trip
determinism). They are the sole safety net for any wire/identity work — write them first.

---

## 1. The five (plus three) collapses

Each collapse names a concrete duplication in today's code, the abstraction that removes
it, and the honest cost.

### Collapse 1 — Parameterize over the reference type ⇒ delete the wire twins

**Today:** every in-memory type has a hand-written wire twin. `lib.rs:85-90` re-exports ~33
`*Wire` types (`KindWire`, `SymbolWire`, `TypeWire`, `FunctionWire`, `ParamWire`, …). Worse,
the builder keeps a **parallel `kind_wires: Vec<KindWire>` side table** (`builder.rs:63`)
because the in-memory `Kind` was made *deliberately lossy* (5 variants) while the truth lives
in the wire twin — so `add_trait`/`add_impl`/`add_enum` push a **placeholder** `Kind::Module`
(`builder.rs:325,351,371`) next to the real `KindWire`. Two type families, kept in sync by hand.

**The abstraction:** one model, parameterized by how references are represented.

```rust
trait Refs { type Node; type Str; }
enum Build {} impl Refs for Build { type Node = LocalIdx; type Str = StrId; }   // building
enum Canon {} impl Refs for Canon { type Node = Hash;     type Str = Box<str>; } // hashed/wire

struct Node<R: Refs> { sym: Sym<R>, kind: Kind<R> }
enum Kind<R: Refs> {
    Module,
    Record  { fields: Box<[R::Node]>, supers: Box<[Type<R>]>, .. },
    Function{ params: Box<[R::Node]>, ret: Type<R>, sig: FnSig, .. },
    Type(Type<R>),  ..
}
enum Type<R: Refs> { Nominal(R::Node), Tuple(Box<[Type<R>]>), Prim(Prim), .. }
```

`Ir<Build>` is the in-memory build form (local arena indices). `Ir<Canon>` is the
canonical/wire form (content hashes). **They are the same type.** "Seal" is one generic
transform `Node<Build> → Node<Canon>` that replaces each `LocalIdx` with the target's
already-computed `Hash`. There is **no `KindWire`, no `TypeWire`, no side table** — the wire
*is* `Ir<Canon>`, and `seal` is a fold, not 33 `to_wire` methods.

**Why it's nearly free to adopt:** nudox-ir *already ships 80% of the machinery*. Its
derive-`Visitor` (`crates/nudox-ir/src/visitor.rs`) already recursively finds every
`EntryIndex` inside any `Kind`/`Type`/`Symbol` via a TT-muncher over structs and enums.
Generalize `visit_mut(&mut Idx)` into `map_refs(Idx → Idx2)` and the same derive gives you
`seal` for *every* kind, forever, with zero per-kind code. The 33 twins become one
`#[derive(Node)]`.

**Cost:** the canonical encoding must be byte-stable (VCS wire). Handled by pinning
`Ir<Canon>`'s serialization as the versioned format (§ Codec) and golden-testing it against
today's bytes.

### Collapse 2 — Everything is a node; two node classes; structural sharing ⇒ delete the "types inline vs types-as-entries" dilemma

**Today:** there are two incompatible type models. `workspace/ir` inlines structural type
exprs with `IntroId` leaves (`kind.rs:198` `Type::Tuple(Vec<TypeRef>)`); the nudox-ir POC
made *every* subtype an arena entry (`ty.rs` `Tuple(List<EntryIdx<Type>>)`). The plan had to
carefully pick one to avoid wire churn.

**The abstraction:** there is one node space, content-addressed, with **maximal structural
sharing**, and exactly **two node classes**:

- **`Decl`** — a *named* declaration (module/record/fn/enum/trait/impl/const/type-alias).
  Carries a nominal `IntroId` (stable across edits) *and* a structural `Hash`.
- **`Struct`** — an *anonymous* structural node (a type expression, a const expression, a
  body). No nominal identity — **pure content hash, deduplicated globally.**

Under content-addressing the "arena bloat" objection to types-as-entries **evaporates**:
`(i32, String)` appearing 10,000 times is *one* `Struct` node. Nominal-vs-structural type
refs unify — both are "a reference to a node," differing only in which class they land in.
The dilemma was an artifact of not having content-addressed sharing.

**Cost:** you commit to a real CAS for structural nodes. You're building one anyway
(`docs/GLOBAL-IR-GRAPH.md`: iroh-blobs). This *is* that.

### Collapse 3 — One relation type ⇒ delete every bespoke edge structure

**Today** the graph's edges are scattered across at least five hand-rolled shapes:
`Node.children`/`parent` (`entry.rs:39`), `Record.fields`/`Function.params` (inline lists),
`vocab::Occurrence` + `ReferenceKind` + `Confidence` + `RelSpan` (`vocab.rs`),
`apply::LinkRecord` keyed by `change::domain::LinkDomainKey`, and body edges
`OracleCall`/`BodyCall`/`OracleTypeMention`/`OracleAccess` (`body.rs`). Same concept — "a
typed, located, confidence-scored edge from one symbol to another" — modeled six ways.

**The abstraction:** split edges by whether they participate in a node's **content identity**:

- **Intrinsic (defining) edges** — a record's field types, a function's signature, an enum's
  variants. These *are* the declaration; they stay **inline** and are Merkle-hashed into the
  node (change the field type → change the record's hash). This is the declaration plane (`.nir`).
- **Extrinsic (fact) edges** — calls, type-mentions, super-types-as-usage, impl-of,
  occurrences, imports, doc-links. These are *facts about* symbols, not part of their
  identity (a new call site must not change the callee's hash). These become **one relation
  table**:

```rust
struct Relation { from: Intro, to: Ref, kind: RelKind, conf: Confidence, span: Option<RelSpan> }
enum RelKind { Calls, Mentions, Implements, Extends, Overrides, Imports, RefersTo, DocLinks, .. }
```

`Occurrence`, `LinkRecord`, `OracleCall`, `BodyCall`, `type_mentions`, `reads_writes` all
become `Relation` rows with a different `RelKind`. The IR is now literally **nodes +
relations = a datalog EDB / property graph** — exactly the "EDB/IDB, never add structures
beside the IR" direction, made real.

**Cost:** none conceptually; it's a unification. Migration cost is rewriting the readers
(they get simpler).

### Collapse 4 — Two identities, crisply separated ⇒ registry becomes three maps

**Today** identity is genuinely two things but they leak into each other: `IntroId`
(nominal, stable-across-signature-change, `intro.rs` + the §4.3 disambiguator in
`builder.rs:570`) and content hashes (`payload_hash`, `CasKey`, `GenerationStamp` in
`change/hash.rs`). The registry (`registry.rs`) mixes a `RwLock<HashMap>` cache, a resolver
trait, and `PristineIntroTable` lookups.

**The abstraction:** name the two identities and let each do exactly one job.

- **Nominal `IntroId`** = a *stable name*. "This symbol was introduced here"; survives body
  and signature edits (that's what makes VCS lineage possible). Minted once by the
  disambiguator strategy.
- **Structural `Hash`** = a *version*. "This exact content." Drives CAS, dedup, incrementality.

A symbol is then just **a nominal cell whose value is a content hash over time** — a `loc` in
a store. So the registry/store is three tiny maps, nothing more:

```
Store = CAS:      Hash  → Blob        // content-addressed, immutable, shared (iroh-blobs)
      + Names:    Intro → Hash        // the mutable "current version" index (iroh-docs)
      + Manifest: Pkg   → {Intro}     // membership per generation
```

`apply::PristineIntroTable` = a materialized slice of `Names`⋈`CAS`. `manifest/*` = `Manifest`.
`registry` = lazy read-through over these three. The lazy `FrozenVec`+`OnceCell` machinery
from nudox-ir is the *implementation* of the read-through cache; the *model* is three maps.

**Cost:** the disambiguator stays (essential — see §3). Everything else shrinks.

### Collapse 5 — One canonical codec ⇒ delete the four encoders

**Today** there are ~four serializers: `wire.rs` (postcard twins), `ascii.rs` (F1 armored
text, `escape`/`encode_typeref`/`encode_typeexpr`), `serialize.rs` (`symbol_path`,
`LinkWire`, `intro_hex_of`), `body_wire.rs` (`nudox.body.v1` envelope). Each hand-maps the
model to bytes.

**The abstraction:** the canonical form *is* `Ir<Canon>`, and its serialization is **derived**
(one `#[derive]`, versioned, domain-separated). The bytes we serialize are the bytes we hash
are the identity. One codec:

- one canonical byte form (versioned tag; keep postcard/CBOR-canonical),
- `hash(node) = blake3(domain ‖ canonical_bytes(node))` — Merkle, because children are already hashes,
- text/`ascii` armoring becomes a *debug/interchange view*, not a second source of truth.

`body_wire` disappears (a body is a `Struct` node; it serializes like any node). `serialize`
helpers become methods on the model. The frozen wire is now "the canonical codec, version N."

**Cost:** you must reproduce today's exact bytes for compatibility, or bump the version and
migrate. Golden tests + a one-time re-hash pass (content-addressing makes re-hash cheap and verifiable).

### Collapse 6 — Body merge is a lattice join, not bespoke logic

**Today** `body.rs` has `merge_body`, `ConflictPolicy`, `BodyMergeNote`,
`is_forbidden_steady_state` — hand-written rules for combining tree-sitter (fast/shallow) and
oracle (slow/deep) body facts.

**The abstraction:** body facts are just `Relation`s (Collapse 3) carrying a `Confidence`
(the lattice already exists: `Syntactic < Suffix < Index < Import < Oracle`, `vocab.rs`).
Merging two producers' output is **`union`, then dedup by `(from,to,kind)` keeping `max`
confidence** — a monotone lattice join. No `ConflictPolicy`, no "forbidden steady state"
special-case; the join is total and associative by construction, and re-running a producer
can only raise confidence (monotone = trivially incremental).

**Cost:** the *two producers* remain (essential: perf/coverage trade-off). Only the merge code dies.

### Collapse 7 — Producers emit facts; structure is derived

**Today** every producer hand-builds a nested `Entry` tree keyed by `NudoxPath`, assembles a
`HashMap<NudoxPath, Entry>` + `root_ids`, and manually wires `members`/parent
(`workspace/compiler/compile/*`; e.g. `wire_members`, `entries_by_path`). ~4,900 LOC of
tree-plumbing duplicated seven ways.

**The abstraction:** a producer emits a flat stream of `Fact`s:

```rust
enum Fact { Declare { local: LocalId, kind, sym, parent: Option<LocalId> }, Relate(Relation) }
```

The store *derives* the tree (parent/children reverse-index), interns strings, content-
addresses `Struct` nodes, and mints `Intro`s. Producers stop caring about paths, arenas,
ordering, or seal. The **combinator `lower` layer** (from the migration plan) becomes sugar
over `Fact` emission: `into_entry::<K>()` emits a `Declare`; `.calls(x)` emits a `Relate`.
`NudoxPath`, `Index`, `entries_by_path`, `wire_members`, `root_ids` all vanish.

**Cost:** the *semantic* mapping (this language's "interface" → `Trait`, its receiver → `Receiver`)
is irreducible per language. But the *mechanical* mapping is written once.

### Collapse 8 — Views, reflection, semver, security are datalog IDB

**Today** `view.rs` (`IrView`), `reflect.rs` (`exported`, `monikers`, `boundary`,
`ExportPolicy`, `CfgAssignment`) are bespoke read models, and semver/security/nav live
elsewhere.

**The abstraction:** with the IR as an EDB (nodes + relations), these are **derived views**
— datalog/Trustfall rules over the base facts:

- `exported(x)` ⇐ `visible(x) ∧ (parent(x,p) ⇒ exported(p))` (a recursive rule, not hand-code).
- `moniker(x, path)` ⇐ walk `parent` edges.
- `usages(y) = { r | r.to = y, r.kind = Calls }` — a query, not an index build.
- semver diff, security taint = rules over `Relation` + two generations' `Names`.

`reflect.rs`/`view.rs` become a *rule set*; the "ONE Trustfall query plane" from the
resolution/IR unification brief is literally this. The search index (`index`) is a
*materialized* IDB (tantivy/qdrant = cached query results), not a separate hand-maintained store.

**Cost:** you need a query engine (Trustfall is already in `registry/graph`). Recursive rules
need a fixpoint evaluator; scope it to what's used.

---

## 2. Before / after

| Concern | Today (≈15 modules, ≈33 twins) | North star (6 abstractions) |
|---|---|---|
| Model | `kind` + rich `Kind` **and** `wire` `KindWire` + `kind_wires` side table | `Node<R>` (A2), one type, `R`-parameterized |
| Seal | `builder::seal_payloads` hand-maps to twins | generic `map_refs: Node<Build>→Node<Canon>` fold |
| Types | inline (workspace/ir) vs entries (POC) — a dilemma | nodes with structural sharing (A2), no dilemma |
| Edges | `children`/`fields`/`Occurrence`/`LinkRecord`/`OracleCall`/… | one `Relation` (A3) = EDB |
| Identity | `IntroId` + `CasKey` + `GenerationStamp` + `payload_hash`, intertwined | nominal `Intro` (cell) vs structural `Hash` (version) |
| Registry | `RwLock<HashMap>` + resolver + `PristineIntroTable` | `Store` = 3 maps (A5), lazy read-through |
| Codec | `wire` + `ascii` + `serialize` + `body_wire` | one derived canonical codec |
| Body merge | `merge_body`/`ConflictPolicy`/steady-state | lattice join over `Relation`s |
| Producers | `NudoxPath`/`Index`/`entries_by_path` ×7 | emit `Fact`s (A4); structure derived |
| Views | `view`/`reflect` + bespoke semver/security | datalog IDB (A6) over EDB |

Net: `entry, index, symbol, kind, kinds/*` collapse to **Model**; `change, intro, skeleton,
wire, ascii, serialize, body_wire` collapse to **Identity+Codec**; `apply, manifest,
registry` collapse to **Store**; `view, reflect, vocab` collapse to **Query**; `body`
collapses into **Model+Relation**; `builder, lower` are **Production**. Six planes.

---

## 3. Essential complexity we must *keep* (honesty ledger)

Elegance is deleting *accidental* complexity, not pretending the hard parts don't exist.
These do not simplify away, and the design should *isolate* them so the rest stays clean:

1. **The nominal-identity bootstrap (§4.3 disambiguator).** Deriving a *stable* id from an
   oracle that has no prior state — stable across signature edits for unique names, distinct
   for overloads, `TraitImpl`-scoped for impls — is irreducible. It's the price of VCS lineage
   without a UUID oracle. **Isolate** it as a single `trait NamingStrategy { fn intro(...) -> Intro }`
   with today's rule as the one impl. It touches nothing else.
2. **Dual-fidelity body production.** Tree-sitter (milliseconds, shallow) + oracle (seconds,
   precise) is a real perf/coverage frontier. The *merge* simplifies (Collapse 6) but the two
   producers stay. Model their output as `Relation`s at different `Confidence`.
3. **Cross-language semantic lowering.** Mapping 7 type systems onto one is inherent. The
   combinator layer removes the *plumbing*, never the *semantics*. Keep per-language grammar modules.
4. **Frozen-format versioning.** A distributed/VCS system can't silently change bytes.
   Content-addressing makes this *cleaner* (version the one canonical codec; re-hash is
   verifiable) but not *absent*. Keep a `FORMAT_VERSION` and golden tests.
5. **CRDT merge (libpijul).** Conflict-free concurrent editing is a genuine feature. Keep it —
   but *outside* the IR model, operating on `Names`/`Manifest` deltas. The IR stays
   libpijul-free (already an invariant); the VCS is a *history over the Store*, not a thing the
   node model knows about.

Everything not on this list is fair game to delete.

---

## 4. What the surrounding infrastructure becomes

Once the IR is "content-addressed nodes + relations + three store maps," the neighbours stop
being bespoke systems and become thin projections:

- **VCS (`ir-vcs`/libpijul).** A generation is a `Manifest` (`{Intro→Hash}`). History is a DAG
  of manifests; a diff is a *set diff* of `Names` entries + a Merkle diff of changed nodes
  (cheap — unchanged subtrees share hashes). libpijul earns its keep only for *concurrent
  merge*; linear history needs just manifest diffs. The elaborate `diff/*` matcher shrinks to
  "compare two `Names` maps; changed hashes point at changed nodes."
- **Registry / resolution.** = lazy read-through over `Store` (A5). Cross-package = look up
  `StableRef = (Pkg, Intro)` in `Manifest`→`Names`→`CAS`. The `RegistryResolver` trait stays;
  its body is three map lookups.
- **Incrementality / daemon.** Content-addressing gives memoization for free: key oracle runs
  by input hash; the `Store` *is* the memo table. Re-analysis of an unchanged file is a hash
  hit. This is the compiler-fleet-cache direction with no extra machinery.
- **Search index.** = materialized IDB (A6). Symbol search, usages, "find refs" are queries
  over `Node`+`Relation`; tantivy/qdrant hold cached projections that a rule invalidates by hash.
- **GUI / navigation.** = queries (A6). "Go to def" = `Names[intro]`; "find usages" =
  `Relation` scan; "expand type" = follow node hashes. No special nav structures.
- **Global IR graph (`docs/GLOBAL-IR-GRAPH.md`).** *Is* this model at scale: PURL-keyed `Manifest`s,
  `CAS` over iroh-blobs, `Names` over iroh-docs, remote generic-fallback vs local accurate-top
  = two `Names` layers with LSM shadowing. The north-star IR and the global graph are the same
  object at two scopes.

---

## 5. Worked example (end to end)

A Go `type Point struct { X, Y int }` with a method `func (p Point) Dist() float64`:

1. **Producer emits facts** (no tree, no paths):
   `Declare{local:0, kind:Record, sym:"Point", parent:mod}`
   `Declare{local:1, kind:Field, sym:"X", parent:0, ty:int}` … same for `Y`
   `Declare{local:2, kind:Function, sym:"Dist", parent:mod, recv:Point(local:0), ret:f64}`
   `Relate{from:Dist, to:Point, kind:Implements?/RefersTo}` (receiver as a relation)
2. **Store interns & seals** (`map_refs: Build→Canon`, bottom-up):
   `int`, `f64` → shared `Struct` hashes (dedup across the whole graph).
   `X`,`Y`,`Point`,`Dist` → `Decl` nodes; each gets a `Hash` (Merkle over its sealed body) and
   an `Intro` (via `NamingStrategy`; `Dist` unique → stable id even if `f64`→`float32` later).
3. **Store updates three maps:** `CAS[hash]=blob`, `Names[intro]=hash`, `Manifest[pkg]∪={intros}`.
4. **Wire = `Ir<Canon>` bytes** (one derived codec) — no `KindWire`, no `ascii` twin.
5. **Queries** (A6): `usages(Point)` = `Relation.to=Point`; `exported(Dist)` = rule;
   semver vs last gen = `Names` diff. VCS records the `Manifest`/`Names` delta.

Every arrow above is one of the six abstractions. Nothing bespoke.

---

## 6. Adoption — fold this *into* the migration, don't fork it

The good news: the biggest collapses **make the migration plan simpler, not harder**, and can
land *inside* its phases. The bold ones become the post-parity north star.

- **Adopt during migration (net-negative code):**
  - **Collapse 1** (parameterize + derive `map_refs`) → land in **P2**. It *replaces* the
    lossy-`Kind`+side-table work rather than adding to it: instead of writing "derive wire
    twins at seal," you write one `#[derive(Node)]` and delete `wire.rs`'s hand-written twins.
  - **Collapse 3** (one `Relation`) → land in **P4**, when folding `vocab`/`apply`/`body` — unify
    the edge readers as you move them.
  - **Collapse 6** (lattice-join body merge) → land in **P4** with `body`.
  - **Collapse 7** (facts) → *is* the **P6** combinator layer, expressed as `Fact` emission.
- **North star (stage after parity, aligned with `docs/GLOBAL-IR-GRAPH.md`):**
  - **Collapse 2/4/5** (content-addressed nodes as *primary*, Store-as-three-maps, one codec)
    is a wire/identity evolution — do it behind `FORMAT_VERSION` once ir-vcs consumers ride the
    derived codec. This is where the IR and the global graph converge.
  - **Collapse 8** (views/semver/security as IDB) → grow the Trustfall rule set already seeded
    in `registry/graph`, retiring `reflect.rs`/`view.rs` incrementally.

**Litmus test for any future addition** (the operating principle): *"Is this a new structure
beside the IR, or a projection of nodes + relations?"* If it's a new structure, it's probably
a `RelKind`, a `NamingStrategy`, or a query — not a new module.
