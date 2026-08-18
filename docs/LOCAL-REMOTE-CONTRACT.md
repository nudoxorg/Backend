# The Surface Contract — one streaming shape for local and remote

> **One line.** The local engine already speaks a good streaming protocol and the
> GUI already has an excellent consumer for it; the remote speaks a buffered
> `Vec` and the GUI has a *second, parallel* consumer for that. Collapse them:
> make **`Surface`** the unit of the contract (it ties request → item → metadata
> → identity at compile time), make **`Answer<S>`** the one thing every backend
> returns, and give `Serve<S>` three impls — local, remote, and a federating
> merge — so a caller holding `Arc<dyn Serve<S>>` genuinely cannot tell which it
> has. Streaming stops being a property of one route and becomes the shape of
> the contract.

This supersedes the transport half of [`LOCAL-REMOTE-PLAN.md`](LOCAL-REMOTE-PLAN.md)
(the Residence model). That document is right about *where data lives*; it says
nothing about *how answers arrive*, and every seam it proposes returns
`Result<T>` / `Page<T>` / `Vec<T>`. §6 below folds its `Residence` type in as a
per-item tag rather than a separate plane.

---

## 0. What is actually there today (measured, not assumed)

### 0.1 The local engine is already fully streaming — and good at it

`EngineHandle` has 18 public methods. All but two (`versions`, `diff_versions`)
return immediately with `(StreamHandle, flume::Receiver<Event>)`. Dropping the
`StreamHandle` cancels the work. Every event carries a `Gen` so a superseded
query's results are dropped before they touch state.

The GUI consumer built on top of this is the strongest part of the system:

| Mechanism | Where |
|---|---|
| Batched drain — await one event, `try_recv` the rest, one `cx.notify()` | `gui/src/bridge/drain.rs:86` |
| Cancellation by ownership — new handle drops the predecessor | `gui/src/bridge/handle.rs` |
| Generation guard on *every* event arm | `gui/src/stores/search.rs:488` |
| `Progressive<T>` — append-only, no `IndexMut`, no `remove` | `gui/src/bridge/progressive.rs` |
| Scroll-stable in-place remeasure of newly-arrived sections only | `gui/src/views/symbol_page/docs.rs:791` |
| 7-state `Phase`/`Display` truth table (skeleton/stale/partial/fresh/error) | `gui/src/bridge/slot.rs:12` |

Search renders three sections (Name/Type/Semantic) independently; a slow
semantic section never blocks the name section, and `SectionStatus::Building
{covered,total}` renders **real partial rows** plus a caveat rather than a
spinner.

### 0.2 The remote is a buffered `Vec` wearing a streaming costume

The wire format is genuinely a typed NDJSON stream — `heart::stream::StreamFrame<T>`
(`Hit | Error | End`) is well-designed, and the terminal-frame rule makes a
truncated stream a typed error instead of a silent empty success. But the answer
is fully materialized at **three** points before anyone can render it:

```rust
// workspace/index/server/http/handlers/search.rs:112
let hits: Vec<Scored<Symbol>> = server
    .search_symbols(&cap, &request)
    .await?
    .try_collect()          // ← the stream dies here, in the handler
    .await?;
```

…even though the trait it just collected says otherwise:

```rust
// workspace/index/server/search/mod.rs:45
/// Run a search, streaming scored results (keyset-paginated via the request's
/// `page`). The stream is the contract — results are never collected here.
async fn search(&self, request: &Search<'_>)
    -> Result<impl Stream<Item = Result<Scored<Self::Item>, Self::Error>> + Send, Self::Error>;
```

Then `hit_stream(hits)` re-streams the already-complete `Vec` through
`futures::stream::iter` → `Body::from_stream`. The bytes chunk; nothing arrives
earlier. The handler's own doc comment is honest about it: *"Because hits are
already collected before this is called…"* and *"for a future truly-streaming
writer."*

Then the client un-streams it again:

```rust
// workspace/heart/client/http.rs:174-177  (`checked`)
let body = response.text().await?;      // ← the whole body, buffered
// …then http.rs:198  read_hit_stream(&body) -> Vec<Scored<T>>
```

`NudoxClient::search` returns `Result<Vec<Scored<Symbol>>, Error>`. No
generation, no cancellation, no sections, no progressive refinement — none of
the vocabulary the local path has.

The federation merge is a fourth collection point: `merge_overlay_first`
(`index/server/search/mod.rs:72`) takes `IntoIterator<Item = Vec<Scored<T>>>`,
so per-source streams must be collected before they can be merged.

### 0.3 So the GUI has two parallel state machines for one concept

Because remote can't speak the streaming shape, the GUI's remote path bypasses
every mechanism in §0.1. It is a **button** in the zero-hit state
(`omni_search.rs:2417`), it spins up a throwaway current-thread Tokio runtime on
a background thread per request because reqwest panics without a reactor
(`stores/search.rs:419-445`), and the answer lands in a separate `RemoteStatus`
field with a separate `RemoteFailure` enum and separate UI treatment. The
crate's own `Cargo.toml` states the intent: *"a remote server … contributes a
second, clearly-labelled result group."*

That is the opposite of transparent, and it is not a UI decision that can be
reversed above the wire — the information and the shape simply aren't there.

### 0.4 Twelve streaming envelopes, none related by type

| Envelope | Where |
|---|---|
| `StreamFrame<T>` `{Hit, Error, End}` | `heart/stream.rs:56` |
| `DocEvent`, `SearchEvent`, `QueryEvent`, `VersionEvent` | `nudox-engine/src/wire/mod.rs` |
| `PackageLoadEvent`, `PackageEvent`, `ProjectEvent`, `SyncEvent`, `JobEvent` | `nudox-engine/src/lib.rs` |
| `IndexEvent` | `nudox-engine/src/packages/acquire/mod.rs:575` |
| `LoadEvent` | `nudox-engine/src/store/source.rs:178` |

Eleven of the twelve re-declare their own `Done`/`Failed` terminal pair. Nine
re-declare a `generation: Gen` field. The *payloads* genuinely differ and should
stay distinct; the *framing* is copied eleven times.

### 0.5 Residence is encoded six ways

`LOCAL-REMOTE-PLAN.md` §0.1 counted four. There are two more:

5. `heart::content::availability::{IrAvailability, ObjectAvailability}` — two
   structurally **identical** four-variant enums (`Local | Remote{store_id} |
   Pending | Missing`) differing only in doc comments.
6. `nudox_engine::wire::Provenance` — `TrustedLocal | SyncedLocal{gen} |
   Remote{gen} | Stale{as_of}`.

`Provenance` is the best of the six: it has GUI theming wired to all four
variants (`gui/src/theme/ext.rs:181`) and a rendering rule per variant. But it
is **dead in practice** — the only conversion from the store plane hardcodes it:

```rust
// workspace/nudox-engine/src/wire/mod.rs:1748-1755
impl From<crate::store::package::Provenance> for Provenance {
    fn from(p: crate::store::package::Provenance) -> Self {
        let _ = p;
        Provenance::TrustedLocal
    }
}
```

Across all of `nudox-engine/src`, `Provenance` is only ever constructed as
`TrustedLocal` (one `SyncedLocal` in `mcp/result_format.rs:1391`).
`Provenance::Remote` is constructed exactly once in the whole system — in the
GUI, at the call site that stitches in remote results
(`gui/src/stores/search_model.rs:443`), because the engine cannot tell it.

There are also two `Provenance` types (engine-wire with payloads, GUI-local
fieldless), converted in two places that **disagree on the `#[non_exhaustive]`
fallback**: `_ => Provenance::Remote` (`search_model.rs:719`) vs
`_ => Provenance::Stale` (`views/symbol_page/header.rs:117`).

### 0.6 The request type does not determine the response type

`heart::query::Query` is one struct with `target: Target`
(`Packages | Symbols | Usages{of}`). But `QueryEngine` has a single associated
`Hit` type, so the pairing is checked **at runtime**:

```rust
// workspace/index/server/http/handlers/search.rs:104
if query.target != Target::Symbols {
    return Err(BadRequestReason::MissingField { field: "target must be Symbols for /search" }.into());
}
```

Three routes, one request type, three unrelated response types, runtime
validation. Related consequences: `NudoxClient` implements **4 of 17** routes,
because each one needs a hand-written method; and `heart::client::dto` and
`index::server::http::dto` independently declare same-named, structurally
identical types (`AddPackageDto`, `HealthDto`, `CompiledLookupRequest`,
`RerankRequestDto`, `JobKeyHex`) with no shared definition.

### 0.7 Local and remote do not share a symbol identity

Found while wiring the surfaces, and it would have shipped silently.

`SymbolId` is minted by `EntryUri::symbol_id(instance_token)` as
`UUIDv5(SYMBOL_ns, instance_token ‖ 0 ‖ package_id/path…)`, and `instance_token`
is `"{org}/{db}"` read from **server configuration**
(`index/server/config.rs:527`). The salt is deliberate — the type's own doc says
it exists "so two instances of the same corpus don't share ids."

Correct for its purpose, fatal as a merge key. The federating merge suppresses a
duplicate across sources by comparing `Surface::Key`; if the local engine and a
remote server mint different ids for the same symbol, **dedup never fires** and
every remote hit renders as a second row beside its local twin. Nothing errors.
The answer is just quietly wrong — the failure mode most likely to survive
review, because it looks like "the remote found more results."

`Symbols::Key` is therefore `(PackageId, fully_qualified)`: both components are
computable independently by either plane, and neither is instance-salted —
`PackageId` is a deterministic UUIDv5 over package coordinates, and the
fully-qualified path is a property of the source rather than of whoever indexed
it. Pinned by three tests in `heart/tests/search_surfaces.rs` §2b, covering both
directions (two instances must agree; distinct symbols must stay distinct).

The general rule this is an instance of: **anything used as `Surface::Key` must
be derived only from facts every plane can compute independently.** A key that
depends on who did the indexing cannot federate.

---

## 1. The contract

### 1.1 `Surface` — request, item, metadata and identity, tied at compile time

A **surface** is one answerable question. It is the unit the whole contract is
generic over.

```rust
pub trait Surface: Send + Sync + 'static {
    /// Stable name, for tracing and for the `Note`/`Frame` schema registry.
    const NAME: &'static str;
    /// The remote route. Naming it here is what stops client and server from
    /// drifting: there is one string, not one per hand-written client method.
    const PATH: &'static str;

    /// What the caller asks. Serialized as the POST body verbatim.
    type Request: Serialize + DeserializeOwned + Send + Sync + Clone + 'static;

    /// One element of the answer.
    type Item: Serialize + DeserializeOwned + Send + Clone + 'static;

    /// Out-of-band metadata this surface emits alongside items: section states,
    /// column headers, latencies, page skeletons. `()` when there is none.
    type Note: Serialize + DeserializeOwned + Send + Clone + 'static;

    /// Per-item identity, for dedup and reconcile across sources.
    type Key: Eq + Hash + Send + 'static;

    fn key(item: &Self::Item) -> Self::Key;
}
```

`Note` is what generalizes `SearchEvent::{SectionState, Latency}`,
`DocEvent::{Head, Highlight}`, and `QueryEvent::Columns`. Those payloads are
genuinely different per surface and *should* stay distinct — only the framing
around them is duplicated today.

Concretely:

```rust
pub struct Symbols;
impl Surface for Symbols {
    const NAME: &'static str = "symbols";
    const PATH: &'static str = "/search";
    type Request = Query;              // heart::query::Query, unchanged for now
    type Item    = Scored<Symbol>;
    type Note    = SearchNote;         // SectionState | Latency | Coverage
    type Key     = SymbolId;
    fn key(item: &Self::Item) -> SymbolId { item.value.id }
}
```

The runtime check at `search.rs:104` becomes unrepresentable: you cannot hand a
`Packages` request to `Serve<Symbols>`.

### 1.2 `Frame<S>` — one envelope, replacing twelve

```rust
#[non_exhaustive]
pub enum Frame<S: Surface> {
    /// Surface-specific out-of-band metadata.
    Note(S::Note),
    /// One element of the answer, tagged with where it came from.
    Item(Located<S::Item>),
    /// A source dropped out. The answer CONTINUES and is still usable — this is
    /// not terminal. Local results stand when the remote is unreachable.
    Degraded(Degradation),
    /// Terminal, normal. Its presence is what distinguishes complete from
    /// truncated (the rule `heart::stream` already established).
    End(Summary),
    /// Terminal, failed. No answer at all.
    Failed(WireError),
}

pub struct Summary {
    pub items: u64,
    pub complete: Completeness,
}

/// Whether the answer covers what was asked. `Partial` is a first-class,
/// renderable outcome — not an error and not a silent truncation.
#[non_exhaustive]
pub enum Completeness {
    Complete,
    Partial { covered: u64, total: Option<u64> },
}

pub struct Degradation {
    pub source: SourceId,
    pub reason: WireError,
}
```

`Degraded` + `Completeness` are the generalization of the GUI's
`SectionStatus::Building{covered,total}` and its separate `RemoteStatus` — one
mechanism instead of two. `heart::stream::StreamFrame<T>` becomes the degenerate
case where `Note = ()` and no source degrades.

### 1.3 `Located<T>` — residence rides on the item

```rust
pub struct Located<T> {
    pub value: T,
    pub residence: Residence,
}

/// Where this datum came from, and how much to trust it. This is
/// `nudox_engine::wire::Provenance` promoted into `heart` — the variant set the
/// GUI already themes, now actually populated.
#[non_exhaustive]
pub enum Residence {
    /// Materialized here, produced from source we can see.
    Local,
    /// Fetched from a remote generation and content-verified.
    Synced { generation: GenerationId },
    /// Served remotely, not materialized locally.
    Remote { generation: GenerationId },
    /// Last-known-good, served while the source is unreachable.
    Stale { as_of: UnixMilliseconds },
}
```

This replaces all six encodings from §0.5. `IrAvailability` and
`ObjectAvailability` collapse into one `Residence` (their `Pending`/`Missing`
states belong to the fetch path, not to a delivered item — an item that is
`Missing` is simply not emitted).

Because residence is per-item rather than per-call, a single answer can
legitimately mix local and remote rows — which is precisely what "transparent"
requires, and what the current one-`Vec`-per-origin shape cannot express.

### 1.4 `Answer<S>` and `Serve<S>` — the one thing every backend returns

```rust
pub struct Answer<S: Surface> {
    rx: flume::Receiver<Frame<S>>,
    /// Drop = cancel. Same contract as the engine's existing `StreamHandle`.
    handle: StreamHandle,
}

impl<S: Surface> Answer<S> {
    pub async fn recv(&self) -> Option<Frame<S>>;          // async consumers
    pub fn try_recv(&self) -> Option<Frame<S>>;            // GUI batched drain
    pub fn into_stream(self) -> impl Stream<Item = Frame<S>>;  // axum / server
}

pub trait Serve<S: Surface>: Send + Sync + 'static {
    fn serve(&self, request: S::Request, generation: Gen) -> Answer<S>;
}
```

**Why `flume::Receiver` and not `impl Stream`.** This is the one non-obvious
choice, and it is forced by a real constraint rather than taste. lindsey has no
async reactor — GPUI's executor is the only runtime (LR-9), and reqwest's
futures panic outright without Tokio. A `flume::Receiver` is drainable from
GPUI's executor (`recv_async`) *and* convertible to a `Stream` for the axum
side (`into_stream`). It is a superset, and it is already what the local engine
uses and what `bridge/drain.rs` is built around — so this promotes a proven
shape rather than inventing one.

`Serve::serve` is **not** `async`. It returns immediately; the answer arrives on
the channel. That matters for the GUI, which cannot await a constructor on the
foreground thread.

---

## 2. Three impls — and transparency by construction

```rust
impl Serve<Symbols> for LocalEngine { … }        // per surface, as today
impl<S: Surface> Serve<S> for RemoteClient { … } // ONE blanket impl
impl<S, L, R> Serve<S> for Federated<L, R>       // ONE blanket impl
    where S: Surface, L: Serve<S>, R: Serve<S> { … }
```

The GUI holds `Arc<dyn Serve<Symbols>>` and cannot tell which it has. That is
the transparency — enforced by the type, not by discipline.

### 2.1 The remote client comes for free

`RemoteClient`'s blanket impl needs only `S::PATH`, `S::Request: Serialize`, and
`Frame<S>: DeserializeOwned`. Adding a surface therefore yields its remote client
with no new code — replacing today's hand-written 4-of-17 route coverage. And
because both sides name `S::Request`/`Frame<S>`, the duplicated
`heart::client::dto` ↔ `index::server::http::dto` type pairs (§0.6) have nothing
left to drift on.

### 2.2 The federating merge — the cleverness, written once

```rust
fn serve(&self, req: S::Request, gen: Gen) -> Answer<S> {
    // 1. Start local AND remote immediately. Not a local-then-remote ladder —
    //    that ladder is pure added latency when both would have answered.
    // 2. Forward each side's Items as they arrive, in arrival order.
    // 3. Suppress a remote Item whose S::key a local Item already claimed,
    //    UNLESS the remote generation is higher — the "local progressed past
    //    remote" / "remote is ahead" reconcile, per LOCAL-REMOTE-PLAN §6.
    // 4. A remote failure emits Frame::Degraded, NOT Frame::Failed. The local
    //    answer stands; End carries Completeness::Partial.
    // 5. End only once both sides have ended.
}
```

Written once, generic over `S`, it serves every surface. Point 4 is the
behavioural fix that makes offline-first honest: today a remote failure is a
separate `RemoteStatus` field the view has to remember to render; here it is a
frame on a still-successful stream, and `Completeness` makes forgetting it
impossible to do silently.

This is also where `LOCAL-REMOTE-PLAN`'s `Reconcile` lattice-join lives — as the
step-3 predicate, not as a separate plane.

---

## 3. Unbuffering the three collection points

The types above are inert until the buffers are removed. All three are small,
localized edits; one is genuinely hard.

**(a) The handler — `search.rs:112`.** Delete `.try_collect()`; map the source
stream to `Frame`s straight into `Body::from_stream`. Trivial *once* (b) lands.

**(b) The federation merge — `merge_overlay_first`, `search/mod.rs:72`.** This
is the hard one. It takes `Vec`s, so it forces (a). Replace with a streaming
k-way merge: pull the current head of each source, emit the best, advance that
source. This requires each source to yield in descending score order — tantivy
and qdrant both do, so it is achievable, but it is real work and it is the
critical path for first-byte latency. Overlay-override becomes a `HashSet<S::Key>`
threaded through the merge instead of a pre-pass.

**(c) The client — `http.rs:174`.** Replace `response.text()` with
`bytes_stream()` plus a line-framing decoder that emits `Frame<S>` as lines
complete. The terminal-frame discipline in `read_hit_stream` carries over
unchanged — it just runs incrementally.

**(d) The GUI runtime.** Replace the per-request throwaway Tokio runtime with
one long-lived current-thread runtime on a dedicated thread that owns the
reqwest client and pumps `Frame<S>` into a flume channel. One thread for the
app's lifetime instead of one per remote query.

---

## 4. Roadmap

| Phase | Deliverable | Risk |
|---|---|---|
| **S0** | `heart::surface`: `Surface`, `Frame`, `Located`, `Residence`, `Summary`, `Completeness`, `Degradation`, `Answer`, `Serve`. Define `Symbols`/`Packages`/`Usages` surfaces with `Request = Query` unchanged. Nothing consumes it. | None — pure addition, no wire change |
| **S1** | Blanket `impl<S: Surface> Serve<S> for RemoteClient` + incremental `bytes_stream` decoding (3c) + GUI runtime thread (3d). `NudoxClient::search` becomes a shim over it. | Low. **Remote genuinely streams from here on** |
| **S2** | Streaming k-way merge replacing `merge_overlay_first` (3b), then drop `try_collect` (3a). | **Highest.** Needs sorted per-source streams; this is the real work |
| **S3** | Port `EngineHandle::{search, open_symbol, query}` onto `Serve<S>`. The 11 event enums become `Frame<S>` + 11 `Note` types. `store::package::Provenance` → real `Residence` instead of the hardcoded `TrustedLocal`. | Medium — mechanical but wide |
| **S4** | `Federated<Local, Remote>`; GUI holds `Arc<dyn Serve<Symbols>>`. Delete the remote-search button, `RemoteStatus`, `RemoteFailure`, and the duplicated `Provenance` conversions (including the two that disagree on their fallback). | Medium — this is where the UX changes |

S0–S1 are worth doing regardless: they make the remote stream without touching
the server, and they delete the client's per-route method problem.

S2 is the one to schedule deliberately. Until it lands, the remote streams
*bytes* incrementally but the server still computes the whole answer first —
so first-byte latency is unchanged even though the plumbing is correct. Worth
being explicit about that, because S1 will *look* like it fixed latency and
will not have.

---

## 5. Open questions

1. **Should `S::Request` stay `heart::query::Query` for all three search
   surfaces, or split per surface?** Sharing it keeps S0 wire-compatible and is
   the recommended start. Splitting later removes `Target` entirely (it becomes
   the surface) and lets `Usages{of}` stop being an optional field on a struct
   where it is meaningless two-thirds of the time.

2. **Typed cursors.** `PageSpecification.cursor` and `Page.next` are both
   `Option<String>`, even though `heart::query::cursor` (364 lines) and
   `client::query::{SymbolCursor, SymbolCursorKey}` are typed. Add
   `Surface::Cursor` and stop stringifying? Recommend yes — it is the same
   category of hole as `Target`/`Hit`.

3. **Two query vocabularies in `heart`.** `heart::query::Query` (the wire) and
   `heart::client::query::{ExecutionQuery, AbstractQuery, Search, Filter, …}`
   (what the index server internals actually use, via `lower_to_symbol_search`).
   Under `Surface`, `S::Request` is the wire type and the lowering is an
   internal detail — but both should not remain public in `heart`.

4. **Does `Note` need a schema registry?** `S::Note` is typed on both sides, but
   a server emitting a `Note` variant an older client lacks needs a rule.
   `#[non_exhaustive]` plus "ignore unknown notes" is probably enough, since
   notes are by construction non-essential to the items.

---

## 6. Resolutions (2026-08-18)

Recorded here rather than folded into §1–§5 so the reasoning stays next to what
was originally asked. Everything below is landed and verified unless marked
otherwise.

### 6.1 Backpressure — `capacity` stops being a lie

`answer_channel(capacity, generation)` validated `capacity > 0` and then built
`flume::unbounded()`, discarding it. Every call site — `merge` at 256, the
remote client at 64, the server's source answers at 256 — declared a bound
nothing enforced.

It could not simply become `flume::bounded`. `Emitter::push` is a plain
non-async fn because `Serve::serve` is called from lindsey's foreground thread,
which has no reactor; and the canonical `Serve` impl emits **synchronously
inside `serve()`, before the `Answer` has been handed to anybody**. A blocking
send there is not a stall, it is a self-deadlock: nothing can drain a receiver
that does not yet exist anywhere else.

The policy, therefore, is two emit paths for two genuinely different producers:

* `push`/`item`/`note` stay non-blocking. On a full channel the frame is
  dropped and the call returns **`EmitError::Lagged`** — a third variant,
  distinct from `Cancelled` (consumer gone, stop working) and `Finished`
  (answer over). Memory is bounded, no thread ever parks, and a sync producer
  with no runtime keeps working.
* `push_async`/`item_async` await room. The spawned producers — the merge pump,
  the server's search tasks, the remote client's pump — use these and get real
  no-loss backpressure that propagates back to the socket.

**Honesty is structural, not a producer responsibility.** The emitter counts
what it dropped and `end()` rewrites its own summary to `Completeness::Partial
{ covered: delivered, total: Some(delivered + dropped) }`. A producer cannot
report `Complete` over a truncated answer even by explicitly asking to. A small
reserve above `capacity` is kept for terminal and `Degraded` frames only, so an
answer can always end and always explain itself — without it the memory bug
would have become a hang at the terminal frame.

Pinned by `heart/tests/backpressure.rs` (20 tests).

### 6.2 `Federated<S>` — and who drives the pump

`merge` deliberately does not spawn; it returns a `MergePump` the caller drives,
which is what keeps it runtime-agnostic. But `Serve::serve` returns only an
`Answer<S>`, and a `Serve` impl that needed special handling from its caller
would not be a transparent one.

So the executor is **injected once, at construction**: `Federated::new(sources,
spawn)`. The index server passes `tokio::spawn`; lindsey passes a GPUI
background-executor closure. After that `serve` is an ordinary call with no
runtime assumptions in its signature — the one thing that genuinely differs
between hosts is named once, in one place.

Pinned by `heart/tests/federated.rs` (12 tests).

**Known wart.** `Federated` guarantees that every source failing still yields a
terminating `Frame::End` (offline is not failure), while `merge` itself sends a
terminal `Failed` when *all* its sources fail — a rule the index server wants
and `tests/merge.rs` pins. The gap is currently closed by appending a silent,
always-complete phantom source so `merge`'s all-failed branch can never trigger.
It works, is unobservable, and is covered by
`every_source_failing_still_ends_the_answer`, but the guarantee is expressed as
an emergent consequence of a counting rule rather than stated outright. Prefer
an explicit merge policy parameter when the index crate is next open.

### 6.3 Cross-plane symbol identity — two defects

**The local plane could not produce a `SymbolHit` at all.** `Symbols::key` is
`(PackageId, path)`, defended in `surfaces.rs` on the grounds that `PackageId`
is not instance-salted the way `SymbolId` is. True — and it reasons only about
two *index* instances. `PackageId` derives from `RegistryOrigin` + name +
concrete version; the engine holds `PackageLineageId { ecosystem, name }`, which
is deliberately version-free, and `RegistryOrigin` appears nowhere in
`nudox-engine`. Task #16 removed the *salt* from the key and never checked
*availability* — the same defect class one step on.

**Remote hits are already non-navigable, in shipping code.**
`gui/src/stores/search_model.rs:607` fabricates a `SymbolKey` by copying a
UUID's 16 bytes into an `IntroId` and zeroing the rest. A real `IntroId` is
`blake3("nudox.intro.v5" ‖ lineage ‖ kind ‖ path ‖ name ‖ disambiguator)`, and
`resolve_symbol` does an exact lookup, so **clicking a remote search result
returns `SymbolNotFound`**. The function's own doc comment concedes there is no
true key to recover and synthesizes one anyway.

The fix keeps the key. A version-free key was considered and rejected:
`heart::Symbol` — all the index has at hit-build time — carries a `PackageId`
and no package *name*, so a lineage-stem key merely moves the mapping problem to
the remote side, per hit. Since some derivation is needed either way it belongs
on the side that does not work today, leaving the index and `merge`'s verified
dedup semantics untouched. So:

* `Language::lineage_tag`/`from_lineage_tag` and
  `RegistryOrigin::default_for` — two small closed mappings, exhaustive with no
  catch-all arm, letting the engine derive the *same* `PackageId` the index
  does. A catch-all would mint ids under the wrong registry and break dedup
  silently, so the totality is pinned rather than trusted.
* `SymbolHit::reference: Option<StableReference>` — navigable identity, using
  the grammar `heart::query` already froze and `Usages` already keys on. Unlike
  `SymbolId` it is content-derived, which is what §0.7 actually asked for.
  `Symbols::fuse` backfills it exactly as it backfills `signature`.

`Option` is a statement about the **source**, never the symbol. A row without a
reference is not navigable *yet*; it becomes navigable when the index serves IR,
with no schema change. That is honest — fabricating a key that always fails is
not.

Pinned by `heart/tests/cross_plane_identity.rs`.

### 6.4 lindsey did not compile — and root-workspace green never said so

`workspace/gui` declares its own `[workspace]`, so it is not a member of the
root one. Cargo applies `[patch]` only from the workspace root of the crate
being built, so the root's `libpijul = { path = "workspace/vendor/libpijul" }`
did nothing there and lindsey resolved the *unpatched* crates.io release:

```
error[E0433]: cannot find `nudox_f1` in `libpijul`
  --> workspace/ir/vcs/f1/mod.rs:41:19
```

Every green build reported during this work was root-workspace and never
touched the GUI. Fixing the patch table then exposed two real errors that had
been hiding behind it — a `Box<PackageMetadata>` drift, and `stores/search.rs`
passing `Vec<Scored<SymbolHit>>` to a function expecting `&[Scored<Symbol>]`
(a live consequence of task #16, verified against the root workspace only).

**Any change to `heart` or `nudox-engine` public types can break lindsey
invisibly.** Check it separately; mirror new vendored patches into
`workspace/gui/Cargo.toml` and run `cargo update -p <crate>` there.
