# IR for the GUI, NudoxPath, and IR storage/interaction

Branch context: `refactor/consolidation-registry-purge` mid-migration. The **live IR plane** (`ir/`, `ir-vcs/`) is IntroId-keyed. Language producers and `compiler/graph/` still speak the older **`NudoxPath` + `Index`** API, which is **no longer defined in `ir/`**. Catalog/search/read-plane code already targets the new model.

---

## 1. What the GUI actually needs from IR

The GUI never reads IR files or arenas. It talks HTTP to the driver and renders **projections**. Map GUI surfaces → wire → IR-derived fields.

### 1.1 GUI → HTTP (today vs driver)

| GUI call (`gui/src/backend.rs`) | Driver route (`driver/http/router.rs`) | Status |
|---|---|---|
| `GET /healthz` | `/healthz` | Live |
| `POST /api/packages` | **`POST /packages`** | Path drift — GUI still uses `/api/packages` |
| `GET /api/packages` | no list route | Drift |
| `POST /symbol-search` + `SymbolQuery` | **`POST /search`** + `heart::query::Query` | Drift — old shape |
| `GET /terminus_search?q=` | no Terminus handler | Dead path (graph is Trustfall-over-IR now) |
| `GET /run?q=&session=` | no `/run` | Dead — use `/search` / `/search/semantic` + `/expand` + `/sessions/:id` |
| `POST /packages/search` | `/packages/search` | Live (body should be `heart::Query`, not a private DTO) |
| `DELETE /session` | `GET /sessions/:id` only | Drift |
| — | `POST /usages`, `POST /expand`, `GET /symbols/:id` | Driver-ready; GUI not wired |
| — | `POST /v1/compiled/lookup`, depshards, rerank | Vector/compiled plane; not GUI yet |

**Constraint (GUI comment):** `body_query` is omitted intentionally (would 501). Body/call-graph is IR-side but not a first-class GUI query yet.

### 1.2 Canonical wire query (`heart::query::Query`)

One algebra for every search surface (wire = domain):

```text
Query {
  target:  Packages | Symbols | Usages { of: StableReference }   // F:<eco>/<pkg>#<hex>
  text:    string
  scope:   { ecosystems[], packages[], include_withdrawn }
  rank:    Fused | TextOnly
  mode:    Precise | Semantic
  routing: { quality, reach, hot_packages[] }   // dense-stage knobs
  session: optional Guid                         // expand merges into session graph
  at:      optional AsOf (Time preferred)
  page:    { limit, cursor? }
}
```

Routes:

| Route | Required target / mode | Returns |
|---|---|---|
| `POST /search` | `Symbols` | NDJSON `Scored<Symbol>` |
| `POST /search/semantic` | same, forces `Semantic` | same |
| `POST /packages/search` | `Packages` | `Page<GlobalPackage>` |
| `POST /usages` | `Usages { of }` | reverse-occ page (501 until reverse projection lands end-to-end) |
| `POST /expand` | `text` = seed `SymbolId` UUID | related `Symbol`s; optional session merge |
| `GET /symbols/:id` | — | one `Sourced<Symbol>` |
| `GET /sessions/:id` | — | `SessionGraph` (node ids) |

### 1.3 Fields the GUI renders, and where they come from in IR

**Search hit / library list** (`SymbolMatchResponse` / `heart::Symbol`):

| UI field | Source plane | IR origin |
|---|---|---|
| plain / fq name | Tantivy `TextIndex` + Dolt symbol projection | `reflect::monikers` → moniker string; leaf = `SymbolWire.name` |
| kind | stored kind token | `OwnedEntryPayload.kind_disc` / `KindWire` |
| package / ecosystem | package id + language token | `PackageLineageId` + catalog |
| score | ranking, not IR | — |
| snippet / docs | (old `/run` hit) docs | `SymbolWire.documentation` (not on lean `heart::Symbol` today) |

Tantivy schema (`index/runtime/text/index.rs`) indexes only:

`id`, `package`, `ecosystem`, `kind`, `name`, `fq_name`, + lower/token shadows.

That is the **serving projection**, not the full IR. Rebuild a `Symbol` from a hit; docs/signature/shape require archive/graph/detail fetch.

**Symbol detail tab** (`SymbolEntry` — Terminus-shaped JSON-LD legacy):

| UI field | IR / graph origin |
|---|---|
| `name`, `fq_name`, `path[]` | moniker segments / parent chain |
| `visibility` | `SymbolWire.visibility` |
| `documentation` | `SymbolWire.documentation` |
| `aliases` | `SymbolWire.aliases` |
| `kind` (params, fields, return, generics…) | `KindWire` tree (function/record/enum/…) |
| `members[]` | parent→children enclosure (`IrView::children_of` / graph `member_of`) |

**Signature / kind section** (what the tab paints):

- Function/method: params name+type, return type → `KindWire::Function` (`IN`/`OUT` F1 keys)
- Struct: fields, generics → `KindWire` record/type + field types
- Else: kind type label only

**Graph view** (`ExplorationGraph`):

- Nodes: same entry payload as detail (id, names, kind)
- Edges: `Member` | `Semantic` | other
- Driver expand walks **graph relationships** (`SymbolStore::related_hits`); session store keeps node id sets
- IR graph edges (Trustfall neighbour methods): `Member` (children), `OccTarget`, `TypeRef` (impl-of / trait supers), `Lineage` (parent), reverse `usages`

### 1.4 IR queries the GUI *needs* (logical, not WOQL)

If the GUI is re-pointed at the live driver, these are the IR-backed operations:

1. **List/search symbols in package** — moniker + kind projection (`monikers` + text index); filter by ecosystem/package/kind; paginate.
2. **Resolve one symbol** — by `SymbolId` / intro; load sealed payload for name, vis, docs, kind body.
3. **Children / members** — enclosure tree for outline and breadcrumb.
4. **Parent / lineage** — breadcrumb up the chain.
5. **Semantic neighbours** — dense search + optional expand (implements/extends/mentions/takes/returns when graph is hydrated).
6. **Usages** — reverse occurrences of a `StableRef` (once reverse index is served).
7. **Session graph** — accumulate expand nodes; not IR storage, Postgres session.
8. **Package search** — catalog/facets; not IR entry plane.

Pure IR reflections used *to build* those projections (not called from GUI):

```text
exported(ir, policy, cfg, intro) -> bool
monikers(ir, policy, cfg)        -> (MonikerPath, IntroId)*
boundary(ir, policy, cfg)        -> (MonikerPath, StableRef)*
IrView::{entry, parent_of, children_of, body, occurrences_of, links}
IrTrustfallAdapter::{members, lineage, type_refs, usages, occ_target}
```

There is **no stored ApiSurface artifact** — public surface is pure function of IR + `ExportPolicy` + `CfgAssignment`.

---

## 2. NudoxPath — where and how

### 2.1 Current status

- **`ir/` has no `NudoxPath`**. Entries are keyed by **`IntroId`** (content-addressed, durable) and addressed in-process by **`ArenaIdx` / `EntryIdx`**.
- Human path is **`MonikerPath`** (`ir/reflect.rs`): root-first name segments from the parent chain; dotted display; used for search/boundary.
- Working-tree file path is **`symbol_path(intro) = "{intro_hex}.nir"`** (`ir/serialize.rs`) — *not* a language FQN.
- Cross-package identity is **`StableRef { package: PackageLineageId, intro: IntroId }`**, wire form often `F:<eco>/<pkg>#<hex>`.

### 2.2 Historical / residual producer meaning

Producers and `compiler/graph/from_ir.rs` still import `ir::entry::{Index, NudoxPath}` (broken against current `ir`). Intended shape (from rust/ts/python/java/csharp/nix lowerers):

```text
NudoxPath::Local(PathBuf)                      // in-package FQN, language-joined (:: or similar)
NudoxPath::External { dependency, path }       // other package; path relative after crate segment drop
```

**Rust producer** (`compiler/compile/rust/ra/ctx.rs`):

- Builds `::`-joined segments (crate rustc name + module chain + name).
- Caches `PathKey → NudoxPath`.
- `Local` only if def’s crate == crate being lowered; **path deps and crates.io deps are `External`**.
- External paths **drop the leading crate segment** (rustdoc-shaped: `helper::Marker` → `External { dependency: "helper", path: "Marker" }`).
- Placed on every `Symbol { path, … }` shell; used for members maps, doc-link targets, alias keys, occurrence anchors.

**Other languages** (same idea, different joining):

| Lang | Local path construction |
|---|---|
| TypeScript | `local_path.join("::")` after link pass assigns real paths |
| Python | module qualname / file path → `NudoxPath::Local` |
| C# | `namespace::Type` / `namespace::Type.member` |
| Java / Go / Nix | namespace or attr path as Local string |

**Resolve** (`compiler/generate/resolve.rs`): import bindings pin unresolved names to `NudoxPath::External`; local defs stay Local. Occurrences keyed by path + span + confidence.

**Graph emit** (`compiler/graph/`): old Terminus model used path in IRI `Symbol/{lang}/{pkg}/{fq}`; linker resolved name strings to those IRIs; members inverted from path membership.

**Blob references** (`index/blob`): legacy NudoxPath wire **staged out** — `RefTarget::{Local,External}` is a plain string mirror of the old split, not a live IR type.

### 2.3 Replacement mapping

| Old (`NudoxPath`) | New |
|---|---|
| Index key / entry identity | `IntroId` (+ parent map in `PristineIntroTable`) |
| FQN display / search string | `MonikerPath` from ancestry + names |
| External dep path | `StableRef` to foreign intro (or unresolved name only transiently in producer) |
| File on disk for symbol | `{intro_hex}.nir` (F1 text) |
| Graph node id | `SymbolId` (uuid projection of intro/slot) / archive intro |

---

## 3. IR model, storage, and interaction (interesting bits)

### 3.1 Data model (`ir/`)

```text
Entry (arena)     = Symbol + Node{parent,children} + EntryInner{Owned(Kind)|Reference}
OwnedEntryPayload = SymbolWire + kind_disc + KindWire + flags + payload_hash
IrView            = PristineIntroTable + bodies + occurrences   // query face at a channel tip
BodyEmbed         = Absent | Present(TreesitterBody ⋃ OracleBody)  // separate channel
Occurrence        = owner IntroId → target StableRef + kind + confidence + rel_span
```

- Strings interned per-arena (`StrId`); wire uses owned `String`.
- Identity: `bootstrap_intro_id` / v2 + `sig_key` domains (`nudox.intro.v1/v2`, `nudox.sigkey.v1`).
- Payload hash domain: `nudox.entry.v2`.
- Type fingerprint: first 4 bytes of `blake3("nudox.tyskel.v1" ‖ skeleton)`.
- **libpijul-free data model**; changes live in `ir-vcs` + vendored libpijul.

### 3.2 Working copy & VCS (`ir-vcs/`)

| Layer | Role |
|---|---|
| **NdIrF1** (`f1.rs`) | Line-oriented text per intro: `NdIrF1\t1` + frozen key registry (name, vis, doc, span, parent, kind body keys…). Strict order/sets. Field-aware Myers via `libpijul::nudox_f1`. |
| **Working tree** | Flat `{intro_hex}.nir` only (no dirs — libpijul unrecord/dir hazard). |
| **Body channel** | `nudox.body.v1\0 ‖ postcard(BodyEmbed)`; path convention separate from `.nir`. |
| **PackageArchive (`NdIr`)** | Sealed mmap serve snapshot: header + TOC sections for intros, strings, links, type skeletons. Deterministic `CasKey`. **No** source, CST, Terminus docs, embeddings. |
| **Sync** | ALPN `nudox/ir-sync/1` (iroh); change ids/hex shared with index compiled plane. |
| **Diff / semver** | Structural delta over generations (`nudox.delta.v1`); surface hashes `nudox.apisurface.v1`, embed hash `nudox.embed.v1`. |

### 3.3 Producer pipeline → IR

```text
language oracle / static parse
    → entries (paths historically NudoxPath-keyed)
    → resolve ladder (occurrences, External anchors)
    → stream NdIrF1 / postcard batches over ir-stream socket
    → host materialize PristineIntroTable + body merge + occ frames
    → seal PackageArchive; project monikers → catalog + text index + vectors
```

Sandbox: guest socket **`/run/nudox/ir-stream.sock`**. Producers under `/opt/nudox/{lang}/bin/nudox-*-producer`.

Body merge is normative: both treesitter + oracle when both run; overlapping spans keep structure from TS and `StableRef`/confidence from oracle; host still writes occurrence frames from call-site union.

### 3.4 Read-plane projections (derived, disposable)

| Store | What it holds | Built from |
|---|---|---|
| Dolt global index | package lifecycle, symbol moniker+kind slots | monikers / intro blobs |
| Tantivy `TextIndex` | name/fq/kind/eco for precise search | `heart::Symbol` projection |
| Qdrant / edgepacks | dense embeddings | embed recipe over name+docs+sig (`nudox.embedtext.v2`) |
| Trustfall adapter | graph walk over `IrView` | in-memory; reverse index optional |
| ReversePositionIndex | target→owners, ty_ref→entries | rebuilt per channel tip; never synced |
| Session store | exploration node sets | expand results |
| Object packs / CAS | source blobs, manifests | separate from IR archive |

Coordination: outbox fans write intents to derived stores. Text poller upserts by `SymbolId` (delete-then-add).

### 3.5 Graph duality (migration residue)

| Old | New |
|---|---|
| TerminusDB documents (`compiler/graph/model.rs`) | Trustfall over `IrView` (`registry/graph/`) |
| WOQL / GraphQL links (`member_of`, `implements`, …) | Neighbour methods; full Trustfall schema still stub (`execute_graph_query` → Unsupported) |
| GUI `terminus_search` / `/run` | `/symbols/:id`, `/search`, `/expand`, `/sessions/:id` |

`from_ir` still describes rich edge emission (mentions/takes/returns, Implementation reification, call-graph Reference nodes) against the **old** Index — target shape for a future graph hydrate, not the live Trustfall wave-1 surface.

### 3.6 Identities & domains (quick index)

| Domain / form | Use |
|---|---|
| `IntroId` / `{hex}.nir` | Per-declaration durable id / file |
| `StableRef` / `F:eco/pkg#hex` | Cross-package ref, usages target |
| `PackageLineageId` | ecosystem + name lineage |
| `SymbolId` (uuid) | Search/session serving id |
| `payload_hash` / `GenerationStamp` | Content / generation identity |
| `nudox.body.v1`, `nudox.entry.v2`, `nudox.delta.v1`, … | Format domains (never renumber casually) |

### 3.7 What is *not* IR

- Source tarballs / file CAS (`index` packs) — companion plane.
- GUI `LocalContext` enrichment — **client-only**, never on INDEX wire.
- Session graphs — Postgres, not pijul.
- Embeddings — derived vectors keyed by recipe, not declaration payload.

---

## 4. Practical checklist for GUI ↔ IR alignment

1. Speak `heart::Query` to `/search`, `/packages/search`, `/usages`; drop `SymbolQuery` / `/symbol-search`.
2. Detail fetch: `GET /symbols/:id` (lean) + later detail endpoint or archive open for full `KindWire`/docs if lean Symbol is not enough.
3. Graph: seed session → `/expand` + `GET /sessions/:id`; stop depending on Terminus JSON-LD.
4. Treat **moniker / IntroId / StableRef** as the path story; do not reintroduce NudoxPath on the wire.
5. Body/call-graph UI = reverse usages + body embed when served; do not invent a free-form `body_query` without an engine.
6. Producers still must be rebased from `NudoxPath` Index maps onto `EntryBuilder` → sealed payloads → intro parent map before compile/graph crates compile against current `ir`.

---

## 5. One-line architecture

**Producers lower code into sealed intro payloads (F1/body channels) versioned by libpijul; the host materializes `IrView`; disposable projections (text, vector, reverse index, monikers) answer GUI queries; NudoxPath was the old in-package path key and survives only in unfinished producer/graph code.**
