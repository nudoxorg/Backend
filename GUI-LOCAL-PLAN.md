# GUI-LOCAL-PLAN.md — lindsey Rev 2 addendum: the local-first planes

> **Status:** Frozen contract, 2026-07-27. **Extends** `GUI-PLAN.md` Rev 1; does not replace it.
> Everything in GUI-PLAN Parts I–VIII still holds. This document replaces exactly three things:
>
> | GUI-PLAN said | Rev 2 says |
> |---|---|
> | §7.1 `ClientHandle` is a stub over today's axum HTTP backend, deleted at Wave 4 | `EngineHandle` is a **real local-first engine** (`crates/nudox-engine`) over `nudox-ir`. There is no stub and nothing to delete. |
> | §9.3 M5 events come from a client-side chunker over `SymbolEntry` JSON | Events come from a chunker over **`nudox_ir::Entry`** — typed all the way down, no JSON, no `serde_json::Value`. |
> | (nothing) | A **query plane** (`crates/nudox-graph`, async Trustfall) and a **hosted MCP server** (`crates/nudox-mcp`) are first-class product surfaces. |
>
> Standing decisions LD-1…LD-20 are inherited verbatim. New ones are LR-1…LR-12 (§L1).

---

## §L0 The stack

```
┌ lindsey (workspace/gui) ─────────── GPUI shell, GUI-PLAN Parts I–VI ────────┐
│  views · stores · motion · theme · bridge          NO knowledge of IR types │
│                     consumes: EngineHandle + the wire protocol only         │
└───────────────────────────────┬─────────────────────────────────────────────┘
                                │ flume, bounded (GUI-PLAN Appendix C)
┌───────────────────────────────▼──── crates/nudox-engine ────────────────────┐
│  L3  Engine · EngineHandle · the wire protocol (SymbolHead/DocEvent/…)      │
│      the chunker: nudox_ir::Entry ──► SymbolHead + RenderSection stream     │
│      owns the Tokio runtime and the single-threaded query LocalSet          │
└──────────┬──────────────────────────────────────┬───────────────────────────┘
           │                                      │
┌──────────▼──── crates/nudox-graph ───┐  ┌────────▼──── crates/nudox-store ───┐
│  L2  schema.graphql · Vertex         │  │  L1  Corpus · PackageView          │
│      CorpusAdapter: AsyncAdapter     │──►      IrSource trait (fixtures /    │
│      typed queries (TryIntoStruct)   │  │      producers) · derived indexes  │
└──────────▲───────────────────────────┘  └────────┬───────────────────────────┘
           │                                       │
┌──────────┴──── crates/nudox-mcp ─────┐  ┌────────▼──── crates/nudox-ir ──────┐
│  L4  rmcp server, streamable HTTP    │  │  L0  IrView · Entry · Kind · Type  │
│      hosted by lindsey on localhost  │  │      IntroId · StableRef · Relation│
└──────────────────────────────────────┘  └────────────────────────────────────┘
```

**Dependency law (reviewable, one line):** arrows point down and left only.
`lindsey → engine → {graph, store} → ir`; `mcp → {engine, graph}`; `graph → store → ir`.
`lindsey` must not depend on `nudox-ir`, `nudox-store` or `nudox-graph` directly, and
`nudox-store`/`nudox-graph`/`nudox-engine` must not depend on `gpui`. Both directions are
enforced by `scripts/lint-gui-no-block.sh` (§L9).

---

## §L1 Standing decisions (LR-1…LR-12)

- **LR-1 One key, everywhere.** The GUI's `SymbolKey` **is** `nudox_ir::change::StableRef`
  (package lineage + `IntroId`). No parallel id type is invented at any layer. It is the
  MCP tool argument, the Trustfall vertex id, the nav-history entry, and the tab key.
- **LR-2 No `serde_json::Value` above L0.** Every payload crossing every seam is a named
  Rust type. LD-7's ban on `kind: Value` generalises: a `Value` anywhere in `engine`,
  `graph`, `mcp` or `lindsey` is a review-blocking finding. MCP tool *schemas* are derived
  from those types via `schemars`, never hand-written.
- **LR-3 The chunker is the only place IR becomes presentation.** `nudox_ir::Entry` →
  `SymbolHead` + `RenderSection` happens in exactly one module (`engine::chunk`). No view,
  no MCP tool, and no store re-derives a signature, a doc section, or a kind label.
- **LR-4 `SigToken` is produced once.** `engine::chunk::signature::tokens(&Entry) ->
  Vec<SigToken>` is the single source of signature rendering, consumed by the symbol page,
  search rows, quick peek, MCP output and the diff view (GUI-PLAN §11 `SignatureLine`,
  §12.3 `HitRow.sig_preview`, §22.2). Rendering a signature by string formatting anywhere
  else is a finding.
- **LR-5 Fixtures and producers are the same trait.** `IrSource` (§L3.2) has exactly two
  in-tree impls: `FixtureSource` (deterministic, `cfg(any(test, feature = "fixtures"))`) and
  `ProducerSource` (drives `nudox_producer::produce`). Screens are never written against a
  shape only fixtures can produce.
- **LR-6 The query plane is async and fallible.** `CorpusAdapter` implements
  `trustfall::provider::AsyncAdapter` from the pinned fork with a real
  `type Error = GraphError`. `unwrap`/`expect` inside adapter resolution is a finding —
  that is precisely what the fork's error channel exists for.
- **LR-7 One schema.** `crates/nudox-graph/schema.graphql` is the only schema. `nudox-mcp`
  serves it to agents verbatim; the GUI's saved queries are checked against it at build time
  by a test that parses every `.trustfall` file in `queries/`.
- **LR-8 MCP is a view of the engine, never a second engine.** Every MCP tool bottoms out in
  an `EngineHandle` call or a `CorpusAdapter` query. A tool that reads `nudox-ir` directly is
  a finding.
- **LR-9 The engine owns every runtime.** One Tokio multi-thread runtime for I/O and
  producers, one `LocalSet` for query execution (the fork's result stream is `!Send`), one
  `rayon`-free CPU path (producers block on the Tokio blocking pool). `lindsey` links no
  async runtime of its own; GUI-PLAN LD-2's three-runtime topology is unchanged, with the
  "client runtime" now being the engine's.
- **LR-10 Local is the truth; remote is an enrichment that may be absent.** Every screen must
  render correctly with zero network. `Provenance::TrustedLocal` is the default, not the
  exception. No code path awaits a remote answer before painting a local one.
- **LR-11 Strong typestates over booleans.** Sealed/unsealed IR, loaded/unloaded packages,
  authorized/unauthorized MCP sessions are distinct types, not flags. If two states have
  different legal operations, they are different types.
- **LR-12 Every public type in L1–L3 is `#[non_exhaustive]` if it is a wire enum**, and
  carries an `Unknown`/`Other` variant if it crosses a version boundary (GUI-PLAN LD-7).

---

## §L2 The wire protocol (crate `nudox-engine`, module `wire`)

This is GUI-PLAN §9.2/§9.3 made concrete against `nudox-ir`. It is the **entire** vocabulary
`lindsey` knows. `nudox-engine::wire` has no dependency on `gpui`; `SharedStr` is a
`triomphe::Arc<str>` newtype that converts into `gpui::SharedString` behind a
`lindsey`-side `From` impl, so the wire crate stays GUI-free.

### §L2.1 Identity & provenance

```rust
/// LR-1: the one key. Re-exported, not redefined.
pub use nudox_ir::change::{IntroId, PackageLineageId, StableRef as SymbolKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Provenance {
    /// Produced on this machine from source we can see. GUI-PLAN §10.4 `trust.local`.
    TrustedLocal,
    /// Fetched and verified from a remote generation. `trust.synced`.
    SyncedLocal { generation: GenerationId },
    /// Served remotely, not yet materialised. `trust.remote`.
    Remote { generation: GenerationId },
    /// Last-known-good, served while offline. `trust.stale`.
    Stale { as_of: SystemTime },
}
```

### §L2.2 Head, sections, tokens

Shapes are GUI-PLAN §9.2 verbatim with three substitutions:

- `SymbolKindV1` → `nudox_ir::kind::KindDiscriminant` (13 frozen variants, already `repr(u16)`
  with `from_u16` — a better closed enum than the plan's invented one, so LD-7's
  `Unknown(String)` becomes `KindTag::Unknown(u16)` for forward compatibility).
- `VisibilityV1` → `nudox_ir::entry::Visibility`.
- `SigToken::Ty { target: Option<SymbolKey> }` — `target` is `Some` exactly when the
  underlying `Type::Nominal(RawRef)` resolves through the corpus (LR-4).

`SectionKind` is derived from the `Entry`, not guessed:

| `RenderSection` variant | Sourced from |
|---|---|
| `Prose` | `Symbol::documentation` parsed by `pulldown-cmark`, split at H2 |
| `CodeBlock` | fenced blocks inside that markdown; `lang` from the fence info string |
| `Members` | `IrView::children_of(intro)` filtered to `Module`/`Trait`/`Impl` children |
| `Fields` | `children_of` filtered to `Kind::Field` / `Kind::Variant` |
| `Examples` | prose sections whose heading matches `^Examples?$` |
| `Callout` | markdown blockquotes with a `[!NOTE]`-style lead |
| `Unknown` | anything a future producer adds — renders as a chip, never a panic |

### §L2.3 Streams

`DocEvent`, `SearchEvent`, `PackageEvent`, `ProjectEvent`, `JobEvent`, `SyncEvent` are
GUI-PLAN §9.3 / §7.1 verbatim. Two additions:

```rust
#[non_exhaustive]
pub enum SearchEvent {
    /// Local name/type hits. MUST be emitted before any semantic work starts (LR-10).
    Section { generation: Gen, section: SearchSectionId, rows: Arc<[HitRow]> },
    Merge   { generation: Gen, section: SearchSectionId, rows: Arc<[HitRow]> },
    Latency { generation: Gen, section: SearchSectionId, elapsed: Duration },
    Done    { generation: Gen },
    Failed  { generation: Gen, error: EngineError },
}

/// NEW in Rev 2 — the query plane surfaced to the GUI and to MCP alike.
#[non_exhaustive]
pub enum QueryEvent {
    /// Column order, sent once, before any row.
    Columns { generation: Gen, columns: Arc<[SharedStr]> },
    Rows    { generation: Gen, rows: Arc<[QueryRow]> },
    Done    { generation: Gen, total: u64 },
    Failed  { generation: Gen, error: EngineError },
}
```

**Protocol invariants** (GUI-PLAN §9.3, property-tested): `Head` first; `Done`/`Failed`
terminal; sections in `section_plan` order; `Highlight` only for already-sent sections;
applying any prefix of a valid stream yields a valid page. `QueryEvent::Columns` precedes
any `Rows`.

---

## §L3 `crates/nudox-store` — the local corpus

### §L3.1 Types

```rust
/// One package's sealed IR plus everything derived from it. Immutable once built.
pub struct PackageView {
    view:      IrView,
    provenance: Provenance,
    indexes:   PackageIndexes,
}

/// Derived, rebuildable, never persisted-as-truth (GD-34: disposable projections).
pub struct PackageIndexes {
    /// Case-folded name → introductions, for prefix and fuzzy search.
    by_name:     NameIndex,
    /// `KindDiscriminant` → introductions, for kind facets.
    by_kind:     EnumMap<KindDiscriminant, Vec<IntroId>>,
    /// Reverse occurrence postings: target `StableRef` → owners. Confidence ≥ Index.
    usages:      PostingList<StableRef, IntroId>,
    /// Reverse type-mention postings, from `Kind::Impl::of` and `Kind::Trait::supers`.
    mentions:    PostingList<StableRef, IntroId>,
    /// Fully-qualified path per intro, precomputed once (never re-walked per query).
    paths:       HashMap<IntroId, SharedStr>,
}

/// The whole local world. Cheap to clone (Arc inside), safe to share across runtimes.
#[derive(Clone)]
pub struct Corpus { inner: Arc<CorpusInner> }

impl Corpus {
    pub fn package(&self, id: &PackageLineageId) -> Option<Arc<PackageView>>;
    pub fn entry(&self, key: &SymbolKey) -> Option<EntryRef<'_>>;
    pub fn packages(&self) -> impl Iterator<Item = Arc<PackageView>> + '_;
}
```

`ReversePositionIndex` in `workspace/registry/graph/reverse_index.rs` is the prior art for
`usages`/`mentions` — port its construction rules (confidence floor, sort+dedup,
`typerefs_of_entry`) rather than reinventing them, then delete the original (§L8).

### §L3.2 `IrSource` — LR-5

```rust
/// Where `IrView`s come from. Async and streaming: a 250-package workspace must
/// surface its first package without waiting for the last.
pub trait IrSource: Send + Sync + 'static {
    fn describe(&self) -> SourceDescriptor;

    fn load(&self, req: LoadRequest)
        -> BoxStream<'static, Result<LoadEvent, SourceError>>;
}

#[non_exhaustive]
pub enum LoadEvent {
    /// Emitted as soon as the package is identified, before its IR is built.
    Discovered { lineage: PackageLineageId, hint: PackageHint },
    /// Progress within one package's production (drives GUI-PLAN §19 pipeline dots).
    Progress   { lineage: PackageLineageId, stage: ProduceStage, done: u32, total: u32 },
    /// The package is sealed and queryable.
    Ready      { package: Arc<PackageView> },
    /// This package failed; others continue. Never aborts the stream.
    Failed     { lineage: PackageLineageId, error: SourceError },
}
```

Impls:

- **`FixtureSource`** — `cfg(any(test, feature = "fixtures"))`. Builds `IrView`s with
  `IrPackage::build` (deterministic ids, no clock, no filesystem) so every kernel and
  screen test is reproducible. Ships a ~200-symbol synthetic package plus a 10 000-symbol
  one for the `typing_storm` gate.
- **`ProducerSource`** — wraps `nudox_producer::produce::<P>` on the Tokio blocking pool.
  Rust (`nudox-producer-rust`, in-process `ra_ap`) is the pilot and needs no external
  toolchain; the other six register behind the same `ProducerRegistry` keyed by
  `nudox_ir::body::Language`, and report `SourceError::ToolchainMissing` rather than
  failing the whole load when their oracle is absent (LR-10).

---

## §L4 `crates/nudox-graph` — the query plane

### §L4.1 Vertex

```rust
/// Strongly typed, no `Value`, no stringly-typed discriminants (LR-2).
#[derive(Debug, Clone, TrustfallEnumVertex)]
pub enum Vertex {
    Package(Arc<PackageView>),
    Symbol(SymbolVertex),      // { package: Arc<PackageView>, intro: IntroId }
    Field(SymbolVertex),
    Function(SymbolVertex),
    Type(TypeVertex),          // a resolved `nudox_ir::kinds::Type` node
    Occurrence(OccurrenceVertex),
    Relation(RelationVertex),
    Span(SpanVertex),
}
```

`SymbolVertex` holds `Arc<PackageView>` + `IntroId` — never a borrowed `&Entry`, so the
vertex is `'static` and the adapter's lifetime parameter stays free. This is what lets the
adapter be async: an `AsyncAdapter` future may outlive any single borrow of the corpus.

### §L4.2 Adapter

```rust
pub struct CorpusAdapter { corpus: Corpus }

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphError {
    #[error("package {0} is not loaded")]         PackageNotLoaded(PackageLineageId),
    #[error("symbol {0} not found")]              SymbolNotFound(IntroId),
    #[error("edge {edge} is not valid on {ty}")]  BadEdge { ty: String, edge: String },
    #[error(transparent)]                          Source(#[from] SourceError),
}

impl<'a> AsyncAdapter<'a> for CorpusAdapter {
    type Vertex = Vertex;
    type Error  = GraphError;
    // resolve_starting_vertices / resolve_property / resolve_neighbors / resolve_coercion
}
```

Dependency, pinned to the fork **by rev** (LD-10 discipline extends to it):

```toml
trustfall = { git = "https://github.com/philocalyst/trustfall", branch = "feat/adapter-error-handling", features = ["async"] }
```

The fork gives us exactly two things we need and upstream 0.8.1 does not:
a real `Adapter::Error` channel (so LR-6 is expressible) and `AsyncAdapter` /
`execute_query_async`, whose result stream is `Pin<Box<dyn Stream>>` — **not `Send`**.
That non-`Send`ness is why LR-9 mandates a `LocalSet`; it is a design constraint, not an
oversight, and it must not be "fixed" by blocking the caller.

### §L4.3 Schema and typed results

`schema.graphql` covers, at minimum: `Package`, `Symbol` (interface) with
`Function`/`Record`/`Trait`/`Impl`/`Enum`/`Field`/`Const`/`Alias` implementors, `Type`,
`Occurrence`, `Relation`, `Span`; edges `members`, `parent`, `usages`, `mentions`,
`implements`, `supertraits`, `signatureTypes`, `occurrencesOf`. Properties are scalars only.

Named queries live in `queries/*.trustfall` beside a `#[derive(Deserialize)]` result struct
consumed through `trustfall::TryIntoStruct` — so a query's shape and its Rust type are
checked together by a test that runs every named query against the fixture corpus.

---

## §L5 `crates/nudox-engine` — the streaming facade

`EngineHandle` is GUI-PLAN §7.1 verbatim, plus:

```rust
impl EngineHandle {
    /// NEW: the query plane. Same slot/gen/cancel discipline as every other call.
    pub fn query(&self, q: GraphQuery, generation: Gen) -> (StreamHandle, flume::Receiver<QueryEvent>);
    /// NEW: the schema, for the MCP server and the in-app query editor.
    pub fn schema(&self) -> &'static Schema;
}
```

Modules: `runtime` (LR-9), `wire` (§L2), `chunk` (LR-3/LR-4: `head.rs`, `signature.rs`,
`sections.rs`, `highlight.rs`), `search` (name/type/semantic fan-out over `PackageIndexes`),
`query` (drives `CorpusAdapter` on the `LocalSet`, coalesces rows into `QueryEvent::Rows`),
`project` (workspace discovery → `IrSource` selection).

**The chunker is the crown jewel and gets written first**, because §L2.2's table is what
makes GUI-PLAN §9.4's zero-jump guarantee provable: `section_plan` `SizeHint`s are computed
from the same walk that later emits the sections, so the skeleton geometry is *derived*, not
estimated.

---

## §L6 `crates/nudox-mcp` — the hosted server

- `rmcp = "2.2"`, transport **streamable HTTP bound to `127.0.0.1:0`** (ephemeral port, so
  two lindseys never collide). stdio is impossible: lindsey is a GUI and does not own stdio.
- Lifecycle: started by lindsey after the engine, stopped on window close. The bound port and
  a copy-able client config snippet are shown in Settings → Connection (GUI-PLAN §21).
- Local-only by construction: bind address is loopback and non-configurable in v1; a session
  token is generated per launch and required on every request (LR-11: `Unauthenticated`
  and `Session` are different types).
- Tools (all arguments and results are `schemars`-derived from §L2 types — LR-2):

  | Tool | Signature | Bottoms out in |
  |---|---|---|
  | `search_symbols` | `(query, kinds?, packages?, limit?) -> Vec<HitRow>` | `EngineHandle::search` |
  | `get_symbol` | `(key: SymbolKey) -> SymbolDoc` | `EngineHandle::open_symbol`, drained |
  | `find_usages` | `(key, limit?) -> Vec<UsageRow>` | `PackageIndexes::usages` |
  | `list_packages` | `() -> Vec<PackageSummary>` | `Corpus::packages` |
  | `graph_query` | `(query: String, args) -> QueryResult` | `CorpusAdapter` |
  | `graph_schema` | `() -> String` | `EngineHandle::schema` |

  `graph_query` is the escape hatch; the five typed tools are what agents should reach for
  first, and their descriptions say so.
- Resources: the schema, and one resource per loaded package.

---

## §L7 Strong-typing doctrine (how "STRONGLY TYPED" is judged in review)

1. **No stringly-typed anything across a seam.** Kinds are `KindDiscriminant`; languages are
   `nudox_ir::body::Language`; section ids are `SectionId(u32)`; tab ids are `TabId(NonZeroU32)`.
2. **Newtype every id.** `Gen`, `SectionId`, `TabId`, `GenerationId`, `JobId`, `SearchSectionId`
   are distinct newtypes. A function taking two `u64`s in a row is a finding.
3. **Make illegal states unrepresentable.** `StreamSlot::display()` (GUI-PLAN §8.1) is the
   model: the compiler enumerates the states, so a view cannot forget one. Apply the same
   shape to `Phase`, `LoadEvent`, `Provenance`, MCP session state.
4. **Errors are enums, never `anyhow` above L1.** `anyhow` is allowed in `main.rs` and tests
   only. `SourceError`, `GraphError`, `EngineError`, `McpError` are `thiserror` enums, and
   `EngineError` is what reaches the GUI so `ErrorState` can branch on it (LD-16).
5. **`#[non_exhaustive]` on every wire enum** (LR-12), so adding a producer or a section kind
   is not a breaking change.
6. **No `as` casts on domain values**, no `unwrap` outside tests and `main.rs`, no
   `#[allow(dead_code)]` left behind by a subagent.

---

## §L8 Backend convergence (the DRY debt this pays down)

`workspace/registry/graph/` today holds a `GraphVertex` enum, five neighbour methods, a
`ReversePositionIndex`, and an `execute_graph_query` that always returns
`Unsupported`. Per the frozen decision, once `nudox-graph` passes its fixture-corpus tests:

1. Port `reverse_index.rs`'s construction rules into `nudox-store::PackageIndexes`
   (they are correct — confidence floor, sort+dedup, `typerefs_of_entry`).
2. Repoint `workspace/index/search/usages.rs`'s `ReverseIndexUsageBackend` at
   `nudox_store::PackageIndexes`.
3. Delete `workspace/registry/graph/{trustfall_adapter,reverse_index}.rs` and the
   `trustfall = "=0.8.1"` pin from `workspace/registry/Cargo.toml`.
4. Wire `POST /query` in `workspace/driver` to `CorpusAdapter`, retiring the stub.

This is **M6 work, sequenced after the GUI slice lands** — it touches the backend build and
must not block the GUI milestones.

---

## §L9 Enforcement

`scripts/lint-gui-no-block.sh` grows from GUI-PLAN §25.4's deny-list to cover the new laws:

| Rule | Check |
|---|---|
| LD-2/LR-9 foreground purity | deny-list grep in `workspace/gui/src` (`std::fs::`, `block_on`, `reqwest`, `tokio::`, `.recv()`, `std::thread::sleep`) |
| §L0 dependency law | `cargo metadata` assertion: `lindsey` has no edge to `nudox-ir`/`-store`/`-graph`; those three have no edge to `gpui` |
| LR-2 no `Value` | grep `serde_json::Value` outside `nudox-ir` and MCP transport internals |
| LR-4 one signature renderer | grep `format!` in `lindsey`'s `render` bodies |
| LD-18 owned tasks | grep `.detach()` outside `main.rs` |
| LR-7 one schema | test: every `queries/*.trustfall` parses against `schema.graphql` |
| §L7.4 error discipline | grep `anyhow::` in `nudox-{store,graph,engine,mcp}` outside tests |

---

## §L10 Revised milestone map

GUI-PLAN §27's M0–M10 are re-based; the numbering is preserved so both documents stay
readable together. **This run targets M0 → M5.**

| Milestone | GUI-PLAN content | Rev 2 additions |
|---|---|---|
| **M0** Floor | §26 skeleton, pins, lint script | crate skeletons for store/graph/engine/mcp; the four bugs are moot (legacy code is not ported) |
| **M1** Motion kernel | §4–§6 verbatim | — |
| **M2** Async kernel + **engine** | §7–§8 bridge | `nudox-store` (+`IrSource`, fixtures **and** producer impl — LR-5), `nudox-engine` runtime + wire + chunker. **Replaces `client_stub` entirely.** |
| **M2.5** Query plane + MCP | *(new)* | `nudox-graph` schema/vertex/adapter/typed queries; `nudox-mcp` server, hosted by lindsey |
| **M3** Shell | §13 | status bar shows the MCP port and the loaded-package count |
| **M4** Search | §15 | search runs over `PackageIndexes`, not HTTP |
| **M5** Symbol page | §9 + §16 | sections come from the chunker over real `Entry`s |
| M6–M10 | §17–§22 | plus §L8 backend convergence |

Acceptance for every milestone is GUI-PLAN's, **plus**: `cargo test` green across all five
crates, `scripts/lint-gui-no-block.sh` green, and a screenshot of the running app attached to
the milestone note.

---

*End of GUI-LOCAL-PLAN Rev 2. Read with `GUI-PLAN.md`; where they disagree, the table at the
top of this file governs and nothing else does.*
