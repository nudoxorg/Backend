# INDEX-PLAN.md — Greenfield Catalog, Dual Deployment, IR Universe

**Status:** Rev 3 — **normative master** · 2026-07-19  
**Posture:** greenfield · **no fallbacks** · no dual stacks · no “phase-2 maybe”  
**Audience:** implementers (including juniors). Prefer following this file over older research when they disagree.

**Companions (owned elsewhere, cited not duplicated):**
- IR types / F1 / channels: `.research/ir-vcs/design/SEMANTIC-IR-VCS-PLAN.md` (SIV) + as-built `nudox-ir-vcs`
- Resolution = IR frames: `.research/resolution/04-ir-unification.md` (**U-*** decisions — folded here as **UR-***)
- Ecosystem specs / followers: `ECOSYSTEM-PLAN.md`
- Smolvm cage / streaming: `SMOLVM-PLAN.md` (amended by this doc for promote + goldens)
- GUI UX: `GUI-PLAN.md` (stores-over-client; **no** Ladybug; no client graph engine)

**Explicitly dead (do not implement):**
Postgres · Litestream · Doltgres · Ladybug · Terminus · ApiSurface structure · OccurrenceSeal /
references-layer CAS · dual vector stacks · public IPFS · naked doltlite-remotesrv · client-side
graph engines · registry tarball as primary source (`.crate` / nuget nupkg / etc. as SoT)

**Supersessions (full, not “refines”):**
| Old | New |
|---|---|
| GD-2 sqlx + Litestream INDEX | **ID-1** rusqdoltlite `catalog.dolt` + §12 ops/DR |
| GD-32 no versioned SQL | **Overturned.** Metadata history **is** DoltLite commit graph. IR history remains libpijul. |
| GD-30 catalog-only-HTTP + S3 CAS IR SoT | HTTP for **query/control**; iroh+Bao for **all bulk transfer**; IR = changestore |
| GD-34 Ladybug | Dead. Graph = Trustfall over IR memory + disposable reverse-index projection |
| GD-13 Terminus hot graph | Dead as product. No Terminus in serving path |
| SV-6 local never upload | **ID-26:** local → **trusted remotes only**; those objects become mainline cache when promoted |
| LIBRARIFICATION dual CAS layouts | Object plane = **ObjectPack** (§6) + IR changestore (§5); no parallel IR `cas/` history |

---

## 0. What “ops” means (and what was missing)

Earlier audits said “no replacement **ops**” after killing Litestream. That did **not** mean
CatalogOps. It meant **operational runbook numbers**: how you back up, restore, measure RPO/RTO,
run multi-host HA. Those numbers live in **§12**. They are first-class, not an afterthought.

---

## 1. Purpose (one paragraph)

Build a greenfield system with two sovereign planes:

1. **INDEX catalog** (`catalog.dolt` via rusqdoltlite) — packages, versions, generations,
   locations, edges, repo facts, advisories, git-monitor watermarks, outbox, overlays.
2. **IR plane** (libpijul `IrRepository`) — symbol history, `occ` frames, materialize views.
   Tree-sitter and oracles are **producers** into the same IR. No references layer. No ApiSurface
   object. Graph queries are Trustfall over IR (+ a disposable reverse index for speed).

**Object plane** (source trees, ObjectPack, goldens, compile stages) is content-addressed and
moved only over **iroh + Bao**. Simple HTTP stays for search/auth/control JSON.

Deploy as **two configurations of the same crates** (§3):
- **Remote / coordinated** — multi-compiler, multi-host, HA, enterprise remotes.
- **Embedded / GUI** — one process, one compiler, local catalog clone, offline branches.

Migrations are sea-query + snapshot branches (catalog) and format_version envelopes (IR/objects)
— always rebuildable projections, never dual-write ladders.

---

## 2. Ownership map

```
 SOVEREIGN                         DISPOSABLE (rebuild anytime)
┌────────────────────────────┐    ┌──────────────────────────────────┐
│ catalog.dolt (DoltLite)    │───▶│ tantivy package + symbol          │
│  metadata + locations +    │───▶│ reverse-position usage index     │
│  outbox + overlays         │───▶│ vector plane (one stack)         │
├────────────────────────────┤    │ moniker/export reflection caches │
│ IrRepository (libpijul)    │───▶│ Trustfall adapter (in-memory)    │
│  channel + changestore     │    └──────────────────────────────────┘
│  entries + occ frames      │
├────────────────────────────┤    EPHEMERAL
│ ObjectPack store           │    scratch.sqlite: jobs, wanted,
│  source trees (not .crate) │    sessions (writer-sticky), claims
│  goldens, L1 stages        │
└────────────────────────────┘
```

**Division of history**
- Catalog commit graph → “what did metadata believe at T?” (dependents, listing, advisories).
- IR channel → “what did symbols / occs look like between gens?”
- ObjectPack → content-addressed blobs; no product history of its own beyond hash identity.

---

## 3. Two deployment shapes (librarification intent)

Same crates, different feature sets and process topology.

| Concern | `Deployment::Remote` | `Deployment::Embedded` |
|---|---|---|
| Process | `server` + `ingestor` + N forge workers | GUI process embeds `index`+`registry`+one forge |
| Compilers | Coordinated pool (Kueue/fleet optional); many SmolvmCage | **Exactly one** SmolvmCage / one toolchain plane |
| Catalog | Writer host + iroh pull replicas | Local `catalog.dolt` clone; branch `local/<device>` offline |
| IR | Shared enterprise IrRepository stores | Local IrRepository under app data dir |
| ObjectPack | Regional iroh providers (durable substrate OK) | Local ObjectPack + trusted-remote provide |
| Talks to | Many JobKeys, HA | One compiler; may pull from trusted remote |

```rust
// heart/deployment.rs — freeze early
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeploymentKind { Remote, Embedded }

pub struct DeploymentProfile {
    pub kind: DeploymentKind,
    /// Remote: N; Embedded: always 1.
    pub max_concurrent_cages: NonZeroUsize,
    pub trusted_remotes: Vec<TrustedRemote>,
    pub catalog_path: PathBuf,
    pub ir_repo_root: PathBuf,
    pub object_pack_root: PathBuf,
}
```

---

## 4. Ground decisions (complete list)

**ID-1 — Catalog engine = rusqdoltlite (DoltLite) only.**  
No plain-SQLite product mode. Vendor+pin. `dolt_commit` / merge / at / gc / rebase available.  
Litestream and sqlx are gone. Single writer thread on advertised `main`.

**ID-2 — `catalog.dolt` + `scratch.sqlite`.**  
Scratch: jobs, wanted, sessions, claims — never on remotes.

**ID-3 — Outbox in catalog is the only projection fan-out.**  
Same transaction as business write. Typed ops (§8).

**ID-4 — Batch heartbeat commits** after ingest batches, listing, forge seal register, git-monitor
updates, bakery, timer.

**ID-5 — Schema v4** (§8). Fully typed via `catalog_table!`. Painful migrations = sea-query +  
`pre-migrate-vN` snapshot branch + rollback = checkout.

**ID-6 — One Query algebra** in heart (§9). Wire = domain.

**ID-7 — Ingestor external on Remote; optional in-process on Embedded for tests only.**  
Serving binary never links feed parsers in Remote builds.

**ID-8 — Ranking within-eco only.** Fixed fusion (ID-11 of rev2 kept):  
`0.45*dependents_pct + 0.25*downloads_pct + 0.20*text_semantic + 0.10*quality`.  
Scoped deps hard-boost. Eval harness guards regressions only.

**ID-9 — Overlays = branches/remotes.** Merge policy: upstream-authoritative / device-owned /
union (`generation_locations`, `listing_events`, `stores`, `object_locations`).

**ID-10 — Offline = branch `local/<device-id>`**, not a mode.

**ID-11 — HTTP control + query; iroh+Bao for all bulk data.**  
HTTP: search, authz, wanted long-poll, health, grants, thin JSON.  
iroh: catalog prolly chunks, IR change files, ObjectPack members, goldens, L1 stages.

**ID-12 — IR is the universe (fold 04-ir-unification).**  
- No `ApiSurface` structure — pure reflections: `exported` / `monikers` / `boundary`.  
- Tree-sitter + oracle = producers on one ir-stream; treesitter is **embedded inside every
  producer path** (not a parallel pipeline) — see §5.1.  
- Occurrences = `occ` IR frames on enclosing entry (relative spans).  
- **Examples / usages = queries over IR** (`Target::Usages`, Trustfall) — never a corpus table.  
- Function/method **bodies**: treesitter (or oracle body facts) are **directly embedded on the
  owning entry** as body payload — they need **not** share the same shape as surface entry IR
  (kinds/params/types). Body is an extension slot of the entry, versioned with it, not a sibling
  store.  
- No OccurrenceSeal, no references CAS, no `occurrences_ref`.  
- Monikers: **committed**, fully typed (`MonikerPath`, `IntroId`, `StableRef`).  
- Graph: **Trustfall over IR memory** + disposable reverse-index for reverse queries.  
  No Ladybug. No Terminus.

**ID-13 — Source provenance + ObjectPack reconstruction (all ecosystems).**  
For each **published version** (e.g. `axum@0.7.9`) the catalog **always** records:

1. **Registry identity** — ecosystem + name + version as published (crates.io / npm / …).  
2. **Where we took source from** (`source_kind` + provenance columns):  
   - Prefer **git** (grit): `repo_url` + `source_rev` (tag/commit that maps to that release).  
   - Else **reconstruct** from the registry package artifact (`.crate`, nupkg, maven sources jar,
     npm tarball, …): unpack once into a **deterministic tree**, seal as **ObjectPack** (not
     stored *as* `.crate`). `source_kind = reconstructed_registry_package`. Preserve
     **registry checksum** so quality of information matches the published artifact.  
3. **`source_pack`** = ObjectPackId of that tree (git checkout or reconstructed).  

We never open live registry CDN packages at query time. Snippets = ObjectPack range-get + IR
spans. Same pattern for every language’s “published package blob.”

**ID-14 — Indexer dual mandate.**  
1. **Registry metadata plane:** versions, deps, license, downloads, yank, **advisories /
   deprecation / security** (listing_events + advisory tables).  
2. **Git plane (primary signal for source):** monitor upstream repos with **grit**
   (`grit-lib`), update index entries (tips, tags, default branch), enqueue IR work when
   source changes. Watermarks in catalog.

**ID-15 — IR generation state always registered.**  
Even with **no IR store linked yet**, catalog rows track parse/IR pipeline state
(`parse_state`, `gen_stamp?`, `channel_tip?`, `ir_status`). Locations may be empty/`pending`.

**ID-16 — ObjectPack unifies packs.**  
One container family replaces separate ad-hoc `cas/` dumps + ndpk/ndix product split:
zstd frames + TOC with **byte-range addressable members** (Bao outboard optional for large
objects; TOC enables single-snippet range get). See §6.

**ID-17 — SmolvmCage is first-class and reproducible.**  
Built from **nix** toolchain images; content-digest identity. Goldens are ObjectPack members
transferred over iroh. Same image digest + producer_version ⇒ shareable golden.

**ID-18 — Local uploads only to trusted remotes.**  
Enrolled devices may `provide` ObjectPack/IR changes to remotes on their trust list. When
work lands on mainline, those objects **are** the cache (content-addressed) — no second upload
path. Anonymous clients are read-only on shared helpers.

**ID-19 — Sessions writer-sticky in scratch.** Exploration graphs only.

**ID-20 — Engine facade is modularity, not a second product.**  
`index::engine` wraps all `dolt_*`. Fail IP-0 ⇒ stop; do not ship non-versioned product.

**ID-21 — Three workspaces** (backend / ir / gui). Crate **names are free**; topology rules
matter (backend never deps libpijul; IR may dep heart).

**ID-22 — Painless persistence evolution.**  
Catalog: sea-query migrations + `pre-migrate-vN` branches.  
IR: format_version on envelopes; never mutate old change bytes.  
ObjectPack: version byte in header; readers support N,N-1.  
Projections: wipe on schema_version bump.

**ID-23 — One vector plane** (vector-core + registry features). Delete legacy runtime/vector.

---

## 5. IR plane (implementer contract)

### 5.1 Producers (oracle + treesitter embedded)

Every producer path **runs treesitter and oracle contributions into one body**, not “oracle
or treesitter.” The host always gets a single ir-stream. Treesitter is embedded *inside* every
producer (or as a mandatory fill pass on the same session):

- **Surface definitions** — oracle when available; treesitter dumb tier otherwise (same entry wire).  
- **Bodies** — **union of both**: take **as much as possible** from treesitter *and* from the
  oracle. Never drop one side because the other “won.” Merge is additive with confidence
  precedence on *conflicts*, not replacement of whole payloads.

Body structure is **embedded on the entry** (slot below). Shape may differ from surface
`EntryInner` (kinds/params/types) — still IR, versioned with the entry, queryable. Finding
examples / call sites remains **IR query**, not a treesitter sidecar DB.

```rust
// ir-stream / producer boundary
pub trait IrProducer: Send {
    fn language(&self) -> Language;
    /// Stream wire entries + unresolved ref facts + body contributions into the host session.
    fn lower(
        &self,
        inputs: &SealedInputs,
        sink: &mut dyn IrStreamSink,
    ) -> Result<ProducerReport, ProducerError>;
}

/// On the entry — not a parallel IR universe.
/// Spans are relative to the owning entry's span start (O(delta) / U-3).
#[derive(Clone, Debug)]
pub enum BodyEmbed {
    /// No implementation body (type alias, forward decl, pure interface, empty module, …).
    Absent,
    /// Normative non-absent case: **merged** treesitter + oracle facts.
    /// Always populate every channel each tier can fill; do not ship Treesitter-only or
    /// Oracle-only as the steady-state product shape when both ran.
    Present(BodyFacts),
}

#[derive(Clone, Debug, Default)]
pub struct BodyFacts {
    pub lang: Language,
    /// CST-derived structure (always filled when treesitter ran on this body).
    pub tree: TreesitterBody,
    /// Semantic outline from the language oracle (filled when oracle analyzed this body).
    pub oracle: OracleBody,
    /// Cross-tier merge metadata (what each side contributed; for honesty / debug).
    pub merge: BodyMergeNote,
}

#[derive(Clone, Debug, Default)]
pub struct TreesitterBody {
    pub locals: Vec<LocalBind>,
    pub calls: Vec<BodyCall>,           // name + receiver + rel_span (pre- or post-resolve)
    pub control: Vec<ControlSketch>,  // if/match/loop sketches — coarse is fine
    pub imports_in_body: Vec<BodyImport>, // rare; usually module-level
    pub root_kind: Option<SmolStr>,   // CST root for debug, not identity
}

#[derive(Clone, Debug, Default)]
pub struct OracleBody {
    /// Prefer StableRef when oracle resolved; else name-level for host ladder.
    pub calls: Vec<OracleCall>,
    pub type_mentions: Vec<OracleTypeMention>,
    pub reads_writes: Vec<OracleAccess>, // optional semantic dataflow crumbs
}

#[derive(Clone, Debug, Default)]
pub struct BodyMergeNote {
    pub treesitter_ran: bool,
    pub oracle_ran: bool,
    /// On overlapping call spans: Oracle confidence wins target; treesitter keeps structure.
    pub conflict_policy: &'static str, // "oracle_target_treesitter_span"
}

#[derive(Clone, Debug)]
pub struct BodyCall {
    pub name: SmolStr,
    pub receiver: Option<SmolStr>,
    pub rel_span: RelSpan, // (u32, u32) relative to entry span start
}

#[derive(Clone, Debug)]
pub struct OracleCall {
    pub target: Option<StableRef>, // None ⇒ host resolve still pending
    pub kind: ReferenceKind,       // call | mcall | …
    pub conf: Confidence,          // typically Oracle
    pub rel_span: RelSpan,
}

/// Host-side merge after both tiers emit for the same entry (normative).
pub fn merge_body(tree: TreesitterBody, oracle: OracleBody, note: BodyMergeNote) -> BodyEmbed {
    if tree.is_empty() && oracle.is_empty() {
        return BodyEmbed::Absent;
    }
    BodyEmbed::Present(BodyFacts { lang: tree.lang_or(oracle.lang), tree, oracle, merge: note })
}
```

**Merge rules (implementers — do not invent softer ones):**

1. If the entry has no body → `Absent`.  
2. If either tier produced any fact → `Present` with **both** `tree` and `oracle` structs
   filled to the max that tier produced (empty vecs only if that tier truly saw nothing).  
3. Overlapping call/type spans: keep **treesitter** structural fields; set **oracle**
   `StableRef` / higher `Confidence` on the matching span; never delete the other tier’s
   non-overlapping facts.  
4. Host `resolve_occurrences` still writes `occ` frames from the union of call sites; body
   embed remains for structure / snippet context.  
5. Forbidden steady-state: shipping only `TreesitterBody` or only `OracleBody` as the enum
   when both producers ran for that package.

**Examples are not a store.** “Show me uses of `Router::new`” =  
`Query { target: Usages { of: stable_ref }, … }` / Trustfall over `occ` frames; snippet windows
via ObjectPack range + entry/occ rel-span; body embed enriches “what’s inside this example
function.” Staleness = U-9. No `UsageCorpus` table.

Oracle duty **(d):** emit resolved references at `Confidence::Oracle`.  
Treesitter: always run on impl bodies (and surface when no oracle).

#### 5.1.1 Worked example — merged `BodyEmbed` (normative)

Source (Rust):

```rust
/// Route a request.
pub fn route(req: Request) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api") {
        handle_api(req)
    } else {
        handle_static(req)
    }
}
```

**Surface entry** (unchanged shape — not BodyEmbed):

```text
kind: Function · moniker: route · vis: pub
params: [(req, Request)] · return: Response
doc: "Route a request."
span: src/lib.rs 10:0–18:1
```

**BodyEmbed::Present** after both tiers (what we store on the entry):

```rust
BodyEmbed::Present(BodyFacts {
    lang: Language::Rust,
    tree: TreesitterBody {
        locals: vec![
            LocalBind { name: "path".into(), kind: LocalKind::Let, rel_span: r(2, 8, 2, 12) },
        ],
        calls: vec![
            BodyCall { name: "uri".into(),         receiver: Some("req".into()),  rel_span: r(2, 15, 2, 18) },
            BodyCall { name: "path".into(),        receiver: Some("uri".into()),  rel_span: r(2, 20, 2, 24) },
            BodyCall { name: "starts_with".into(), receiver: Some("path".into()), rel_span: r(3, 13, 3, 24) },
            BodyCall { name: "handle_api".into(),  receiver: None,                rel_span: r(4, 8, 4, 18) },
            BodyCall { name: "handle_static".into(), receiver: None,              rel_span: r(6, 8, 6, 21) },
        ],
        control: vec![
            ControlSketch::If {
                cond: r(3, 7, 3, 35),
                then_arm: r(4, 8, 4, 24),
                else_arm: Some(r(6, 8, 6, 27)),
            },
        ],
        imports_in_body: vec![],
        root_kind: Some("function_item".into()),
    },
    oracle: OracleBody {
        calls: vec![
            // same spans as treesitter where possible — StableRef filled
            OracleCall {
                target: Some(StableRef::parse("F:rust/http#…uri…").unwrap()),
                kind: ReferenceKind::MethodCall,
                conf: Confidence::Oracle,
                rel_span: r(2, 15, 2, 24), // may cover uri().path() chain as oracle sees it
            },
            OracleCall {
                target: Some(StableRef::same(intro_handle_api)),
                kind: ReferenceKind::Call,
                conf: Confidence::Oracle,
                rel_span: r(4, 8, 4, 18),
            },
            OracleCall {
                target: Some(StableRef::same(intro_handle_static)),
                kind: ReferenceKind::Call,
                conf: Confidence::Oracle,
                rel_span: r(6, 8, 6, 21),
            },
        ],
        type_mentions: vec![
            OracleTypeMention { ty: StableRef::parse("F:rust/http#…Request…").unwrap(), rel_span: r(0, 14, 0, 21) },
            OracleTypeMention { ty: StableRef::parse("F:rust/http#…Response…").unwrap(), rel_span: r(0, 25, 0, 33) },
        ],
        reads_writes: vec![],
    },
    merge: BodyMergeNote {
        treesitter_ran: true,
        oracle_ran: true,
        conflict_policy: "oracle_target_treesitter_span",
    },
})
```

What each side uniquely kept (union, not pick-one):

| Fact | Treesitter | Oracle |
|---|---|---|
| local `path` | yes | often no |
| `if` structure | yes | often no |
| `handle_api` call span | yes | yes + **StableRef** |
| `Request`/`Response` type mentions | weak/name | yes + **StableRef** |
| CST `root_kind` | yes | no |

**`occ` frames** (still on this entry, after host resolve) are separate from BodyEmbed but
fed by the **union** of call sites:

```text
occ  F:rust/…#handle_api     call  orc  <rel>
occ  F:rust/…#handle_static  call  orc  <rel>
```

Usages query for `handle_api` does not read BodyEmbed; it reads reverse `occ`.  
UI “open this example’s impl outline” may read `BodyFacts.tree.control` + `oracle.calls`.

### 5.2 Host session (already largely as-built)

```rust
// nudox-ir-vcs — normative use
impl<C: ChangeStore> IrRepository<C> {
    pub fn begin_recording(&self, job: JobKey) -> Result<RecordingSession<'_, C>, VcsError>;
}

impl RecordingSession<'_, _> {
    pub fn stage(&mut self, batch: StagedEntryBatch) -> Result<StageReport, VcsError>;
    /// Host resolve ladder (U-5): binds unresolved refs → occ frames on owners.
    pub fn resolve_occurrences(&mut self, deps: &dyn DepIrProvider) -> Result<ResolutionStats, VcsError>;
    pub fn checkpoint(&mut self, msg: &str) -> Result<Option<ChangeHashHex>, VcsError>;
    pub fn finish(self) -> Result<FinishReport, VcsError>; // one change; tip for gen pin
    pub fn abandon(self) -> Result<(), VcsError>;
}
```

**Deleted APIs (do not reintroduce):** `ApiSurface`, `DepSurfaceProvider`, `OccurrenceSeal`,
`stage_occurrences` as a separate plane, `SymbolTable` / `MergedSymbolTable`,
`Extraction`/`RawDefinition`/`RawReference` as public types.

### 5.3 Reflections (pure)

```rust
// nudox-ir or registry/reflect — pure, no I/O
pub fn exported(ir: &IrView, policy: &ExportPolicy, cfg: &CfgAssignment, id: IntroId) -> bool;

pub fn monikers<'a>(
    ir: &'a IrView,
    policy: &ExportPolicy,
    cfg: &CfgAssignment,
) -> impl Iterator<Item = (MonikerPath, IntroId)> + 'a;

pub fn boundary<'a>(
    ir: &'a IrView,
    policy: &ExportPolicy,
    cfg: &CfgAssignment,
) -> impl Iterator<Item = (MonikerPath, StableRef)> + 'a;

pub trait DepIrProvider: Send + Sync {
    fn ir(&self, pkg: &PackageLineageId, pin: &DepPin) -> Result<Arc<IrView>, DepMissing>;
}
```

Caches of moniker maps: disposable, keyed by `(channel_tip, policy_id, cfg_id)`, never synced.

### 5.4 `occ` frame (grammar frozen in 04-ir-unification §3)

Relative spans on the **owning entry**. σ rewrites targets. Unbound refs → `ResolutionStats`
in `GenerationMeta` only.

### 5.5 Graph = Trustfall + reverse index

```rust
// registry/graph/trustfall_adapter.rs
pub struct IrTrustfallAdapter<'a> {
    pub ir: &'a IrView,
    /// Optional speed layer: reverse occ/typeref postings for this tip.
    pub reverse: Option<&'a ReversePositionIndex>,
}

// Adapter exposes vertices: Entry, Occ, Package, …
// Edges: Member, OccTarget, TypeRef, Lineage, DependsOn (from catalog edges join)

pub fn execute_graph_query(
    adapter: &IrTrustfallAdapter<'_>,
    query: &str, // Trustfall query string or typed builder
    args: BTreeMap<Arc<str>, FieldValue>,
) -> Result<Vec<BTreeMap<Arc<str>, FieldValue>>, GraphQueryError>;
```

**HTTP routes** (still JSON — thin results, not bulk IR):

| Route | Meaning |
|---|---|
| `Query { target: Usages { of }, … }` | reverse occ |
| `GET /v1/symbols/:ref/implementors` | reverse iof/super |
| `GET /v1/symbols/:ref/mentions` | reverse typeref |
| `GET /v1/symbols/:ref/lineage` | continuity |
| `POST /v1/expand` | neighborhood (existing) |

Bulk IR transfer never goes through these routes — clients already holding IR use Trustfall
locally (Embedded) or fetch change files via iroh then query.

**Replacement points (delete):**
- `registry/runtime/graph/*` if Terminus-coupled  
- Any Ladybug feature flags  
- `terminus-client` from server path  

---

## 6. ObjectPack (source + goldens + stages)

### 6.1 Why

Need range-addressable, highly compressed storage that can serve “one snippet” without
loading a whole tree — and fold former `.ndpk`/`.ndix` roles into **one** format.

### 6.2 Format (normative sketch)

```
ObjectPack v1
  magic: b"NDPK"
  version: u16 = 1
  flags: u16
  toc_offset: u64
  toc_len: u64
  -- members (zstd frames, optionally Bao-capable) --
  -- TOC: sorted (path or role key) → (offset, uncompressed_len, compressed_len, blake3) --
```

```rust
// heart/object_pack.rs
pub struct ObjectPackId(ContentBlake3); // root hash of TOC+policy

pub enum MemberKey {
    Source { path: RelativePath },
    Golden { image_digest: ImageDigest, producer_version: u32 },
    Stage { job_key: JobKey, stage: StageName },
    Meta { name: SmolStr },
}

pub trait ObjectPackStore: Send + Sync {
    fn put_pack(&self, builder: ObjectPackBuilder) -> Result<ObjectPackId, PackError>;
    fn has(&self, id: &ObjectPackId) -> bool;
    /// Range get: single member or byte range within member (for snippet windows).
    fn get_member_range(
        &self,
        id: &ObjectPackId,
        key: &MemberKey,
        range: Range<u64>,
    ) -> Result<Bytes, PackError>;
    fn provide_iroh(&self, id: &ObjectPackId) -> Result<(), PackError>;
    fn fetch_iroh(&self, id: &ObjectPackId, from: &EndpointId) -> Result<(), PackError>;
}
```

Compression: zstd (trained dicts optional per eco later). Large members (≥1 MiB): generate
Bao outboard at put for verified range streaming over iroh.

### 6.3 Source acquisition policy (yes — reconstruction is in the plan)

**In scope for every ecosystem that ships a packaged artifact:** download once → normalize tree
→ ObjectPack. We keep the **same information quality** as the registry artifact (checksum,
file set) without retaining the vendor container format (no `.crate` / `.nupkg` blob as the
working representation).

```rust
pub enum SourceAcquisition {
    /// Preferred: grit clone/fetch at GitRev for this published version.
    Git {
        url: RepoUrl,
        rev: GitRev,
        /// Still record registry checksum of the published package for cross-check.
        registry_checksum: Option<ContentBlake3>,
    },
    /// Reconstruct: unpack .crate / nupkg / npm tgz / maven sources — then ObjectPack.
    /// Quality ≡ registry artifact; format ≡ our ObjectPack (rangeable zstd + TOC).
    ReconstructedRegistryPackage {
        registry: RegistryId,
        /// e.g. crates.io package sha256 for axum 0.7.9
        checksum: ContentBlake3,
        /// ephemeral fetch locator; not used at query time after pack exists
        package_uri: String,
    },
}

pub struct VersionProvenance {
    /// Published coordinates (axum @ 0.7.9 on crates.io).
    pub registry_id: RegistryId,
    pub version: VersionCanonical,
    pub registry_checksum: Option<ContentBlake3>,
    /// Where source ObjectPack came from.
    pub acquisition: SourceAcquisition,
    pub source_pack: ObjectPackId,
}

// NEVER: "serve/search opens live .crate from CDN as working tree"
```

**Per-item index row (e.g. axum):** yes — for that published version we store:

| Field | Example |
|---|---|
| package stem + version | `rust` / `axum` / `0.7.9` |
| `packages.repo_url` | upstream git URL from metadata |
| `versions.source_rev` | **git ref (tag/commit) used when kind=git** |
| `versions.registry_checksum` | crates.io checksum of the published `.crate` |
| `versions.source_pack` | ObjectPackId of materialised tree |
| `versions.source_kind` | `git` or `reconstructed_registry_package` |

So: the index knows **which published version** and **which VCS ref (or reconstructed pack)**
backs it — not “whatever is on main of the repo today.”

**crates.io:** sparse/git index = versions + dep requirements. `.crate` may differ from a git
tag (cargo package filters). Prefer repository + release tag/commit; else reconstruct from
`.crate` once; always keep checksum.

### 6.4 Git monitoring (grit)

```rust
// ingestor/git_monitor.rs
pub struct GitMonitor {
    grit: grit_lib::repo::Repository, // or grit-lib session per url
    catalog: MetaStoreHandle,
}

impl GitMonitor {
    /// Poll or receive hooks; update repo_facts + enqueue CatalogOp::SourceMoved.
    pub async fn tick(&self, stem: PackageStemId) -> Result<(), MonitorError>;
}
```

Catalog holds `git_watermarks(stem_id, last_rev, last_checked_at)`.

---

## 7. Dual-plane sync (catalog + IR + ObjectPack)

```
produce (SmolvmCage) → record IR channel → put ObjectPack(source/stage/golden)
  → register catalog (gens, locations, ir_status)
  → background iroh provide to trusted remotes
  → other hosts: fetch on demand; serve when available SOMEWHERE
  → promote branch → main: same content hashes = instant cache hit
```

### 7.1 Availability

```rust
pub enum IrAvailability {
    Local,                 // tip applied locally
    Remote { store_id: StoreId },
    Pending,               // catalog knows gen; IR not fetchable yet
    Missing,
}

pub enum ObjectAvailability { /* same shape for ObjectPackId */ }
```

### 7.2 Transport matrix

| Payload | Transport | Auth |
|---|---|---|
| Search hits, expand JSON, health | HTTP `/v1` | session/principal |
| Catalog prolly chunks | iroh ALPN `nudox/catalog-sync/1` | ticket / mTLS |
| IR change files | iroh (`ir-sync`) | same trust domain |
| ObjectPack members | iroh-blobs + Bao | DownloadGrant for non-trusted paths; trusted remotes enrolled |
| Golden VM images | ObjectPack `MemberKey::Golden` over iroh | trusted remotes |

### 7.3 Trusted remote upload (ID-18)

```rust
pub struct TrustedRemote {
    pub name: RemoteName,
    pub endpoint: EndpointId,
    pub can_provide: bool, // local may upload
    pub can_fetch: bool,
}

/// On seal (Embedded or edge Remote host):
/// 1. local present
/// 2. if online && trusted_remotes: provide IR changes + ObjectPack in background
/// 3. catalog push/merge when policy allows
/// When mainline already has the same hashes → zero extra work (cache hit)
```

---

## 8. Schema v4 (catalog.dolt) — implementer DDL source

All via `catalog_table!`. Codecs total. Migrations = list of `TableCreateStatement` /
`TableAlterStatement` from same macro.

```
packages
  stem_id BLOB16 PK
  ecosystem TEXT NOT NULL
  name_struct TEXT NOT NULL          -- purl / StructuredName canonical wire
  name_canonical TEXT NOT NULL
  name_original TEXT NOT NULL
  repo_url TEXT
  created_at INTEGER NOT NULL
  UNIQUE(ecosystem, name_canonical)

versions
  id BLOB16 PK                        -- PackageId instance
  stem_id BLOB16 FK NOT NULL
  version_canonical TEXT NOT NULL
  version_original TEXT NOT NULL
  published_at INTEGER
  toolchain TEXT                      -- json ToolchainSet digest/ref
  license_spdx TEXT
  yanked_upstream INTEGER NOT NULL DEFAULT 0
  parse_state TEXT NOT NULL           -- enum ParseState
  parse_phase TEXT
  attempts INTEGER NOT NULL DEFAULT 0
  failure TEXT                        -- json Failure
  source_kind TEXT NOT NULL           -- git | reconstructed_registry_package | unknown
  source_pack BLOB32                  -- ObjectPackId of source tree (nullable until acquired)
  source_rev TEXT                     -- GitRev (tag/commit) when kind=git
  registry_checksum TEXT              -- published artifact checksum (crates.io sha256, npm integrity, …)
  registry_package_uri TEXT           -- how we fetched the package blob if reconstructed (not opened live later)
  UNIQUE(stem_id, version_canonical)

  -- Provenance example: axum 0.7.9
  --   ecosystem=rust, name_canonical=axum, version_canonical=0.7.9
  --   repo_url on packages (e.g. https://github.com/tokio-rs/axum)
  --   source_kind=git, source_rev=<tag or commit for 0.7.9>, source_pack=<ObjectPackId>
  --   registry_checksum=<crates.io .crate sha256> always kept for honesty even when source is git

generations
  gen_stamp BLOB32 PK                 -- GenerationStamp domain (typed newtype)
  version_id BLOB16 FK NOT NULL
  channel_tip BLOB32                  -- nullable until first seal
  job_key BLOB32
  producer_toolchain TEXT
  sealed_at INTEGER
  ir_status TEXT NOT NULL             -- none|pending|sealed|failed  ← always set
  resolution_stats TEXT               -- json optional advisory

stores
  store_id BLOB16 PK
  kind TEXT NOT NULL                  -- ir_vcs_local|ir_vcs_iroh|object_pack_local|object_pack_iroh
  endpoint TEXT NOT NULL
  healthy INTEGER NOT NULL
  added_at INTEGER NOT NULL

generation_locations
  gen_stamp BLOB32
  store_id BLOB16
  status TEXT NOT NULL                -- present|pending|evicted
  PRIMARY KEY (gen_stamp, store_id)

object_locations
  object_id BLOB32                    -- ObjectPackId
  store_id BLOB16
  status TEXT NOT NULL
  PRIMARY KEY (object_id, store_id)

compile_cache
  job_key BLOB32 PK
  kind TEXT NOT NULL                  -- l0_tip|l1_stage|golden
  gen_stamp BLOB32
  object_id BLOB32                    -- ObjectPack member pack or tip ref
  image_digest TEXT
  updated_at INTEGER NOT NULL

edges
  dependent_version BLOB16 NOT NULL
  dep_ecosystem TEXT NOT NULL
  dep_name_canonical TEXT NOT NULL
  requirement TEXT NOT NULL
  resolved_stem BLOB16
  kind TEXT NOT NULL
  source TEXT NOT NULL                -- feed|manifest|git
  PRIMARY KEY (dependent_version, dep_ecosystem, dep_name_canonical, kind)

repo_facts
  stem_id BLOB16 PK
  stars INTEGER
  last_activity_at INTEGER
  archived INTEGER NOT NULL DEFAULT 0
  default_branch TEXT
  fetched_at INTEGER NOT NULL

git_watermarks
  stem_id BLOB16 PK
  last_rev TEXT
  last_checked_at INTEGER NOT NULL
  last_error TEXT

popularity
  ecosystem TEXT NOT NULL
  stem_id BLOB16 NOT NULL
  downloads INTEGER
  downloads_pct_ppm INTEGER
  dependents_pct_ppm INTEGER
  computed_at INTEGER NOT NULL
  PRIMARY KEY (ecosystem, stem_id)

facets
  version_id BLOB16 PK
  keywords TEXT
  quality_ppm INTEGER
  extras TEXT

listing_events
  seq INTEGER PRIMARY KEY AUTOINCREMENT
  version_id BLOB16 NOT NULL
  status TEXT NOT NULL                -- listed|withdrawn|advisory|deprecated
  reason TEXT
  valid_from INTEGER NOT NULL
  valid_to INTEGER
  recorded_at INTEGER NOT NULL

advisories
  id BLOB16 PK
  stem_id BLOB16
  version_range TEXT
  severity TEXT
  summary TEXT
  url TEXT
  valid_from INTEGER NOT NULL
  valid_to INTEGER
  recorded_at INTEGER NOT NULL

-- symbols table: SEARCH PROJECTION SHAPE ONLY (outbox materialize), not identity SoT
-- Identity = IntroId in IR. This table is wiped with tantivy.
symbols_proj
  intro_id BLOB32 NOT NULL
  version_id BLOB16 NOT NULL
  gen_stamp BLOB32 NOT NULL
  moniker TEXT NOT NULL
  kind TEXT NOT NULL
  PRIMARY KEY (gen_stamp, intro_id)

outbox
  seq INTEGER PRIMARY KEY AUTOINCREMENT
  version_id BLOB16
  gen_stamp BLOB32
  sink_kind TEXT NOT NULL             -- text|vector|usage_index
  op TEXT NOT NULL                    -- upsert|delete  (typed enum in Rust)
  created_at INTEGER NOT NULL

sink_watermarks
  sink_kind TEXT PK
  last_seq INTEGER NOT NULL
  updated_at INTEGER NOT NULL

overlays
  name TEXT PK
  remote_endpoint TEXT
  branch TEXT NOT NULL
  precedence INTEGER NOT NULL
  last_merged_commit TEXT
  added_at INTEGER NOT NULL

edgepack_artifacts
  edgepack_key_digest BLOB PK
  version_id BLOB16 NOT NULL
  recipe_fingerprint TEXT NOT NULL
  artifact_id BLOB
  ram_estimate INTEGER
  published_at INTEGER

schema_meta
  -- user_version via rusqdoltlite_migration runner
```

### 8.1 CatalogOp (ingestor → writer)

```rust
// index/protocol.rs
#[derive(Serialize, Deserialize)]
pub enum CatalogOp {
    UpsertPackage { stem: PackageStemWire, repo_url: Option<String> },
    UpsertVersion {
        coordinates: VersionCoordinates,
        published_at: Option<UnixMs>,
        toolchain: Option<ToolchainRef>,
        license: Option<String>,
        edges: Vec<EdgeWire>,
        facets: FacetWire,
        source: Option<SourceAcquisitionWire>,
    },
    SetRepoFacts { stem: PackageStemId, facts: RepoFactsWire },
    SetListing { version: PackageId, status: ListingStatus, valid_from: UnixMs, reason: Option<String> },
    UpsertAdvisory { advisory: AdvisoryWire },
    SourceMoved { stem: PackageStemId, rev: GitRev, checked_at: UnixMs },
    SetIrStatus { version: PackageId, status: IrStatus, gen: Option<GenStampWire> },
    Refresh { stem: PackageStemId },
}

pub trait MetaStore: Catalog {
    fn apply_ops(&self, ops: &[CatalogOp]) -> Result<ApplyReport, MetaError>;
    fn register_generation(&self, reg: GenerationRegistration) -> Result<(), MetaError>;
    fn outbox_claim(&self, sink: SinkKind, limit: usize) -> Result<Vec<OutboxRow>, MetaError>;
    // …
}

pub trait Catalog: Send + Sync {
    fn get_package(&self, id: PackageId) -> Result<Option<PackageRow>, MetaError>;
    fn changed_since(&self, cursor: CatalogCursor) -> Result<ChangedPage, MetaError>;
    fn at(&self, as_of: AsOf) -> CatalogAsOf<'_>;
}
```

---

## 9. Query algebra + search

```rust
// heart/query.rs
#[derive(Serialize, Deserialize)]
pub struct Query {
    pub target: Target,
    pub text: String,
    pub scope: Scope,
    pub rank: RankSpec,
    pub at: Option<AsOf>,   // prefer AsOf::Time externally
    pub page: PageSpec,
}

pub enum Target {
    Packages,
    Symbols,
    Usages { of: StableRef },
}

pub enum AsOf {
    Commit(CommitHash),
    Time(UnixMs),
}

impl SearchEngine {
    pub fn search(&self, q: &Query) -> Result<Page<Hit>, SearchError>;
}
```

**Replacement points:** delete `RegistryQuery`, dual `PackageSearchRequest`, `PackageSearchDeps`,
server `Pagination`, package half of `SearchRequestDto`, GUI wire sextet, handler double-cursor.

Pagination **only** via `PageSpec` inside the engine after rank.

---

## 10. SmolvmCage (first-class, reproducible)

```rust
// compiler/sandbox/smolvm.rs — already exists; make it THE cage
pub struct SmolvmCage<R: VmRuntime> { /* … */ }

impl<R: VmRuntime> Cage for SmolvmCage<R> {
    fn id(&self) -> CageId { CageId("smolvm-microvm") }
    // project CapabilityBudget → VmConfig (nix image by digest)
}
```

**Reproducibility rules:**
1. Toolchain images built with nix (`workspace/compiler/image.nix` + SMOLVM toolchain packs).
2. Image identity = OCI digest; JobKey includes that digest.
3. Golden: after warm boot of image digest D, checkpoint → ObjectPack `MemberKey::Golden` →
   iroh provide to trusted remotes.
4. Restore golden only if `image_digest` + `producer_version` match; else rebuild.

**Embedded:** one cage. **Remote:** pool of goldens per digest, still content-keyed.

---

## 11. Serving loops (Remote)

1. HTTP router — Query, expand, usages, authz, wanted, grants, health  
2. CatalogOp apply (writer)  
3. Outbox followers → tantivy / vector / usage reverse-index  
4. iroh catalog pull (replicas)  
5. ir-sync provide/fetch  
6. ObjectPack provide/fetch  
7. Forge: SmolvmCage → RecordingSession → register_generation  
8. GitMonitor + registry followers (ingestor process)  
9. Promote local/edge branches → main (background)  
10. Bakery  

**Embedded:** same libraries; single-threaded writer; one forge; no multi-replica sessions.

---

## 12. Ops & DR (replacement for Litestream “ops”)

| Metric / drill | Normative value |
|---|---|
| Writer RPO to backup remote | ≤ 30s (push after each commit batch; timer ≤ 30s) |
| Read replica lag | pull on outbox-head advance or ≤ 5s poll |
| Catalog restore | clone from backup remote OR file snapshot at commit boundary + `dolt` open |
| Projection rebuild | wipe tantivy/vector/usage_index; replay outbox + `changed_since` |
| IR restore | re-fetch change closure from any `present` store; apply tips |
| ObjectPack restore | re-fetch by ObjectPackId from providers |
| DR drill | CI weekly: kill writer → promote replica → search green; restore backup → green |
| `dolt_gc` | weekly on abandoned `local/*`; exclusive window; never force-rewrite `main` history |
| Commit pin policy | Prefer `AsOf::Time`; `AsOf::Commit` stable ≥ 90d on `main` |

---

## 13. Migrations (painless)

| Store | Mechanism |
|---|---|
| catalog.dolt | `rusqdoltlite_migration` + sea-query; `pre-migrate-vN` branch before run; fail ⇒ checkout |
| IR changes | immutable; new format_version on new changes only |
| ObjectPack | header version; readers support current and previous |
| projections | `schema_version` marker file; mismatch ⇒ full rebuild |
| scratch | delete anytime |

---

## 14. Dead code / replacement map (beginner checklist)

| Delete / stop using | Replace with |
|---|---|
| Postgres pools, schema, PgSessionStore, Litestream | `index` crate + scratch sessions |
| `registry/runtime/vector` legacy | vector-* features |
| Terminus client in server | Trustfall + usage reverse index |
| Ladybug | nothing (dead) |
| ApiSurface / DepSurfaceProvider | reflections + DepIrProvider |
| OccurrenceSeal / occurrences_ref / syntax/occurrence sidecar | `occ` frames + host resolve |
| SymbolTable / MergedSymbolTable | monikers reflection |
| Serving `.crate` as source | ObjectPack from git (grit) or reconstructed pack |
| Dual query DTOs | `heart::Query` |
| iroh-docs catalog | DoltLite remotes over iroh ALPN |
| Public IPFS | refuse |

---

## 15. Build order (gates a junior can run)

| Phase | Deliverable | Gate (must pass) |
|---|---|---|
| **IP-0** | rusqdoltlite vendor; engine facade; iroh catalog+ir-sync+ObjectPack smoke; macOS+Linux | open/commit/merge/gc; two-process iroh; Bao range get; bench ≤2× old PG ingest |
| **IP-0b** | Re-home IR workspace; kill Terminus/Ladybug deps from backend graph | `cargo build` three workspaces |
| **IP-1** | `index` crate schema v4 + MetaStore + scratch + migrations | property: codec, migrate up/down via branch |
| **IP-2** | heart Query/AsOf/Page; GUI client types switch | serde goldens |
| **IP-3** | Projections: tantivy + usage reverse index; outbox typed | rebuild-from-empty; NDCG floor |
| **IP-4** | Server swap; delete Postgres; Trustfall route; SmolvmCage only cage | integration `/v1` green |
| **IP-5** | Ingestor: registry followers + grit GitMonitor + advisories | separate process itest |
| **IP-6** | Dual-plane topology: trusted provide, serve-when-available, promote, offline branch | E2E §7; Availability cases |
| **IP-7** | `occ` + resolve-in-session + moniker reflections + usages Query | O-1..O-11 from 04-ir-unification §7 |
| **IP-8** | ObjectPack production path; source policy; golden iroh share | pack range-get; golden restore hit |
| **IP-9** | AsOf surfaces; retention jobs | time-travel tests |

---

## 16. GUI / client (subagent scope)

**Out of this file’s implementation detail**, but normative constraints for the GUI subagent:

1. Embedded profile only (`DeploymentKind::Embedded`).  
2. **No Ladybug.** Graph UI calls HTTP routes or local Trustfall over fetched IR.  
3. **No Terminus.**  
4. Offline: `local/<device-id>` catalog branch + local IrRepository + local ObjectPack.  
5. Trusted remote list in settings; background provide.  
6. Wire types from `heart` only (`Query`, pages, `StableRef`, `Confidence`).  
7. SmolvmCage for local compile; goldens from ObjectPack when present.

---

## 17. Risks (no open product questions)

| Risk | Response |
|---|---|
| rusqdoltlite youth | Vendor; IP-0 hard gate |
| Git tag ≠ `.crate` tree | Prefer git; record reconstructed packs with checksum honesty |
| grit-lib maturity | Pin version; thin adapter trait `GitRepo` so grit is swappable **only at adapter** — not a second product design |
| Trustfall perf | Reverse index projection for hot reverse queries; Trustfall for flexible queries |
| Commit growth | batch commits + dolt_gc + AsOf::Time |
| Poisoned promote | pijul verify + BLAKE3 ObjectPack verify before present |

---

## 18. Acceptance scenarios (product)

1. **Metadata AS OF** — dependents of X at time T via catalog only.  
2. **IR AS OF** — symbol + occ at channel tip pinned by gen.  
3. **No live .crate serve** — search hit → snippet from ObjectPack (git or reconstructed pack).  
3b. **Provenance** — axum@X row carries registry checksum + source_rev/source_pack used.  
4. **Git move** — grit sees new tag → CatalogOp → IR job.  
5. **Advisory** — listing_events/advisories bitemporal.  
6. **Offline seal** — device branch; reconnect provide; mainline cache hit.  
7. **Trusted upload** — only enrolled remotes accept provide.  
8. **Golden share** — host B restores golden via iroh; cage cold-start skipped.  
9. **Usages query** — Trustfall/reverse index; no Terminus.  
10. **Reflection only** — monikers() over IR; no ApiSurface artifact on disk.  
11. **IR status without store** — version row `ir_status=pending`, locations empty; UI honest.  
12. **Migrate catalog** — pre-migrate branch; rollback checkout works.

---

## 19. Reading order for a beginner implementer

1. This file §§1–4 (decisions)  
2. §5 IR + 04-ir-unification U-decisions  
3. §6 ObjectPack + §7 sync  
4. §8 schema + CatalogOp  
5. §9 Query  
6. §10 Smolvm  
7. §15 phases in order; never skip IP-0  
8. ECOSYSTEM-PLAN for follower details  
9. SMOLVM-PLAN for cage budgets (minus dead SV-6 never-upload — use ID-18)

---

*End INDEX-PLAN rev 3. Greenfield. No fallbacks. IR is the universe. Catalog is versioned SQL. Data moves on iroh+Bao. HTTP stays thin. Graph is Trustfall. Source is git/ObjectPack. Indexer watches registries and grit. Smolvm is reproducible nix.*
