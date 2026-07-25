# IR-MIGRATION-PLAN — unify the IR into `crates/nudox-ir`

> **Status:** greenfield freeze, 2026-07-24. Authoritative for how `workspace/ir`'s
> richness folds into `crates/nudox-ir`'s architecture ("the SPIRIT"), how the four
> live consumers and seven language producers repoint onto it, and how production
> gets a clean **builder + parser-combinator** API. Supersedes the `IR-NOTES.md`
> POC narrative and the `06-new-ir-rewrite.md` brief (both are *donors*, not the
> target). Where this plan and `.research/ir-vcs/design/IR-NATIVE-VCS-DESIGN.md`
> (production deltas) disagree on the wire/identity contract, the design wins.

## 0. Decisions locked

| # | Decision | Choice |
|---|----------|--------|
| D1 | **Destination crate** | `crates/nudox-ir` is the home. It grows to full richness; `workspace/ir` is dissolved into it; the 4 consumers + 7 producers repoint to it. |
| D1a | **Package name** | Rename the package to **`ir`** (directory optionally moves to `crates/ir`). This preserves the ~200 existing `use ir::…` sites; only 4 consumer `Cargo.toml` `path=` lines change. `nudox_ir` is *not* kept as an import prefix. |
| D2 | **Production API** | Clean typed **closure-tree builder** (`create`/`create_ref`/`link`) + per-kind `bon` builders that auto-emit links, **plus a parser-combinator lowering layer** so each language declares an oracle→IR grammar driven by a shared engine. |
| D3 | **Migration style** | **Strangler** — grow the home crate to parity *behind the exact `ir::` public surface the consumers use*, cut consumers over while they keep compiling, then repoint producers last, then delete `workspace/ir`. |
| D4 | **Wire/identity model** | Keep `ir`'s **content-addressed** identity (`IntroId`/`StableRef`, frozen `u16` `KindDiscriminant`, wire twins, `PristineIntroTable`) as the frozen downstream contract. Adopt nudox-ir's model only *in memory / at build time*. |

The three strands being reconciled:

1. **SPIRIT** — `crates/nudox-ir`: macro-generated `Kind`/`KindDiscriminant`/`EntryKind`,
   derive-`Visitor`, bit-packed typed `EntryIndex`, closure-tree `EntryBuilder`, lazy
   `FrozenVec`+`OnceCell`+ouroboros registry, `UniqueId`. Clean but only 7 kinds, minimal `Type`,
   no bodies/wire/change/manifest.
2. **RICHNESS + LIVE CONTRACT** — `workspace/ir` (pkg `ir`, ~8k LOC): 12 frozen kinds, rich
   `Type`/`TypeRef`, ~33 wire twins (generics/where-preds/cfg/attrs/auto-traits/flags),
   `IntroId`/`StableRef`, `intro` bootstrap + §4.3 disambiguator, `skeleton`, dual-fidelity
   `body`, `change`/`manifest`/`apply`, `view`/`reflect`/`ascii`/`vocab`. Consumed by
   `ir-vcs` (deep), `registry`, `index`, `driver`.
3. **LEGACY PRODUCER RICHNESS** — the 7 producers in `workspace/compiler/compile/*`
   (~4,900 LOC, *not* a Cargo member) target a **deleted** nested `ir` API: `Entry::Module(Symbol{path:NudoxPath, inner, members})`,
   `ir::generics::{ConstExpr,Constraint,GenericArg,Generics,TraitRef,TypeExpr,Variance}`,
   `ir::protocols::{ReceiverKind,TraitMethod}`, `ir::ty`, `ir::function`, `Index{roots,entries}`.
   They emit generics/const-expr/protocol/variance detail that *neither* current crate fully models.

---

## 0.5 — Rev 2: adversarial corrections to the build plan (2026-07-24)

A three-adversary review (full verdict in `IR-NORTH-STAR.md` §0.5) changes several decisions:

- **D5 — codec: two concrete types + `proc-macro derive(Seal)`, NOT a GAT `Node<R>`.** The GAT
  infects `ir-vcs`'s ~1,276 twin sites (incl. 13 concrete `KindWire` match arms in `diff/diff.rs`)
  for a phase distinction that is really two inherent-impl blocks, and the decl-macro `Visitor`
  can't do a type-changing fold. Keep the rich build `Entry`/`Kind` and the frozen
  `OwnedEntryPayload`/`KindWire`; generate the 12-arm seal with a **stable proc-macro**. This also
  gives the "can't serialize/hash an unsealed node" safety for free (the build type has no
  `Serialize`/`payload_hash`). Supersedes §2.6's "derive twins" phrasing.
- **D6 — P0 becomes a HARD gate: write golden postcard *byte* snapshots first.** They don't exist
  (only round-trip determinism). Without them, twin-unification (P2) is silent-corruption-prone.
- **D7 — new P0.5 (before P3): fix the `IntroId` disambiguator — a VERIFIED data-corruption bug.**
  `skeleton.rs:220,244` ignore `generics`/`wheres`/negativity, so distinct impls/overloads mint the
  same `IntroId` → `PristineIntroTable` overwrite. Fold generics/wheres/negativity (or an explicit
  `impl_index`) into the placement key; the `Type` model also needs a generic-application node to
  tell `Foo for Bar<u32>` from `Foo for Bar<String>` — sequence with P7 (generics subsystem).
- **D8 — strong-typing core (P1/P2), GAT-free:** `const DISC` + `type Body` on `EntryKind`; a checked
  `downcast → Result` as the only narrowing (delete the `unsafe` `TypedEntry` cast + the no-op
  `resolve_typed`); per-kind `KindBody` (enum only at storage/wire) — this *is* the I4 "kill the
  side table" work and it makes placeholder kinds unrepresentable; total derived
  `KindDiscriminant↔u16`; `StrId`-typed collision key; single-source `parent` (derive `children`);
  `NonZeroU16` width; private index newtypes; role-newtypes for `inputs`/`outputs`.
- **D9 — relation scope (P4): extrinsic facts only.** Unify occurrences/calls/mentions into one
  `Relation`; keep fields/params/variants **inline**; keep undirected `LinkRecord` and
  attribute-facts distinct. Body-merge stays a span-overlay (§2.6), **not** a lattice join.
- **Identity model (§2.3) restated:** three layers (content checksum / placement key /
  lineage-by-matcher), not "nominal vs structural." The IR is a name-keyed property graph with
  per-node checksums; content-addressed **only at the CAS boundary** (SCC-gated intra-package,
  strictly nominal inter-package). Keep the Hash①/② discipline.
- **Open decision (resolve before P8):** overloads = N intros vs a `Group` node — the producers fold
  overloads into one node; §5 splits them. Pick one.

---

## 0.6 — Built so far, and the next phases (2026-07-25)

### Landed (on `main`, 56 tests, clippy-clean)

`crates/nudox-ir` is now a rich, typed, content-addressed IR: 12 frozen-`u16` kinds with
rich bodies, full-parity `Symbol`, forms/flags/generics, checked `downcast` (the `unsafe`
typed-handle lie is gone), content-addressed identity (`IntroId`/`StableRef`, byte-verified
against `workspace/ir`), collision-free `intro`+`skeleton`, `seal`, `PristineIntroTable`,
occurrence `vocab`, dual-fidelity `body`, `reflect`, `IrView`, `GenerationStamp`.

**The `Ref` unification (the big streamline).** One type is now *the* reference everywhere
an entry points at another:

```rust
enum Ref<T> { Local(EntryIndex<T>), Intro(IntroId), Foreign(StableRef) }
```

It replaced `EntryIndex` in kind bodies, `UntypedEntryIndex` in `Node` tree edges, and the
re-export forwarding ref — and it is what `Type::Nominal`/`Type::Apply` carry. The
derive-`Visitor` was generalized to walk `&mut Ref` (its dead in-place `visit` dropped), so
**`seal` lowers every `Local → Intro` in one `visit_mut` pass** — kind-body refs, type refs,
and tree edges at once. That is the whole "Build→Canon" step, achieved with **no GAT, no
type duplication, no wire twins** (exactly what the adversarial review recommended over the
`Node<R>` design). `PristineIntroTable` is now genuinely self-contained.

### Next phases, in order

**P-A — close the residual identity gap (small, self-contained).**
`ref_skeleton` hashes a *same-package, not-yet-sealed* nominal base as a placeholder, because
skeletons are computed before refs are lowered and a forward-referenced sibling has no
`IntroId` yet. Fix: split `seal` pass 2 into (a) mint intros for all entries using only
name/path/kind + *structural* disambiguator evidence, then (b) resolve local nominals to
those intros and recompute the skeleton-bearing disambiguators for impls/overloads. Needs a
fixed-point/ordering rule for the mutually-recursive case (`impl Foo for Bar` where `Bar`'s
own id depends on nothing circular — the two-tier split is what makes it terminate). Gate: a
test where `impl Foo for Bar` and `impl Foo for Baz` (bare same-package nominals, no generic
args) mint distinct ids.

**P-B — the production API (`lower/`): builder + parser-combinators.**
The D2 layer. `EntryBuilder` already returns `Ref<T>` and auto-tracks parent/children, so
what remains is (1) per-kind `bon` builders that auto-emit graph links, and (2) the combinator
vocabulary (`Lower<O>`, `map`/`and_then`/`alt`/`many`/`field`/`into_entry`) plus a shared
`drive(root_nodes, grammar, pkg) -> IrPackage`. Producers then emit `Fact`s (`Declare`/
`Relate`) and the store derives the tree — deleting `NudoxPath`/`Index`/`entries_by_path`/
`wire_members` from all seven frontends. Land this **before** touching producers.

**P-C — consumer cutover (~300 `use ir::` sites; the compatibility phase).**
`ir-vcs` (deep: ~1,276 wire-twin refs, 13 concrete `KindWire` match arms in `diff/diff.rs`),
then `registry`/`index`/`driver` (read-model only). Two honest sub-decisions to make first:
(i) nudox-ir has **no `KindWire` twins** — consumers either move to matching the rich `Kind`
directly (preferred; that *is* the twin-deletion win) or a compatibility shim is written;
(ii) nudox-ir's `IntroId` is domain `nudox.intro.v3` and **not** byte-compatible with
`workspace/ir`'s buggy v2, so cutover is a lineage reset, not a migration. Write the golden
postcard **byte** snapshots (Rev 2's P0 gate — they still do not exist) before this phase.

**P-D — producer repointing (7 languages, ~4,900 LOC).**
Order by independence: Rust → Go → C# → Java → Python → Nix → TypeScript (TS/OXC last,
heaviest cross-module linking). Make `workspace/compiler` a Cargo member so it builds in-tree.
Gate per language: the existing `tests/snap_*.rs` insta snapshots reproduce.

**Still open (decide before P-D):** overloads = N `IntroId`s (what `seal` does today) vs a
`Group` node (what Java/TS/C#/Python producers actually emit — one node with an `overloads`
array). These are contradictory; the producers' model may win on contact.

---

## 0.7 — P-A, P-B, richness parity and the cutover gate all landed (2026-07-25)

`crates/nudox-ir`: **100 tests (92 unit + 8 golden), clippy clean.** All of §0.6's P-A and
P-B is done, plus two phases §0.6 did not know were needed.

**P-A closed the identity gap** with a one-step stratification rather than the two-tier
provisional/final scheme §0.6 proposed. `path_id(e) = hash(kind, path, name,
Disambiguator::None)` is ref-independent, so it is computable for every entry up front and
terminates by construction. For an entry declared once at its path, `path_id` *is* its final
`IntroId`. A resolved `Ref::Local` encodes byte-identically to `Ref::Intro`, so skeletons are
now stable across sealing. `skeleton.rs` public surface: 10 items → 3.

**P-B is `Lowering`, not combinators.** Producers get a flat item list with parent pointers;
`create_export` already interns, so `declare`/`refer` are order-independent with no new
machinery. The `Lower<O>`/`alt`/`many` vocabulary was *deliberately dropped*: combinators pay
off over a uniform token stream, and the seven producers consume seven different typed ASTs
that already pattern-match their own enums. `finish` also makes `IrPackage::is_valid` real
(Undeclared/Duplicate/Cycle) — and the cycle case is load-bearing, because `seal` walks parent
chains with no guard and **hangs** on a cycle today.

**Two phases §0.6 missed**, both found by auditing nudox-ir against `workspace/ir`:

- **`Alias` kind at frozen discriminant 5** — the one whole kind with no home. Named `Alias`
  because `kinds::ty::Type` already exists. It deliberately does *not* feed the identity
  skeleton: hashing `target` would make identity signature-dependent and sever a symbol's
  history when its right-hand side is edited.
- **Richness parity** — the new IR was *architecturally* better but **lost data** in seven
  places. Now closed: auto-trait facts, variant discriminants, const values, fn `abi`/
  `is_defaulted`, three-way `Sealed` + `TriState` dyn-compat, and `Symbol` `attrs`/`cfg`.
  With nudox-ir's own gains (first-class `Param` entries with their own `IntroId`s,
  `Type::Nominal`/`Apply`, inline `Type` bounds vs id-only `TypeRefWire`, `Ref<T>`, checked
  `downcast`, collision-free skeletons), **"richer after cutover" is now true in data, not
  just in architecture.**

**The golden gate exists** (Rev 2's P0): one rich sealed fixture, six pins, one-command
regeneration that fails so CI cannot leave it set — verified to actually guard by perturbing
the fixture.

### What P-C now needs (from the cutover inventory)

Counts are measured, not estimated. `ir-vcs` ~395 refs, `registry` ~47, `index` ~29,
`driver` ~13.

- **Delete the `KindWire` twins, do not shim** — 328 `KindWire::` sites, but they are *pure
  field comparators*; nudox-ir's rich `Kind` carries the same data. `diff/diff.rs` has 14
  match arms, not 13.
- **Order:** `registry/graph` → `index` → `driver` → `ir-vcs` (minus format) → `ir-vcs/diff`
  → `f1.rs`/`blob.rs`. The last is the real bottleneck: NdIrF1 is the libpijul change format
  and its key registry lives in the **vendored libpijul fork**, so a format change is
  lock-step across both.
- **v2 → v3 is a lineage reset, not a migration.** `.nir` filenames *are* IntroId hex and
  pijul change files embed v2 ids, so those stores must be wiped and resealed. Good news: no
  `.nir` files or change stores are committed to the repo, and `symbols_proj.intro_id` is
  still a stub column — the blast radius is runtime stores only.
- **Atomic pairs** (cannot be sequenced apart): `ir-vcs::protocol::BodyWire.body` carries
  `ir::BodyEmbed` across the ir-vcs↔driver boundary; `IrView` is a different type in each
  crate and both `registry` and `driver` take it.
- **Still missing for P-C/P-D:** `OwnedEntryPayload` has no nudox-ir equivalent (the old
  `PristineIntroTable` stores content-hashed payloads and uses `payload_hash` for fast-path
  equality; nudox-ir stores `Entry` with no content hash); no NdIrF1 serializer; no link
  records; `manifest` is a stub next to `BlobManifestV3`/`Outbox`; `body::Language` must be
  unified with `heart::Language`.
- **`workspace/compiler` calls an `ir` API that does not exist** (`ir::entry::NudoxPath`,
  `ir::pipeline::Ir`). It is Buck2-only and not a cargo member — audit separately before P-D.

**Still open:** overloads = N `IntroId`s (what `seal` does) vs a `Group` node (what the
Java/TS/C#/Python producers emit). Unchanged from §0.6; decide before P-D.

---

## 0.8 — P-D: the producer port (2026-07-25)

`workspace/compiler` is **bit-rotted, not stale**: commit `dcc0b72c` (Jul 23) deleted both
`workspace/ir/BUCK` *and* ten modules the producers call, and did not update them. It is the
only copy of all seven producers. So P-D is a **rewrite salvaging the oracle layer**, not a
repoint.

**New home:** `crates/nudox-producer` (the shared contract) + `crates/nudox-producer-<lang>`,
all Cargo members. The contract is two methods —

```rust
fn invoke(&self, src: &PackageSource) -> Result<Self::Oracle, ProducerError>;
fn lower(&self, oracle: &Self::Oracle, out: &mut Lowering<Self::Id>) -> Result<(), ProducerError>;
```

— plus `produce()` = invoke → lower → finish → seal. Everything daemon-shaped in the old
trait (`ExecPlan`, `SealedCommand`, `Captured`, `WorkerPool`, `ProducerProfile`, `ThreatTier`,
`Mounts`, hermetic env) stays in the sandbox plane. `Lowering<Id>` is generic over the
producer's *own* id, so no frontend invents a path scheme, and `refer` resolving
later-declared ids means **one pass, no intermediate tree** — that is what deletes
`NudoxPath`/`Index`/`wire_members` from all seven.

### Dependency triage (measured, not guessed)

| Lang | Oracle | Supply | Rank |
|---|---|---|---|
| Go / C# / Java | subprocess + JSON | none needed | **(a)** — only `serde` |
| Rust | `ra_ap_*` | crates.io, lockstep `=0.0.341` (salsa keys break across versions) | **(a)** |
| TypeScript | `oxc_*` | crates.io `=0.139.0`; `oxc_resolver` `=11.23.0` | **(a)** |
| TypeScript (tsz) | `tsz_*` | git `dff7690` (`0.1.48`; crates.io only has 0.1.9) | (b) — opt-in, defer |
| Python | `pyrefly` | git `3e17a690`; build.rs downloads a typeshed | (b) + 3 source bugs |
| Nix | `snix_eval` | git `50b41ae`, **never on crates.io**, needs a local patch, **GPL-3.0** | **(c)** — vendor it |

So Rust and TypeScript are *not* blocked, contrary to the first estimate. Only Nix needs
build-system work.

### Salvage map (measured `ir::` reference counts)

- **Rust** (4,896): ~2,600 carries over (`load` 183/0 refs, `error` 328/0, `traversal` 332/0,
  `producer` 85/0, `ctx` 440/2, `function` 283/2, `docs` 251/2, `generics` 269/1, `walk`
  115/2, `source` 45/2). Rewrite is confined to `ra/ty.rs` (981/8) and `ra/item.rs` (1325/20).
- **TypeScript** (7,251): ~1,300 carries over (`entry` 414/0, `graph` 328/0, `error` 132/0,
  `jsdoc` 249/1, `facts` 156/1). Rewrite ~5,100, heaviest `extract/decl.rs` (1742/**24** —
  low density for its size; most of it is OXC AST matching worth preserving).
- **Python** (3,642): `docstring.rs` (760/**0**) is pure salvage; `types.rs` (745/**87**) is
  the densest IR coupling anywhere.
- **Nix** (5,563): ~2,450 carries over (`traversal` 723/0, `syntax` 673/0, `eval` 460/0,
  `docs` 369/0, `error` 116/0). Rewrite `walker` 927/9, `options` 313/11, `sig` 571/5.
- **Go**: `oracle.rs` (436/0). **C#**: `schema.rs` (541/0) + `xmldoc.rs` (488/0).
  **Java**: `javadoc.rs` (796, near-zero).

Pure rewrite, no salvage: `graph/from_ir.rs` (1,337) and `render/` (~3,500) — both map the
deleted `ir::kind::Entry` and have no oracle logic.

### Subprocess escape hatch
**Nix** is the one place worth considering it: a snix-based JSON dumper would move the
GPL-3.0 link out of our address space entirely. Costs a redesign of `walker.rs`'s
closure↔rnix span fusion. Python's `pyrefly check` emits LSP diagnostics, not IR — not a
shortcut. Rust and TS have no subprocess mode (ra_ap *is* the replacement for the old
rustdoc subprocess).

---

## 1. Target architecture

The home crate ends up as one crate with a clear two-face design:

```
                    ┌───────────────────────── in-memory / build time ──────────────────────────┐
producers ──lower──▶│  EntryBuilder (closure tree)  ▶  EntryArena<Entry{Symbol,Node,Kind}>       │
 (combinators)      │      · typed EntryIdx<T> references (arena-local)                           │
                    │      · rich in-memory Kind bodies (NO parallel side table)                  │
                    │      · graph links (EntryLink)                                              │
                    └───────────────────────────────── seal ────────────────────────────────────┘
                                                          │  two-pass: IntroId + §4.3 disambiguator,
                                                          │  lower EntryIdx→IntroId, derive wire twins
                                                          ▼
                    ┌──────────────────────────── wire / query time (FROZEN) ────────────────────┐
 ir-vcs ◀───────────│  (IntroId, OwnedEntryPayload, parent:Option<IntroId>)  ▶  PristineIntroTable│
 registry/index ◀───│  KindWire/SymbolWire/TypeWire twins · IntroId/StableRef refs · LinkRecord   │
 driver             │  IrView / Occurrence · body_wire (.nb) · manifest · ascii/postcard F1       │
                    │  lazy Registry<R:RegistryResolver> (Result-fallible, IntroId-keyed)         │
                    └────────────────────────────────────────────────────────────────────────────┘
```

**Central thesis (kills the biggest anti-pattern):** today `workspace/ir` keeps a
*deliberately lossy* in-memory `Kind` (5 variants) plus a **parallel `kind_wires:
Vec<KindWire>` side table** carrying the real data. We delete the side table. In-memory
`Kind` becomes **full-fidelity** (nudox-ir spirit); it references **nominal** siblings by
build-time `EntryIdx<T>` and **foreign** entries by `StableRef`; structural type nesting
stays inline. `seal` computes every entry's `IntroId`, then *derives* the `KindWire` twin,
lowering `EntryIdx<T>` → `TypeRef::Same(IntroId)`. Producers never touch wire types; the
wire contract is unchanged.

### 1.1 Module layout (home crate)

```
crates/ir/ (pkg "ir")
├─ lib.rs                 prelude + build + re-export surface (mirror today's ir::* exports)
├─ visitor.rs             derive-macro Visitor (from nudox-ir) — index relocation + walks
├─ index.rs               ArenaIdx, PackageIdx, EntryIdx<T>, RawEntryIdx, StrId, LinkId,
│                         TypeFingerprintId, sealed EntryKind, UntypedMarker
├─ entry/                 Entry, EntryInner{Owned(Kind)|Reference(RawEntryIdx)}, Node,
│                         EntryArena, StringInterner, TypedEntry<T>
├─ symbol.rs              Symbol (StrId-interned, full parity: aliases/deprecation/doc_links),
│                         Visibility (6 levels), ByteSpan, Deprecation, DocLink
├─ kind.rs               register_kinds! ⇒ Kind, KindDiscriminant(frozen u16), EntryKind impls,
│                         markers; rich Kind bodies
├─ kinds/                 module, record{Record,Field,FieldKey,FieldAttribute,RecordForm},
│                         sum{Enum,Variant,VariantForm}, function{Function,Receiver,FnModifier,Param},
│                         trait_{Trait,TraitFlags}, impl_{Impl,ImplFlags}, const_static, reexport,
│                         ty{Type,TypeRef,Primitive,Width}, generics{GenericParam,WherePred,…},
│                         protocols{ReceiverKind,…}  ← restored legacy richness
├─ link.rs                EntryLink (undirected, sorted) → LinkDomainKey/LinkRecord at seal
├─ builder/               EntryBuilder (closure tree), per-kind builder integration, seal
├─ lower/                 parser-combinator production layer (Lower<O>, combinators, driver)
├─ change/                domain, encode, hash, ids  (IntroId/StableRef/PackageLineageId/…)
├─ intro.rs               bootstrap_intro_id{,_v2}, sig_key, Disambiguator{,V2}
├─ skeleton.rs            fnsig/type/trait-impl skeletons, type_fingerprint
├─ wire.rs, body_wire.rs, serialize.rs, ascii.rs   FROZEN F1 wire twins + codecs
├─ apply.rs               PristineIntroTable, LinkRecord   (materialize container)
├─ view.rs, reflect.rs, vocab.rs                    read model / monikers / occurrence vocab
├─ body.rs                dual-fidelity body facts
├─ manifest/              generation, manifest, outbox
└─ registry/              lazy Registry<R>, RegistryResolver (Result), state, resolver
```

### 1.2 Invariants (must hold at every phase gate)

- **I1 — Frozen wire.** `KindDiscriminant` `u16`s, the ~33-key F1 registry, `IntroId`
  preimages, `GenerationStamp`/`payload_hash` domains never change. Round-trip tests pin them.
- **I2 — Consumer surface.** The `ir::` items in §6 keep their paths and shapes; `use ir::…`
  in consumers compiles unchanged (only Cargo `path=` moves).
- **I3 — Id stability.** A unique-named symbol's `IntroId` is signature-stable; overloads
  flip via `FnOverload`; impls always use `TraitImpl` (§4.3). Existing `builder.rs` tests port verbatim.
- **I4 — No side table.** After P2 there is exactly one source of truth per entry (the rich `Kind`); `KindWire` is only ever *derived* at seal.
- **I5 — libpijul-free.** The crate never links `libpijul`/`iroh`; change semantics stay in `ir-vcs`.

---

## 2. Design pillars (what to build)

### 2.1 Core model (entry/index/symbol)

`Entry`, `Node`, `EntryInner` are already **near-identical** across the two crates — adopt
`workspace/ir`'s versions (they carry the interned `Symbol`) and keep the shape:

```rust
struct Entry { sym: Symbol, node: Node, kind: EntryInner }
enum  EntryInner { Owned(Kind), Reference(RawEntryIdx) }   // Reference = re-export/alias
struct Node { parent: Option<RawEntryIdx>, children: Box<[RawEntryIdx]> }
```

- **Index representation:** keep `workspace/ir`'s **explicit compound** `EntryIdx<T> =
  {package_idx: PackageIdx, arena_idx: ArenaIdx}` (multi-package, cross-package-native) as the
  in-memory handle — *not* nudox-ir's single bit-packed `NonZeroUsize`. Rationale: the
  content-addressed wire (D4) makes nudox-ir's import/export bit-packing + visitor-relocation
  unnecessary; the compound handle is simpler and is what the arena/registry already assume.
  Keep nudox-ir's **sealed `EntryKind`** + phantom `fn()->T` variance + `erase()`/`typed()`.
- **Symbol (full parity):** `name: StrId`, `visibility: Visibility` (6 levels + cfg), `documentation:
  Option<StrId>`, `source_path: StrId`, `span: ByteSpan`, `aliases: Box<[StrId]>`,
  `deprecation: Option<Deprecation>`, `doc_links: Box<[DocLink]>`. `StringInterner` owned by `EntryArena`.
- **Visitor:** port nudox-ir's derive-`Visitor` (decl_macro) so every model type gets recursive
  `visit`/`visit_mut(&mut RawEntryIdx)`. Used by seal to lower `EntryIdx→IntroId`, by walks, and by any future relocation.

### 2.2 Kinds (macro-generated, full-fidelity, all 12)

Merge the two `register_kinds!` macros into one that emits, for each kind: the `Kind` enum
arm (carrying the **rich body**), the frozen-`u16` `KindDiscriminant`, the `EntryKind` marker
+ `into_kind()`/`discriminant()`, and the `Visitor` impl. Preserve the **explicit frozen
`u16`** values (Module=1…Reexport=12; never renumber — I1).

Rich in-memory bodies (restoring the "types/params/fields are entries" spirit + legacy detail):

| Kind | In-memory body (references are `EntryIdx<T>` unless noted) |
|------|-----------------------------------------------------------|
| Module | `{}` (children via `Node`) |
| Record | `fields: Box<[EntryIdx<Field>]>`, `super_types: Box<[TypeRef]>`, `form: RecordForm`, `generics`, `wheres`, `auto` |
| Field | `key: FieldKey`, `ty: Option<TypeRef>`, `attributes`, `default: Option<ConstExpr>` |
| Function | `receiver: Option<Receiver>`, `input_params: Box<[EntryIdx<Param>]>`, `output_params`, `modifiers`, `sig: FnSigFlags`, `generics`, `wheres` |
| Param | `ty: Option<TypeRef>`, `attributes: Box<[ParamAttribute]>`, `default: Option<ConstExpr>` |
| Enum / Variant | `variants: Box<[EntryIdx<Variant>]>` / `fields`, `discr: Option<ConstExpr>`, `form: VariantForm` |
| Trait | `supers: Box<[TypeRef]>`, `flags: TraitFlags`, `generics`, `wheres`, `items` |
| Impl | `of: Option<TypeRef>`, `self_ty: TypeRef`, `flags: ImplFlags`, `generics`, `wheres` |
| Const / Static | `ty: TypeRef`, `value: Option<ConstExpr>` / `+ mutable: bool` |
| Type | `Type` (alias target), `generics`, `wheres` |
| Reexport | `target: StableRef` |

`Type`/`TypeRef`/`Primitive`/`Width`: keep `workspace/ir`'s **wire** shape frozen (structural
exprs inline; nominal leaves `TypeRef::Same(IntroId)|Foreign(StableRef)`). In memory, the
nominal leaf during build is an `EntryIdx<T>` (or `StableRef` for foreign); seal lowers it.

### 2.3 Identity, seal, and the wire twin (the crux)

- **Build time:** producers reference nominal entries by typed `EntryIdx<T>` (they *have* the
  indices). No `IntroId`s exist yet.
- **`seal(&arena, package: &PackageLineageId) -> Sealed`** (moves today's `seal_payloads` logic
  into the builder module, unchanged in id semantics — I3):
  1. Pre-pass: compute ancestor-segment chain + collision counts over `(kind_disc, segments, name)`.
  2. Pass 1: pick `DisambiguatorV2` (`FnOverload`/`TraitImpl`/`Span`/`None`) and compute every
     entry's `IntroId` via `bootstrap_intro_id_v2`.
  3. Pass 2: derive each `KindWire` from the rich `Kind`, using `Visitor` to lower every
     `EntryIdx<T>`→`TypeRef::Same(IntroId)`; foreign stays `StableRef`. Emit
     `(IntroId, OwnedEntryPayload, Option<IntroId> parent)` triples + `LinkRecord`s.
- **Output contract (unchanged for `ir-vcs`):** the triples feed `PristineIntroTable`; `ir-vcs`
  owns the diff to `IrAtom`s. `apply::{PristineIntroTable, LinkRecord}` keep their exact API (hard contract).

### 2.4 Registry (best of both)

Take nudox-ir's **lazy** machinery (`elsa::FrozenVec` + `OnceCell` interior-mutable append,
ouroboros self-referencing lookup, `papaya` concurrent map) but rekey it on the **content-addressed**
identity and make it **fallible** (production delta): `PackageLineageId` instead of path
`PackageId`; `Result<_, ResolveError>` instead of panics; lookups keyed by
`IntroId`/`StableRef`/`ProductionEntryId` returning `PristineIntroTable` entries. Keep
`workspace/ir`'s `RegistryResolver`/`AsyncRegistryResolver` trait shapes (already `Result`,
already what few callers expect) but back the cache with the lazy append store instead of
`RwLock<HashMap>`. Drop nudox-ir's import/export index-relocation path (unneeded under D4).

### 2.5 Graph links (restore from the POC)

Re-add the POC's `EntryBuilder::link`/`link_between`/`link_many` + `EntryLink` (undirected,
sorted vertex pairs). At seal, `EntryLink` → `LinkDomainKey` (domain `"nudox.link.v1"`, sorted
refs) → `LinkRecord`, exactly matching `change::domain` + `apply::LinkRecord`. This makes "IR
is a proper graph" real *and* wires it to the existing link contract in one step.

### 2.6 Bodies / change / manifest / views (fold as-is)

Port `body`, `body_wire`, `change/*`, `intro`, `skeleton`, `wire`, `serialize`, `ascii`,
`apply`, `view`, `reflect`, `vocab`, `manifest/*` **verbatim** in behavior; the only edits are
mechanical: retarget `EntryIdx`/`Symbol`/`Kind`/`StrId` imports to the unified modules, and
have `wire` derive from the rich `Kind` rather than a side table. These carry ~half the crate's
LOC and *all* of the frozen wire/domain constants — treat them as "move + rewire imports," not "redesign."

### 2.7 Generics / const-expr / protocols subsystem (the deferred richness)

Neither current crate fully models what the legacy producers emit. Land a real subsystem
(`kinds/generics.rs`, `kinds/protocols.rs`, a `ConstExpr` type) covering: `GenericParam`
(Lifetime/Type{bounds,default}/Const{ty,default}), `WherePred`, `TraitRef`, `Variance`,
`GenericArg`, `Receiver`/`ReceiverKind`, `TraitMethod`, and `ConstExpr` (field/param/variant
defaults, discriminants, array lengths, const values). Wire twins already partly exist
(`GenericParamWire`, `WherePredWire`) — extend, keeping the F1 registry append-only (I1).

---

## 3. Production API — builder + parser-combinators (D2)

Two layers. The **builder** is the imperative core (always available); the **combinator
lowering** layer sits on top so producers read like grammars.

### 3.1 Layer A — typed closure-tree `EntryBuilder`

```rust
impl<'a> EntryBuilder<'a> {
    /// Create a child entry; the closure builds its subtree with `b` scoped to it as parent.
    fn create<T: EntryKind>(&mut self, sym: Symbol, build: impl FnOnce(&mut EntryBuilder) -> T) -> EntryIdx<T>;
    /// Re-export / alias: an Entry whose EntryInner::Reference points at `other`.
    fn create_ref<T: EntryKind>(&mut self, sym: Symbol, other: EntryIdx<T>) -> EntryIdx<T>;
    /// Undirected graph links (sealed into LinkRecord).
    fn link<T: EntryKind>(&mut self, idx: EntryIdx<T>);
    fn link_between<T: EntryKind, U: EntryKind>(&mut self, a: EntryIdx<T>, b: EntryIdx<U>);
    /// String interning shortcut (returns StrId via the arena interner).
    fn intern(&mut self, s: &str) -> StrId;
}
// Terminal:
fn IrPackage::build(pkg: PackageLineageId, root: Symbol, f: impl FnOnce(&mut EntryBuilder)) -> IrPackage;
fn IrPackage::seal(&self) -> Sealed;   // triples + LinkRecords + PristineIntroTable
```

Parent/child tracking is automatic (closure scope). This is nudox-ir's ergonomics with the
compound index + interned `Symbol`.

### 3.2 Layer A′ — per-kind `bon` builders that auto-emit links

Keep the POC trick: each kind exposes `Kind::builder()…` whose `finish` is private (`#[builder(finish_fn(vis=""))]`);
the public `.build(b: &mut EntryBuilder)` runs *our* code after finish so links are emitted for free:

```rust
Record::builder().form(RecordForm::Struct).fields(field_idxs).build(b) // auto: b.link_many(fields)
```

This guarantees the graph is populated whenever a kind is constructed through the builder.

### 3.3 Layer B — parser-combinator lowering (`lower/`)

A tiny combinator vocabulary over an oracle's node type `O`, threading builder context so
producers *declare* oracle→IR mappings instead of hand-writing traversal + parent plumbing.

```rust
/// A lowering step: consume an oracle node, drive the builder, yield `T`.
trait Lower<O> { type Out; fn run(&self, node: &O, cx: &mut Cx) -> Result<Self::Out, LowerError>; }

// Cx wraps &mut EntryBuilder + the oracle's symbol table + diagnostics.
// Combinators (free fns / methods):
fn field<O, F>(name: &str, inner: F) -> impl Lower<O>;      // descend into a named child
fn many<O, F>(inner: F) -> impl Lower<O, Out = Vec<_>>;     // map over children, in order
fn opt<O, F>(inner: F) -> impl Lower<O, Out = Option<_>>;
fn alt<O>(branches: [BoxLower<O>; N]) -> impl Lower<O>;      // first matching oracle shape wins
trait LowerExt<O>: Lower<O> {
    fn map<U>(self, f: impl Fn(Self::Out)->U) -> _;
    fn and_then<U>(self, f: impl Fn(Self::Out,&mut Cx)->Result<U>) -> _;
    /// Terminal: create an Entry of kind K from the accumulated body + symbol.
    fn into_entry<K: EntryKind>(self, sym: impl Fn(&O,&mut Cx)->Symbol) -> impl Lower<O, Out = EntryIdx<K>>;
}
```

Each language contributes a `grammar` module: a set of `Lower` values mapping *its* oracle
nodes (rust-analyzer HIR, OXC, go/types Decl, doclet Extraction, pyrefly, nix eval, Roslyn) to
`into_entry::<K>()` calls. A shared `drive(root_nodes, grammar, pkg) -> IrPackage` runs the
grammar, and `IrPackage::seal()` finishes. Combinators handle the uniform "walk children,
recurse, thread parent, emit links" that all 7 producers currently duplicate by hand.

**Worked example — a Go struct decl:**

```rust
let record = field("fields", many(go_field.into_entry::<Field>(field_sym)))
    .map(|fields| Record::builder().form(RecordForm::Struct).fields(fields))
    .into_entry::<Record>(record_sym);         // parent/child + links automatic
let decl = alt([go_struct→record, go_iface→trait_, go_func→function, go_alias→type_]);
```

Producers shrink to: (1) build/parse the oracle, (2) apply the grammar. No `NudoxPath`, no
manual `entries_by_path` map, no explicit parent threading, no `seal` boilerplate.

---

## 4. Migration phases (strangler, sequenced)

Each phase is independently landable and ends at a green `cargo test` + the invariants (§1.2).
Consumers keep compiling through P1–P4 because we do not touch `workspace/ir` until P5b.

- **P0 — Scaffolding.** Confirm/pin the **nightly** toolchain (home crate uses `decl_macro`,
  `macro_derive`, `macro_metavar_expr*`). Add the home crate's future deps (`heart`, `blake3`,
  `smol_str`, `async-trait`) to its `Cargo.toml`. Stand up a `snapshot/` harness that seals a
  fixture package and pins triples + wire bytes (the parity oracle for later phases).

- **P1 — Core + macro machinery into the home crate.** Bring over `visitor.rs`, the merged
  `register_kinds!`, sealed `EntryKind`, compound `EntryIdx`/`index.rs`, full-parity
  `Symbol`/`StringInterner`, `Entry`/`Node`/`EntryArena`, `link.rs`. Rename package → `ir`.
  Gate: home crate builds; POC-level unit tests pass.

- **P2 — Rich kinds, kill the side table (I4).** Land all 12 rich `Kind` bodies (§2.2). Port
  `wire.rs` twins but make them **derived** from `Kind`. Delete the `kind_wires` side table.
  Gate: `wire` round-trip byte-identical to `workspace/ir` on fixtures.

- **P3 — Identity/seal/skeleton/change.** Fold `change/*`, `intro`, `skeleton`, `serialize`,
  `ascii`; move `seal_payloads`→`builder::seal` (I3). Port `builder.rs`'s id tests verbatim.
  Gate: `IntroId`/`payload_hash`/`GenerationStamp` domains + disambiguator tests pass unchanged.

- **P4 — Apply/view/body/manifest/registry.** Fold `apply` (`PristineIntroTable`,`LinkRecord`),
  `view`/`reflect`/`vocab`, `body`/`body_wire`, `manifest/*`; land the lazy fallible `Registry`.
  Gate: `PristineIntroTable`/`IrView`/`Occurrence`/body-merge tests pass.

- **P5 — Consumer cutover.** (a) Diff the home crate's `pub` surface against §6; add any missing
  re-exports/aliases so `use ir::…` is satisfied. (b) Flip the 4 consumer `Cargo.toml`
  `path`s (`../ir` → `../../crates/ir`). (c) `cargo build -p ir-vcs -p registry -p index -p driver`;
  fix only mechanical breakage. Gate: whole workspace (minus producers) green; **`workspace/ir`
  now has zero reverse-deps**.

- **P6 — Production API.** Land Layer A/A′ (`builder/`) + Layer B (`lower/`). Port the
  `EntryBuilder` tests. Gate: seal a multi-kind package through the combinator path and match P0 snapshots.

- **P7 — Generics/const-expr/protocols subsystem (§2.7).** Extend kinds + wire twins
  (append-only). Gate: a fixture exercising generics/where-preds/const defaults/receivers round-trips.

- **P8 — Producer re-pointing (7 languages).** For each producer, write a `grammar` module over
  its oracle and delete the dead `Entry::*`/`NudoxPath`/`Index` construction. Order by
  size/independence: Rust → Go → C# → Java → Python → Nix → TypeScript (TS/OXC last; heaviest
  cross-module linking). Make `workspace/compiler` (or its `compile` crate) a Cargo member so it
  builds in-tree. Gate per language: the existing `snap_*` insta snapshots (all 6/7 targets) reproduce.

- **P9 — Delete + cleanup.** Remove `workspace/ir` from members and disk; drop the historical
  `nudox-ir-*` naming from comments; update `README`/`CONSOLIDATION-NOTES`. Gate: `rg 'workspace/ir\b'`
  clean; full workspace + Buck dual-build green.

---

## 5. Consumer contract to preserve (I2)

The exact `ir::` items in downstream use today (keep paths/shapes):

- `ir::change::{IntroId, StableRef, PackageLineageId, EcosystemId, PackageName, ChangeSetFingerprint, ContentBlake3, ChangeId}`
- `ir::wire::{KindWire, SymbolWire, OwnedEntryPayload, TypeWire, TypeRefWire, FieldWire, ParamWire, FunctionWire, EnumWire, RecordWire, ImplWire, TraitWire, TypeAliasWire, VariantWire, ConstWire, StaticWire, ReexportWire, ModuleWire, GenericParamWire, WherePredWire, FnSigFlags, ImplFlags, TraitFlags, RecordForm, VariantForm, SelfKind, TriState, CfgExpr, AttrTok, AutoFact, AutoState, AutoTrait, EntryPayloadFlags, Sealed, DeprecationWire, DocLinkWire}`
- `ir::apply::{PristineIntroTable, LinkRecord}` — **hard contract** (`ir-vcs` builds tables directly)
- `ir::view::{IrView, Occurrence}`; `ir::{BodyEmbed, ReferenceKind, IrView}`
- `ir::serialize::{symbol_path, is_symbol_path, intro_hex_of, LinkWire}`; `ir::ascii::{escape, unescape, hex_to_32, encode_typeref, decode_typeref, encode_typeexpr, decode_typeexpr}`
- `ir::vocab::{ReferenceKind, Confidence, RelSpan}`; `ir::symbol::{Symbol, Visibility, ByteSpan, Deprecation, DocLink}`; `ir::kind::{Kind, KindDiscriminant}`
- `ir::skeleton::{trait_impl_skeleton, function_signature_skeleton, type_fingerprint}`; `ir::intro::{bootstrap_intro_id_v2, sig_key}`
- `ir::registry::{Registry, RegistryResolver, AsyncRegistryResolver, RegistryState, ResolveError, ProductionEntryId}` (defined; minimal live use — safe to evolve to lazy/fallible as long as names hold)

Coupling depth: **ir-vcs** (deep — change + all wire twins + skeleton + intro + ascii + PristineIntroTable) ≫ **registry** (IrView/Occurrence + wire introspection + vocab) > **index** (IrView/Occurrence + vocab) > **driver** (IntroId + BodyEmbed + ReferenceKind).

---

## 6. Testing & validation

- **Wire/id parity (P2–P4):** golden byte snapshots of sealed triples, `KindWire`/`SymbolWire`
  postcard, `ascii` F1 encodings, `IntroId`/`payload_hash`/`GenerationStamp` for a fixed fixture,
  captured from `workspace/ir` at P0 and asserted identical after the fold.
- **Id-semantics (I3):** port every test in `workspace/ir/builder.rs` (unique-fn stability,
  overload flip, impl→TraitImpl, distinct intros) verbatim.
- **Consumer build gates (P5):** `ir-vcs`/`registry`/`index`/`driver` compile + their existing
  suites (continuity, checkpoint, ref_probes, size_tests, trustfall adapter) pass.
- **Producer parity (P8):** the `tests/snap_*.rs` insta snapshots for all compile targets +
  renderers must reproduce (this is the end-to-end oracle that richness survived).
- **Round-trip (all phases):** `serde`/postcard/ascii serialize→deserialize identity on every kind.

---

## 7. Risks & mitigations

| Risk | Mitigation |
|------|------------|
| Frozen-wire drift while merging twins | P0 golden snapshots + I1 gate every phase; derive twins, never hand-edit discriminants. |
| `ir-vcs` deep coupling breaks subtly | Keep §6 surface byte-for-byte; cut over at P5 with the full `ir-vcs` suite as the gate; `PristineIntroTable` API frozen. |
| Types-as-entries vs inline structural mismatch | Wire stays inline+`IntroId`-leaf (frozen); `EntryIdx` nominal refs are *in-memory only*, lowered at seal. No wire change. |
| Producers target a dead API; big rewrite | They're already broken/detached, so no regression risk; land the combinator layer (P6) before touching them (P8); snapshot parity per language. |
| Nightly features (`decl_macro`) surprise the workspace | P0 pins the toolchain; if undesirable, fall back to a `proc-macro` `Visitor`/`register_kinds` (mechanical, isolated to 2 files). |
| Generics subsystem scope creep (P7) | Model exactly what the 7 producers emit today (const-expr/variance/receiver/trait-method); defer anything not exercised by a snapshot. |
| Registry redesign (lazy+fallible) churn | Few live callers; keep trait names; land behind the same `Registry<R>`/`RegistryResolver` façade. |

## 8. Open sub-decisions (non-blocking; defaults chosen)

- **Directory:** keep `crates/nudox-ir` vs rename to `crates/ir`. *Default: rename to `crates/ir`* (matches pkg name `ir`, makes the original "crates/ir" literal true).
- **`Visitor`/`register_kinds` as decl-macro vs proc-macro.** *Default: keep decl-macro* (already written, zero build-graph cost) unless P0 finds the workspace must stay stable.
- **Combinator layer home:** in-crate `ir::lower` vs a sibling `ir-lower` crate. *Default: in-crate* module behind a `producer` feature so `ir-vcs`/`registry` don't compile it.
- **P7 vs P8 ordering:** generics subsystem can land lazily per-producer if a language needs less. *Default: P7 before P8* so producers target a stable kind set.

## 9. Appendix — source → destination map (abridged)

| From | To | Note |
|------|----|----|
| `crates/nudox-ir/{visitor,kind,index,entry/*,id/*}` | home `{visitor,kind,index,entry,change/ids}` | spirit core; `id::UniqueId`→`change` |
| `crates/nudox-ir/{registry/*,package/*}` | home `{registry,builder}` | keep lazy store; rekey on IntroId; fallible |
| `workspace/ir/{kind,wire,symbol,skeleton,intro,serialize,ascii}` | home same | frozen wire + rich kinds (derive twins) |
| `workspace/ir/{change,manifest,apply,view,reflect,vocab,body,body_wire}` | home same | move + rewire imports |
| `workspace/ir/builder.rs::seal_payloads` | home `builder/seal.rs` | id semantics unchanged (I3) |
| legacy `ir::{generics,protocols,ty}` (producer-side) | home `kinds/{generics,protocols,ty}` | restored richness (P7) |
| `workspace/compiler/compile/*` | rewritten onto `ir::lower` grammars | P8; delete `Entry::*`/`NudoxPath`/`Index` |
| `workspace/ir` (whole) | **deleted** | P9 |
```
