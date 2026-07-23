# One global PURL-keyed IR graph — design exploration

> Status: in-progress synthesis (2026-07-22). Supersedes the overlay/federation
> framing in `LOCAL-REMOTE-PLAN.md`. Two sections (LSM mechanics §5, identity
> continuity §6) are being completed from research and are marked **[pending]**.

## 0. The corrected frame

The optimization target is **sharing IR**, not tracking identity or provenance.
Everything below serves that.

Authority is **inverted** from the earlier draft:

- **Remote IR = generic.** The most-broadly-valid build (canonical config,
  sandboxed, no ambient machine state). It is the **fallback / base layer**, and
  it is what's *available* before a local build exists.
- **Local IR = accurate.** Built in-process (2A — embed `ra_ap_*` etc.) against
  the developer's *real* machine: their env vars, `build.rs` outputs, feature
  selection, local-path deps, local binaries. It is the **ground truth for what
  the developer actually compiles**, and it **shadows** the generic layer.

So precedence is **local-accurate-first, remote-generic-fallback** — realised as
an **LSM-style layering** (§5): the local-accurate layer sits *on top* and wins
on read; the generic layer is the durable base underneath. Resolution switches to
local **piecemeal, per package, as each local build lands** — no flag day, no
merge, no fork attribution. A read for a symbol walks layers top→down and takes
the first hit.

This holds **even for external registry dependencies**: they have `build.rs` too,
so "upstream is authoritative" is wrong for them as well. The generic registry IR
is the fallback; the dev's locally-built version of that same dependency (under
the dev's resolved features/target) shadows it when present.

### 0.1 Three states of a package's IR (type-level, not heuristic)

| State | Meaning | Persisted upstream? |
|---|---|---|
| **Generic** | remote build, canonical config, hermetic | yes — the base graph |
| **Interpretation** | *same source bytes*, different env/features/`build.rs` output | no — a **temporary local view** |
| **Fork** | source bytes **edited** by the developer | no — local only; upstream recompiles *if/when* it merges |

The distinction Generic↔Interpretation↔Fork is decidable **by type + content**, not
by a matcher: an Interpretation shares the package's **artifact digest** (§2) with
the generic build — same bytes, different observation — so it is *not* a fork. The
moment the source bytes change (edit), the artifact digest changes and it becomes
a Fork. This is the honest, non-heuristic version of "different interpretation vs
true fork": **fork ⇔ input bytes differ; interpretation ⇔ same bytes, different
ambient state.**

### 0.2 What gets stored where

- The **remote registry graph stores only generic IR that is actually *used*** —
  referenced by some resolved dependency closure. Throwaway forks from throwaway
  projects never touch it.
- **Local** holds a **temporary view**: the LSM top layer(s) for the packages in
  the dev's active workspace closure, evicted like any cache. A package that is a
  *library the dev is authoring* is not persisted as authoritative anywhere — it
  becomes generic IR only when published and recompiled on the remote.

## 1. Identity: PURL is the golden standard

Package identity **is** the canonical [PURL](https://github.com/package-url/purl-spec):
`pkg:type/namespace/name@version?qualifiers#subpath`. This replaces the flat
`PackageLineageId = (ecosystem, name)`.

- **Canonicalize at ingest, never at query.** Percent-encoding + component-order
  normalization must happen once on the way in, or the global keyspace fragments
  (two byte-different strings denoting one package split the graph).
- **Forks are namespace, not overlay.** `pkg:github/openai/git` is a *distinct
  node* with a declared `forked-from` edge to `pkg:generic/git@<rev>` (Model 5).
  Enterprise/personal registries **add nodes** (and edges) to the one graph; they
  never overlay or alias an existing node's meaning.
- **PURL names a *version*, not *bytes*.** Byte-exact identity is a separate layer
  (§2) — required, because `build.rs` and non-immutable registries mean one
  version string can map to differing bytes.

Symbol identity *within* a package stays `IntroId` (deterministic, declaration-
local — §see identity discussion §6), and the cross-package edge is the
PURL-rooted `StableRef`. Full identity model treatment (Models 3+4+5) in §6.

## 2. Byte-exact, reproducible acquisition (the crates.io point)

Tracking a `Cargo.toml` version bump is **not** a reliable identity signal. Key IR
to **`purl@version` + the artifact digest**, and verify the digest against the
registry before generating IR — so IR is provably generated from the exact bytes
the registry serves.

| Ecosystem | Digest source | Field | Immutability |
|---|---|---|---|
| crates.io | sparse/git index entry | `cksum` = SHA-256 of the `.crate` | versions immutable; yank hides, not deletes |
| npm | registry packument | `dist.integrity` (SRI SHA-512) | pinned at publish; unpublish window exists |
| PyPI | JSON API release files | `digests.sha256` per file | first upload permanent; same-name re-upload rejected |
| Go | `go.sum` / checksum DB | `h1:` = SHA-256 of a canonical file-tree | **Merkle transparency log** (sum.golang.org) |
| Maven | detached checksum files | `.sha1` / `.sha256` | append-only repo policy |

Two takeaways: (1) every major ecosystem exposes a fetch-and-verify digest, so
"generate IR iff the `.crate` hash equals what crates.io serves" is implementable
everywhere; (2) **Go's transparency log is the most graph-native model** — it's
already an append-only Merkle structure you could federate with directly, and is
the shape to imitate for the global graph's canonical-content layer.

## 3. The global graph (candidate architectures)

Current code is strictly **per-package**: one `IrRepository` = one sanakirja
pristine + one changestore + one channel; `PackageArchive` seals one package;
cross-package refs are dangling `StableRef` *values* nothing resolves through a
shared structure. The pivot makes those cross-package edges **first-class** in one
graph, with packages as **shards/subgraphs**.

Precedents (research): **Software Heritage** — the whole public-source universe as
*one* content-addressed Merkle DAG (SWHID = hash of structure → free global dedup),
served as a compressed in-memory graph with a server-side **transitive-closure
API**, plus a columnar bulk plane. **deps.dev** — a PURL-native global dep graph,
served as a low-latency point API **and** a BigQuery bulk dataset (same two-plane
split), with dep graphs resolved against a *generic* target (literally "generic
IR"). Partial fetch: **IPLD/GraphSync selectors** (declarative "which subtree to
fetch" = the transitive dep closure of a package) and **Willow range-based set
reconciliation** (namespace-scoped incremental delta sync).

Four candidates:

- **A. Compressed-graph-per-shard + closure API (SWH-style).** Shard by PURL
  `type` (ecosystem); each shard a compressed in-memory graph with a BFS/closure
  endpoint; registries append nodes. Transport unit: a sub-DAG rooted at a PURL as
  a stream of content-addressed IR blocks. Weakness: cross-ecosystem edges need a
  thin cross-shard edge index.
- **B. IPLD/GraphSync-native. ★** Each IR node is a content-addressed block
  (CID derived from the artifact digest, §2); `PURL → CID` via a light signed
  name-service; closure fetch = a GraphSync **selector** query. Sharding is
  implicit (content-addressing distributes); registries "**merely add nodes**"
  most literally — publish blocks + update a pointer, zero coordination. This is
  the most direct match to your stated goal.
- **C. Columnar bulk + point-query API (deps.dev-style).** Whole graph as
  Parquet/ORC on object storage partitioned by `(ecosystem, name-prefix)`, plus a
  low-latency `GetClosure(purl)` service. Cheapest on existing infra; weakest on
  peer-to-peer partial sync.
- **D. Willow-reconciled namespace shards over an IPLD substrate. ★** B's
  content-addressing + Willow range sync for *incremental* per-namespace updates,
  so a mirror reconciles only new ranges since last pull instead of re-walking
  selectors. Best fit if "registries add nodes" must also mean "stay live-synced
  cheaply"; cost is a newer, less-proven protocol.

**Recommendation: B as the substrate, layered with D for incremental sync**; A/C
are lower-risk fallbacks for the query/analytics plane. Shard boundary = PURL
ecosystem prefix (natural, matches per-ecosystem resolver + hash semantics); hot
popular-ecosystem shards get sub-sharded by name-prefix or hash.

## 4. The libpijul split (resolving the "big rethink")

libpijul's pristine is **per-repository** by construction (a sanakirja B-tree over
*one* repo's file-line graph). One giant pristine for all packages is the wrong
shape — it models *file-line history*, not a *semantic cross-package node graph*.
The resolution is not "replace libpijul" but **split the two roles it's currently
conflating**:

1. **Per-package temporal engine → keep libpijul.** "How did *this* package's IR
   change across generations" stays a per-package channel/pristine. This is the
   part that maps cleanly onto git and is worth preserving unchanged.
2. **Global cross-package graph → a new derived, content-addressed, shardable
   index** over all packages' *current tips* (§3, arch B). This is the plane
   clients query and piecemeal-download; it is **derived and disposable**,
   rebuilt from the per-package tips, never the source of truth for history.

So libpijul answers *time* (per package); the global graph answers *space*
(across packages), and it's the graph — not libpijul — that clients shard and
partially replicate. The generic base layer (§5) is a snapshot projection of the
global graph at a set of tips.

### 4.1 Pijul relates channels by change-set membership (not branch points)

Confirmed from the pijul manual/theory: **a channel is "a pointer to a set of
changes," not a commit chain, and two channels relate purely by hash-set
intersection of their change memberships.** There is *no branch-point object* —
none is computed, stored, or needed. A change is a self-contained, BLAKE3-
identified unit carrying its dependencies as explicit hashes (a DAG/poset;
trichotomy: A depends on B, B depends on A, or they commute). Pristine state =
`f(change-set)`, order-independent (CRDT). `fork` is **O(log n)** copy-on-write
(sanakirja page sharing — "fork without copying a byte").

**Consequence for the local/generic model:** a `local-*` branch off an old rev
and an advanced generic branch are *never* "unconnected" — their common ancestry
is just the always-available intersection of two hash sets, exact regardless of
how far they diverged. No synthetic merge-base. So:

- **Coverage** ("how much complete IR do we have locally") = the **change-set
  intersection** of the local channel and the generic channel.
- **The local-accurate-over-generic delta** = their **symmetric difference**.
- Both are cheap set ops, native to pijul — the *local* side needs no manifest
  (the manifest is only for the *remote* content you don't yet have, §5/§12).

### 4.2 Pijul changes ≡ IPLD blocks (the two backends are one DAG)

A pijul change is *a content-addressed unit with explicit dependency hashes
forming a DAG* — which is exactly what an **IPLD block** is. A channel is a
labeled root-set over that DAG; materializing it applies the downward dependency-
closure. Therefore **"materialize pijul from IPLD" is not a conversion — the IPLD
graph *is* the change store, and a channel is a named subset of it.** The
`IpldResolver` and `PijulResolver` (§12) are not two storage models; they are two
*views* of one content-addressed change DAG — raw blocks vs. a materialized
pristine. This is what makes the four-tier hierarchy (§12) coherent: every tier
is the same DAG, differently materialized.

## 5. Layering: pijul branches (not LSM) + IPLD storage

### 5.0 Correction (2026-07-22): LSM was two jobs; split them

LSM does not sit on the pijul model — correct. LSM was smearing **two** jobs into
one abstraction; split them and the mismatch vanishes:

- **Layering / merge-on-read (local shadows generic) → pijul branches.** A
  `local-<env-fingerprint>` branch off the *significant generic rev* is the
  layering primitive. `output` the local branch → local-accurate; `output` the
  generic channel → generic. "Which layer wins" = "which channel you
  materialize." The branch **is** the local-vs-generic delta (empty when the local
  build is byte-identical to the generic tip; one change when env-observed), which
  pijul gives for free and tombstones only fake. **Provenance is free**: the
  branch point is a real VCS fact, so the local↔generic relationship needs no
  continuity heuristic (the matcher shrinks to intra-package renames, §6.4).
  Branch names use an **env fingerprint** (toolchain + resolved features + target
  + capturable `build.rs` witness) so identical envs *converge* on one name
  (enabling LAN share); a random suffix is only the fallback for non-deterministic
  builds. Branches are cache — LRU-evicted on project close.
- **Storage / delivery → IPLD/Willow + S3.** Immutable content-addressed IR blocks
  in the global graph (§3), delivered via iroh (§7), durable on S3 behind the
  blobs. **Pijul is materialized on demand from the IPLD graph, not stored** —
  "download the bare pijul store" = fetch the package's IPLD subgraph and apply it
  into a local pristine. Latency: fetch (~sub-second) + pijul-construct (~50–300ms)
  is *negligible* beside local IR build (seconds+), so generic-first + local-in-
  background is correct.

The **coverage map** ("how much complete IR do we have locally") is the surviving
manifest: a per-project `package → has-local-branch` bitmap, the routing signal
that delegates each search to local-branch or remote-generic.

The rest of this section keeps the LSM read-merge *intuition* (recency shadows by
key) as the mental model, but the *mechanism* is pijul-channel `output`, and the
*storage* is IPLD — not SSTables.

### 5.1 Why the read-path *is* "local shadows generic"

An LSM read probes structures in strict **recency order** — memtable → newest
SSTables → … → oldest — and **the first structure that has an entry for the key
wins, full stop.** There is no merge of *values*, only a merge of *presence*:
recency alone decides which version you see. A **tombstone** is a write like any
other that shadows older values with "absent." This is exactly the model we want,
with **zero new machinery**:

- **base layer** = remote-**generic** IR: immutable, content-addressed SSTables
  (one per package@digest), each with a bloom filter + `IntroId` min/max range so
  a reader skips files that can't hold a key.
- **overlay** = local-**accurate** IR: the dev's just-built symbols in a small
  local memtable (in-process from 2A, or a tiny local SSTable set), always probed
  first.
- **read** = the LSM merge: probe local, fall through to the generic base only on
  a miss. First hit wins. **No fork, no attribution, no value-merge** — "local"
  and "generic" are just two SSTable generations at different recency, and the
  engine already knows how to shadow older generations.
- **"symbol absent under my config"** = a **tombstone** in the local overlay
  (feature-gated-out, platform-specific) — shadows the generic entry with absent.
- **"switch in piecemeal as each package builds"** = each locally-built package
  becomes a new top-of-manifest SSTable, exactly like an LSM flushing a memtable
  to L0. A half-built workspace is just an overlay manifest **with holes**; reads
  for not-yet-built packages fall through to the generic base *by construction*,
  not by special-casing. This is your "piecemeal local-first resolution."
- **"compaction" = publish.** When local IR is accepted as canonical upstream,
  it's compacted into the generic base — the same merge-and-drop-shadowed-entries
  operation LSMs already run, triggered by "merged upstream" instead of a size
  threshold.

### 5.2 The S3 delivery pattern (your "integrate S3 + get data to devices")

Because SSTables are immutable, the whole thing disaggregates over object storage:
**object storage is the source of truth for immutable segments; a small,
strongly-consistent manifest is the only thing needing synchronous coordination;
clients are stateless readers that cache hot segments and cold-**range-read** the
rest**, using bloom/min-max/hotcache metadata to avoid fetching irrelevant
segments. Systems to imitate: **SlateDB** (LSM directly on S3, only the manifest
kept consistent), **RocksDB-Cloud** (SSTs 1:1 to S3 objects, working set on local
SSD, remote compaction), **Neon** (cold delta/image layers in S3, hot cached),
and especially **Quickwit** (immutable splits + a ~10 MB hotcache let a searcher
cold-open a multi-GB split from S3 in <60 ms and range-GET only the slices a query
touches).

This *is* the answer to "getting data to local devices," and it fits your usage
profile precisely: **mostly-read, rarely-updating clients keep a warm cache**, the
manifest changes rarely (cheap invalidation = bump a generation counter, diff the
file list), and steady-state cost is a handful of range GETs on cache misses — not
a bulk sync. Clients never talk to S3 directly; S3 is the **durability backing for
the immutable base segments** behind the transport layer (§7, §9).

### 5.3 Cases where the client *does* fetch, beyond "upgrading"

You said you don't see users upgrading packages often, and local data isn't
written back — both true, and the LSM/mostly-read model is built for that. But
several flows still push data to a quiet client, and they should be designed for:

1. **Security advisories / yanks / withdrawals.** A package a client already holds
   can get a CVE or be yanked. That status must propagate even with no upgrade —
   a small manifest/status delta (a "flagged/withdrawn" tombstone-like edge),
   which the iroh-docs reconciliation (§7) carries ∝ diff.
2. **Transitive-dep / lockfile resolution changes** pull new generic IR even when
   direct deps are pinned.
3. **Toolchain / target changes** regenerate local IR (a new *interpretation*,
   §0.1) with no package upgrade at all.
4. **Producer-version improvements.** The remote may recompute *better* generic IR
   (richer extraction, new producer) for the *same* package@digest; clients
   benefit from re-fetching without any version change.
5. **New-project cold closure.** Opening a fresh project pulls a large dep-closure
   burst once, even if steady state is quiet.
6. **Cache eviction/GC** forces re-fetch of previously-held IR.
7. **(Opt-in) team LAN sharing** — a teammate's identical-env local-accurate IR
   could be pulled peer-to-peer over iroh LAN instead of rebuilt; gated on an
   env/digest match, since local IR is env-specific (§0.1).

### 5.4 Where LSM fits a graph poorly — and the fix

LSM's shadow/tombstone story is airtight for **point lookups** (IntroId →
declaration). It strains on **graph structure** in three ways, each with a known
fix:

1. **Edges have two endpoints.** A single sorted KV space can't traverse from
   either side. Fix: **adjacency-list encoding** — emit `(src, kind, dst) →
   payload` for outgoing and a mirror `(dst, kind, src) → payload` for incoming,
   so "neighbours of X" is a prefix range-scan. (This is why RocksDB backs Dgraph
   et al., and it's exactly your existing `Reference` reverse-projection.)
2. **Edge-level shadowing is subtler than node-level.** If a local overlay
   tombstones node X but a generic-layer edge still points at X, you get a
   dangling edge; if local adds an edge a stale generic reverse-index doesn't
   know, you get a duplicate. **Edge tombstoning must be *derived automatically*
   from node tombstoning**, never hand-maintained — this is the one real
   correctness obligation the graph layering adds.
3. **Multi-hop = many small reads**, not one scan. Fix: either precompute
   transitive-closure edges as their own KV rows at IR-build time, or — better —
   put a **graph-query engine (Trustfall) *above* the LSM**, using the shadow-
   aware, S3-backed KV purely as sharded node+adjacency storage, and let Trustfall
   own traversal semantics. This is exactly the split your `resolution-ir-
   unification` / Trustfall plan already implies: **Trustfall = traversal; LSM =
   shadow-aware sharded storage; the two compose cleanly.**

### 5.5 Provenance class + the publish-exclusive-IR flow

Every IR node carries a **provenance class** that dictates storage/publish policy
structurally (not by convention):

- **`derived`** (hermetic): reproducible anywhere from `(source@digest, toolchain,
  config)`. Dedup globally; any node can regenerate it; lives in the **public
  generic graph**.
- **`observed`** (env-witnessed): a witnessed result of running on one machine —
  `build.rs` side effects, local binaries, local paths. Carries a machine witness
  + (on publish) a publisher signature. Lives **only where produced and wherever
  explicitly published**; never in the public generic graph.

This makes the **publish-exclusive-IR** flow first-class (the real gap in §5.3):
some IR *only the local can produce* because the remote cannot reproduce it — an
internal crate whose `build.rs` links a proprietary binary, or generated bindings
depending on local tools. For these, the local (or CI) is the **only** authority,
and an **enterprise/private registry persists the `observed` node** so teammates
can *fetch* what they can't rebuild. So authority inverts *only* for the
irreproducible case: public packages → remote-generic wins; private-irreproducible
packages → local `observed` IR is the sole source. This is where private
registries earn their keep.

### 5.6 The three-tier fidelity ladder (treesitter first paint)

IR splits by **config-sensitivity**, giving three tiers that each shadow the one
below as it arrives:

1. **Treesitter skeleton** — declared items + signatures at the *syntax* level. No
   compilation, no `build.rs`, no resolved types → **config-invariant**,
   computable in **milliseconds on any node, before download completes.** First
   paint of names/structure/signatures.
2. **Generic oracle** — remote build, fetched (~sub-second). Accurate types,
   generic config.
3. **Local oracle** — built locally via 2A (seconds+). Perfectly accurate to the
   dev's reality; lands as the `local-*` branch.

The skeleton tier does two jobs: (a) it carries first-paint UX before any network,
and (b) it is the **cheap change-detector that gates incremental oracle reuse** —
a millisecond treesitter diff answers "did *my source* change?" But
skeleton-unchanged is **necessary, not sufficient**: my resolved types can change
because a *dependency's* IR changed. So the reuse gate is *skeleton-unchanged AND
referenced-dependency-IR-hashes-unchanged* (the Salsa red/green condition, §Mech A,
made nearly free by treesitter). When a base dependency's ambient config shifts
fundamentally, broad oracle invalidation is **correct, not a failure** — the
skeleton tier absorbs the UX while the oracle recomputes.

**Cross-version reuse (the concrete win):** treesitter-diff a new package version's
skeleton against the previous; run the oracle only on changed-skeleton symbols
(gated by dep-IR-hash). A patch touching 3 functions reuses 997. Skeleton parsing
is safe anywhere (client pre-download triage); the oracle-reuse *substitution* runs
in the sandbox where the oracle runs.

### 5.7 Two branch kinds (purity-classified), = the provenance class as a name

The `local-*` branch name is not a blind env-hash. Classify **conservatively** by
verified purity (default to machine-local; promote to reproducible only on proof):

```rust
pub enum LocalBranch {
    /// Generation VERIFIED pure w.r.t. env (static: no build.rs / no proc-macro /
    /// hermetic inputs; OR empirical: two cluster nodes built byte-identical IR).
    /// env_fp is meaningful → identical envs CONVERGE on one name → shareable.
    /// (== provenance class `derived`, §5.5.)          local-r-<env_fp>
    Reproducible { env_fp: EnvFingerprint },
    /// Purity unverifiable (build.rs reaches ambient state / local binaries /
    /// local paths). The IR is a WITNESS of one machine; the id names the
    /// producer, not a reproducible env. No cross-machine convergence.
    /// (== provenance class `observed`, §5.5.)          local-m-<machine_fp>
    MachineLocal { machine_fp: MachineFingerprint },
}
```

Correctness bias mirrors under-merge: a wrong `Reproducible` claim lets two
divergent machines collide on one branch name — the one corruption to forbid — so
refuse the claim unless proven. **Teammate API-surface view:** a teammate's
`MachineLocal{machine_fp}` branch is *their machine's* observed IR; the GUI offers
a read-only picker ("view Alice's surface"), correctly labelled as her machine's
interpretation, never merged.

### 5.8 Purity depth per language (what gates the `Reproducible` branch)

`Reproducible` (env-fingerprint) vs `MachineLocal` (§5.7) is gated by how much
ambient state can reach the IR at compile time — and *how deep*: bodies only, or
all the way into **types**. The "type-thing": const generics / non-type template
params / comptime-typed languages let ambient config reach a **signature**
(`Buffer<{env-derived N}>` → different types by env), so purity must be assessed
for the signature, not just the body.

| Language | Compile-time ambient vectors | Reaches types? | Verdict |
|---|---|---|---|
| Nix | eval, but impure ops **marked** (`getEnv`, IFD) | yes | easiest — enforced-pure |
| Go | `go:embed`, build tags, `-ldflags -X`, `go:generate` | no | easy — no comptime; impurity external/enumerable |
| Java / C# | annotation processors / Roslyn source generators | rarely | medium — one analyzable vector |
| TypeScript | `const enum`; Turing-complete but **pure** type system | yes, **purely** | easy for values, safe for types |
| Python | import-time arbitrary (decorators/metaclasses/env) | via runtime typing | oracle sees *declared* types only |
| Rust | `env!`, **`build.rs`**, **proc-macros**, `include!`, const generics | **yes** | hard — Turing-complete + ambient |
| C/C++ | preprocessor, `__DATE__`/`__TIME__`, generated `config.h`, `constexpr`, templates | **yes** | hard — build system *is* the config generator |
| Zig | `comptime` (`@embedFile`,`@import`); **types are comptime values** | **yes, natively** | hardest in principle, but sandboxed → analyzable |
| D | CTFE, `mixin`, `import()` | yes | hard |

Promote to `Reproducible` only on proof — **static** (no `build.rs`/proc-macro/
`env!`/`include!`/ambient-fed const generics) or **empirical** (cluster double-build
byte-match, §10). This *reinforces* the tier ladder (§5.6): the treesitter skeleton
is pure *syntax* → config-invariant even in Zig; the **oracle** (type resolution) is
exactly where env can leak, including into signatures — which is why the purity risk
lives at the oracle tier, not the skeleton tier.

## 6. Identity: the Model 5+4+3 stack, refined by what's provable

### 6.1 The hard result: continuity is undecidable

Two independent facts foreclose a non-heuristic "is S′ the same symbol as S":

- **Semantic equivalence is undecidable** (Rice's theorem). "S′ computes the same
  function as S" is a non-trivial semantic property; no terminating algorithm
  decides it for a Turing-complete language. Your **num→bool example
  (`x != 0` vs `bool(x)`) is provably outside *any* syntactic hash — Unison's
  included.**
- **Even syntactic tree-mapping is non-unique** whenever the trees contain
  repeated/near-identical substructure (two structurally-identical helpers): "the"
  mapping is a *choice*, not a fact.

So continuity is irreducibly a **best-effort classification**, and every method —
GumTree, ChangeDistiller, difftastic, winnowing, MinHash, SimHash, even Unison's
exact-syntax hash — is heuristic or syntactic-only at core. **There is no
breaking-edge research that changes this; it's a theorem, not a gap.**

### 6.2 How Unison actually hashes, and why it doesn't transfer

Unison identifies each definition by a **SHA3-512 of its normalized syntax tree**:
(1) **alpha-equivalence via De Bruijn indices** — local variable names erased to
positions, so renamed locals hash identically; (2) **dependencies referenced by
their own hash, not by name** — the hash is over a name-erased, dependency-closed
graph fragment, so renaming a dependency never perturbs a caller; (3)
**mutually-recursive cycles hashed together** as an SCC, each member identified by
`(cycle_hash, index)`.

It **normalizes**: local names, formatting, the namespace name, dependency
spelling. It does **not** normalize: operand order, recursion scheme (fold vs
explicit), inlined-vs-factored, or any algebraic/observational equivalence — those
get *different* hashes. It works because Unison is a **small, pure, macro-free
language with one canonical elaborated AST**, no implicit conversions, no
overloading resolved by out-of-band type context, and a compiler-owned namespace
separating names from identity. Rust/TS/Python have none of that — macros/codegen,
implicit coercions, overloading, and surface-syntax pluralism mean identical
syntax can differ in meaning and vice versa. Retrofitting would require a full
canonicalizing semantic elaborator per language, **and even then Type-4
equivalences (num→bool, reorder, inline) stay out by §6.1.** Verdict: Unison's
trick is real but **non-transferable**.

### 6.3 What survives: Model 3 is nominal-id + continuity-*edges*, not a dual-*id*

The dual-coordinate idea (a deterministic nominal id **and** a deterministic
content id) does **not** survive contact with §6.1 — there is no clean content
*id*, because a content hash either misses Type-3/4 (Unison-style) or is a
similarity *score*, not an identity. The refinement:

- **Nominal id = `IntroId`** (deterministic, declaration-local, PURL-rooted per
  Model 4). This stays the **one** primary key: it drives graph identity, LSM
  keys, and compute reuse (§8). Exact, reproducible, convergent across machines.
- **Continuity across renames/moves = a derived, best-effort *edge*** in the
  provenance DAG (Model 5), computed by the pipeline in §6.4 — **not** baked into
  identity. This is precisely the "explicit, optional, clearly-fallible
  annotation rather than load-bearing machinery" you asked for two rounds ago.

So the stack is: **Model 4** (PURL-rooted coordinate) names packages+symbols;
**Model 5** (provenance-DAG) records forks and — now — rename/move continuity as
*declared or best-effort-inferred edges*; **Model 3** contributes the *nominal*
axis only, with its content axis demoted from "id" to "continuity signal."

### 6.4 The continuity pipeline (correctness-preserving, storage-reducing)

1. **Candidate generation (cheap, corpus-scale):** a **MinHash sketch** over
   k-shingled, alpha-renamed AST-subtree/token sequences per symbol. It's the one
   metric *designed* for "did this symbol survive an edit" (Jaccard degrades
   gracefully under small edits instead of collapsing), ~100–200 ints to store,
   sub-linear top-k via LSH. Use it only to **shortlist**.
2. **Structural verification (exact, per-pair):** **APTED** exact tree-edit-
   distance (or GumTree mapping) between the old symbol and each shortlisted
   candidate. TED is the *most deterministic* option — a true metric, same trees →
   same cost on any machine, no seed or corpus-tuned threshold — cheap at *symbol*
   granularity (tens–hundreds of nodes), never run pairwise across all symbols.
3. **Never silently merge an ambiguous pair.** Near-tie or borderline edit cost →
   record an explicit *unresolved-continuity* edge, or just treat old-as-deleted +
   new-as-new. **The asymmetry is the whole correctness argument: a false merge
   irreversibly splices two unrelated histories** (corrupting blame, evolution,
   every longitudinal query); **a false split only costs some redundant rows,
   re-linkable later.** Bias every threshold toward **under-merging.**
4. **This bias also reduces stored history and speeds history walks** (your stated
   want): fewer, higher-confidence edges keep the version graph a set of short,
   confident chains instead of a dense speculative tangle, so a blame/ancestry
   walk is a straight walk, not a search.
5. **Type-4 is explicitly out of scope:** if two versions are semantically
   equivalent but structurally unrelated, report "no structural continuity" rather
   than attempt undecidable equivalence checking.

### 6.5 Where the 5+4+3 stack falls short (honest)

- **No semantic continuity.** num→bool, commutative reorder, inline/extract,
  loop↔fold all read as "new symbol." Acceptable under the never-merge policy, but
  history *understates* continuity — safe, not complete.
- **Boilerplate false-positives** in candidate generation (getters, common idioms
  share shingles) — filtered by the verification step, at CPU cost.
- **The content axis is a signal, not a key** — so continuity is a *classification
  with an error mode*, not a lookup. You must be comfortable that some real
  renames won't be linked (they degrade to delete+new).
- **Provenance edges can be misdeclared** (a fork lying about its parent) — but
  that's a *claim*, not an inference, which is the honest failure mode.
- **MinHash/APTED need per-symbol AST access** — fine, the IR *is* that, but it
  couples continuity to having both generations' IR materialized.

### 6.6 The unified similarity surface: pq-grams (one decomposition, three resolutions)

Similarity runs **only on the id-changing case** — exact keys handle the rest:

```
same IntroId + same sig_key      → identical decl identity      (no op)
same IntroId + different sig_key → SignatureEvolved{old,new}     (EXACT — f1.rs already has this op)
different IntroId                → pq-gram surface (below) → RenameEdge / moved / new
```

Candidate-gen (MinHash) and exact-verify (APTED) are **not two stages** — they are
**one feature decomposition, the pq-gram profile** (Augsten et al.), read at three
resolutions. A **pq-gram** = p ancestors × q consecutive children (null-padded); the
**profile** = the bag of all of them, extracted **O(n)** in one traversal. Node
labels = `(kind_disc, type-skeleton token)` via `ir/skeleton.rs`, with
`KEY_SPAN`/`KEY_DOC`/`KEY_NAME`/`KEY_CFG`/locals excluded → an alpha-normalized IR
tree.

```rust
pub struct PqProfile {
    grams:  SortedBag<PqGram>,     // p ancestors × q children
    sketch: [u8; 128],             // MinHash of `grams`         → (a) KEY_SKETCH F1 line
    hist:   LabelDegreeHist,       // label + fanout histograms  → extra provable bounds
}
impl PqProfile {
    pub fn of(e: &OwnedEntryPayload, body: Option<&TreesitterBody>, p: u8, q: u8) -> Self; // O(n)
    pub fn lsh_bands(&self)          -> impl Iterator<Item = BandKey>; // (a) corpus-scale candidates
    pub fn distance(&self, o: &Self) -> f32;                          // (b) TRUE METRIC, O(n log n)
    pub fn ted_lower_bound(&self, o: &Self) -> f32;                   // max(scaled dist, hist bounds) — PROVABLE
}
pub fn continuity(old: &Sym, new: &Sym, lb: f32, thr: f32) -> Decision {
    if lb > thr { Decision::Distinct }                    // pruned by the bound — safe, NO false negative
    else        { classify(apted(&old.ast, &new.ast, &IrCost)) }  // exact ONLY on survivors
}
```

- **(a)** the bag is *shingles* → directly MinHashable; the sketch's Jaccard estimate
  *is* the bag overlap → LSH candidate retrieval.
- **(b)** the normalized bag difference is the **pq-gram distance** — a true metric,
  **O(n log n)**, and a **provable lower bound on (fanout-weighted) TED**. Lower bound
  ⇒ **safe to prune, no false negatives** (a learned embedding can't promise this).
- **(c)** that bound seeds branch-and-bound around **APTED**, run exact only on pairs
  the bound can't rule out. Output → existing `ContinuityOp`/`RenameEdge`.

**"Small changes not outsized" is now a built-in property, not a knob:** pq-gram
distance bounds *fanout-weighted* TED, which penalizes edits at high-fanout nodes
more — and in IR, high fanout = many fields / params / items = a genuinely bigger
edit. A renamed local (leaf, fanout 0) barely moves the profile; adding a field to a
20-field record moves it proportionally. **Correctness vs learned embeddings:**
code2vec/ASTNN/tree-transformers give ANN+score in one vector space but have *no
metric/bound guarantee* (unrecoverable false negatives, training-dependent) — keep
them for a *separate* semantic "similar code" layer, never for continuity. Safe
pruning uses `max(scaled pq-distance, label-hist, degree-hist)` — all three provable
lower bounds, tightest wins. Crates: `probminhash`/`gaoya` (MinHash), `apted` port.

## 7. iroh range-based set reconciliation — how we actually use it

`iroh-docs` replicated KV keyed by `(namespace, author, key)`; entries carry only
`{blake3, size, timestamp}` — **not bytes**. Sync uses **range-based set
reconciliation** (Willow lineage): two replicas exchange fingerprints over
recursively-bisected ranges, transferring only where fingerprints diverge — two
already-synced replicas confirm equality in **one round trip regardless of size**,
and a real diff costs ∝ diff.

Concrete use in this design:

- **`namespace` = a PURL-scoped index shard** (e.g. `pkg:cargo/*` or one
  enterprise registry). The heavy generic node is the **writer**; clients are
  readers.
- **`key` = a graph node's PURL+IntroId; value entry = the IR block's blake3 +
  size.** So a client reconciling the namespace learns *which nodes exist and
  their content hashes* cheaply, without pulling IR bytes.
- **Then `iroh-blobs` fetches the actual IR blocks by hash on demand** (bao-tree
  verified ranges — pull only changed sub-ranges of a shard/archive), peer-to-peer
  / LAN when a path exists, else from the heavy node.
- **This is the delta channel for the global graph:** when the generic graph
  advances (new generations at new tips), a client re-reconciles the namespace
  (∝ diff) and blob-fetches only the changed nodes. It is also the transport for
  Mechanism A/B compute-sharing — the reconciled entries can point at memo/
  arrangement batches, not just IR blocks.

## 7.5 The three-layer storage model (content DAG + signed refs + COB)

The universal pattern (git, Radicle, IPFS, iroh-docs): **a large immutable
content-addressed object set + a tiny *mutable signed pointer record* naming
entry-points into it.** IPLD "not having branches/tags" is a non-problem — *no*
system puts refs in the content layer; you add a small record on top. The whole
system collapses to three layers, only two of which ship:

1. **Immutable content-addressed DAG** — pijul **changes** (each a hash + explicit
   dependency hashes = already a Merkle DAG; tags are changes too), IR blocks, and
   **embeddings**. Everything derived-and-addressable.
2. **Signed refs records** (the "manifest") — per channel: `channel → {tip
   change-hashes, tag hashes}`. Radicle's **`sigrefs`** model: a per-writer *signed*
   blob, **one namespace per peer** (`local-*` branches and generic are just
   different namespaces → one writer each, no conflicts), reconciled by **iroh-docs
   RBSR**, with a **monotonic counter** for freshness (Radicle shipped a replay CVE
   from missing this). Large flat change-sets shard as a **HAMT** (update rewrites
   only trie-path blocks).
3. **COB-style CRDT metadata** — the *mutable annotations over the content DAG*:
   provenance edges (forks/`forked-from`), continuity edges (renames, §6), purity/
   authority classes. Signed change-ops merged as a CRDT, converge regardless of
   order. Not IR content, not refs — their own home.
- *Derived, never shipped:* pristines (sanakirja), reverse indexes, vector indexes,
  tantivy.

**Embeddings live in layer 1**, keyed by `H(recipe_version ‖ model ‖ H(embed_text))`,
with a thin `(IntroId@generation) → embed_key` map — *not* in pijul (a model upgrade
would otherwise churn history), and content-addressing **dedups** identical
embed-texts (boilerplate embeds once). Qdrant/edge is the derived ANN *index* over
them; the map is a small manifest entry.

### 7.5.1 Corrected: the intersection does not avoid a manifest

Earlier claim retracted. Computing `local ∩ remote` needs the remote's set, which
you fetch — that *is* a manifest. But it's **tiny + content-addressed**: each
package channel has a **state hash** (one digest over its change-set), so the
in-sync case is **one hash compare** (no enumeration), and the divergent case costs
**∝ delta** (RBSR the differing change-hashes, then `iroh-blobs` fetches the change
bodies). "Reach out to the remote for their manifest" = fetch+verify their signed
refs record (layer 2) for that package.

### 7.5.2 Reconstructing a pijul repo from content-addressed blocks

1. Resolve identity → fetch latest **signed refs record** per peer; verify signature
   + freshness counter.
2. Read each `channel → {tips, tags}`.
3. Pull the wanted **change closure**: transitively fetch every change reachable via
   dependency hashes from the tips (content-addressed, verify each block) — this is
   the "packfile" step *and* your coverage/intersection (fetch only what you lack).
4. Per channel: fresh empty **pristine**, **apply changes in dependency order**
   (topo-sort; independents commute).
5. Register tags + set channel tips from the record.
6. Materialize the working copy.

**Canonicity is a deployment mode, resolving the etcd-vs-iroh-docs question:** the
central generic index runs **etcd** (single linearizable canonical tip); enterprise/
personal registries and client peers run **signed-refs + RBSR** with a **delegate-
threshold** canonical rule (Radicle). Same refs-record shape, different consistency.

### 7.5.3 Querying under partial coverage (qdrant best-possible-at-T)

Coverage is per-package-shard (`depshard`); a project's packages are *local-built*
(a), *generic-fetched* (b), or *not-fetched* (c). The vector query: partition the
scope by `Manifest::coverage`; query local edge shards for (a)+(b-local) and remote
qdrant for the rest; **RRF-merge** (ranks, not raw scores — requires the *same
model* both planes); return with a **completeness envelope** ("N local-accurate, K
remote-generic, J not yet available"); **refine as shards land** (the optimistic→
accurate transition, streamed). Never block on full coverage, never silently
under-report.

## 8. Mechanism C, expanded — nominal reuse across divergent local shapes

The problem Mechanism C solves: when a dev edits locally, their computation
diverges in *shape* from the generic graph, so shape-identical (subgraph) reuse
misses. The fix is to key reuse by a **stable nominal name**, not by subgraph
shape — and your `IntroId` (deterministic, declaration-local, unchanged by edits
*elsewhere*) already *is* that nominal key (the Adapton/Skip "nominal
memoization" idea, for free).

Concretely, in the LSM + global-graph model:

- **Compute reuse is keyed by the nominal identity of the *inputs consumed*, not
  by the output shape.** A local rebuild of symbol `S` consumes its dependencies'
  IR by `IntroId`. Editing `S` changes `S`'s own nominal id only if its
  declaration path/name/kind changed; it does **not** change the `IntroId`s of the
  dependencies it reads. So the generic graph's memoized derivations for those
  unchanged dependency ids are still hits.
- **Recompute is therefore bounded to the edited subtree.** Only symbols whose
  *inputs* actually changed (the edited symbol + its transitive dependents) fall
  through to local recomputation; everything else is served from the generic base
  layer by nominal-id lookup — the LSM read-merge (§5) and the Salsa-style memo
  (Mechanism A) are the *same* "nominal-id → is my input unchanged? reuse : recompute"
  operation at two granularities (IR node vs derived query).
- **Divergent *shape* doesn't defeat it**, because the match key is the nominal id
  of a symbol, not "this exact subgraph occurred." A locally-refactored function
  that still calls `serde::Deserialize` reuses the generic IR/derivations for
  `serde::Deserialize` regardless of how the caller's shape changed.
- **Renames are the one gap** (the edited symbol's *own* nominal id changed) — and
  that's exactly where §6.4's continuity edge comes in: it re-links for *history/
  display*, but compute reuse doesn't wait on it (the renamed symbol just
  recomputes, which is correct and cheap since it's the one thing that changed).

So Mechanism C = **nominal-id-keyed reuse over the generic graph, with recompute
scoped to exactly the inputs whose nominal identity changed** — no CAS of outputs,
no overlay, no dependence on the (undecidable) continuity question for
correctness.

## 9. How it all composes (the one-layer synthesis)

The pieces are not four systems; they are **four faces of one layer**:

| Concern | Realised by | Backed by |
|---|---|---|
| **Identity** | PURL (Model 4) + `IntroId` nominal key; forks/continuity as provenance-DAG edges (Model 5) | deterministic, §6 |
| **The graph** | one global content-addressed PURL-keyed IR graph, sharded by ecosystem; nodes+adjacency rows (arch B) | §3, §5.4 |
| **Layering** | LSM read-merge: local-accurate memtable shadows generic base by key; tombstones for absent | §5 |
| **Traversal** | Trustfall *above* the shadow-aware KV | §5.4 |
| **The manifest** (small, consistent) | `iroh-docs` namespace, range-reconciled ∝ diff | §7 |
| **The segments** (immutable, cold) | `iroh-blobs` content-addressed blocks, bao-verified ranges | §7 |
| **Durability** | S3 (or any object store) *behind* iroh-blobs; never client-facing | §5.2 |
| **Compute sharing** | nominal-id-keyed memo/arrangement reuse (Mechs A/B/C) over the same manifest+blobs | §8 |
| **Time / history** | per-package libpijul (kept, per §4) — the *only* per-package engine | §4 |

The load-bearing realisation: **the LSM "small mutable manifest + immutable cold
segments" split and the iroh "cheap-to-reconcile docs namespace + fetch-by-hash
blobs" split are the same split.** `iroh-docs` *is* the manifest transport;
`iroh-blobs` *is* the segment transport; S3 is the durability behind the segments;
Trustfall is the traversal; the LSM merge is local-over-generic; the global graph
is the content model. One layer, four faces — and **no overlays, no matcher as
load-bearing machinery, no CAS-of-outputs, no per-package global pristine.**

## 10. Distributed: compile cluster → HA index → readers

**Keystone: content-addressing turns exactly-once into idempotent-retry.** Don't
chase exactly-once compilation — use at-least-once scheduling + content-addressed
idempotent writes. Same `(source@digest, toolchain, config)` → same IR hash → a
redundant compile during a rebalance is a wasted CPU-second and a no-op PUT, not a
correctness bug. This is where the CAS spine earns its keep *most* (the cluster),
not in client dedup.

**Topology:**
- **Work coordination → rendezvous hashing (HRW) over PURL-namespace shards** (=
  the §3 ecosystem shard boundary). Backed by **etcd/K8s Lease** liveness (short
  TTL + fencing epochs) as the membership oracle. A thin coordinator does *only*
  cost-aware rebalancing (some packages are 1000× costlier to compile).
- **Write/index → single-writer-per-shard** (HRW gives exclusive ownership) into a
  small strongly-consistent store (**etcd/DynamoDB**, not FoundationDB), mirroring
  S3 keys with generations; **S3 conditional writes (`If-Match`)** as a
  belt-and-suspenders CAS for rare cross-shard GC/rename.
- **HA → etcd Raft** election; on node loss HRW just recomputes ownership at lease
  expiry. No custom reassignment.
- **Reads → stateless, content-hash-keyed cache (foyer/NVMe — already vendored),
  CDN.** Quickwit/Neon-shaped disaggregation.

**Honest iroh verdict (correcting earlier over-enthusiasm):** for immutable
artifacts to many light cloud clients, **plain S3 + CDN wins by default** (it's
what crates.io/npm run). iroh/willow/bao earn their keep in a *narrower* role:
(1) **client-facing metadata reconciliation** — iroh-docs delta-syncs a *changing
index* cheaply where a CDN would force polling (this win stands); (2) **LAN/CI
locality** (Kraken-style peer fetch); (3) **offline dev**; (4) **client-push**
(teammate surface share). So: **S3+CDN is the primary byte pipe; iroh is the
client-facing metadata layer + a later LAN/offline side-channel; the cluster's
internal coordination is boring etcd+S3.**

**The manifest has two faces** (resolving the §12 table question): **etcd = the
authoritative, strongly-consistent `PURL+IntroId+Resolution → CID` table** the
single-writer shards append to; it is **projected** to a client-facing index —
either a CDN-cached *signed manifest* clients conditional-GET, or an **iroh-docs
namespace** for cheap delta reconciliation. Writers use etcd; readers use the
projection. Derived views ("latest per namespace," "dependents of X" for
Trustfall) are the one place **differential-dataflow/Materialize** (IVM, O(change))
is worth prototyping.

## 11. The resolver + manifest traits (the four-tier hierarchy)

Frame the tiers as a **cache hierarchy** — a miss at Lₙ faults from Lₙ₊₁ and
populates Lₙ:

```
L1  local memory   in-process IrView (hot)
L2  local store    materialized pijul channel + local KV (PristineIntroTable / ReversePositionIndex)
L3  remote memory  the compile cluster's hot index (RPC)
L4  remote store   S3 / IPLD content-addressed change DAG  ← durable source of truth
```

Two seams. `Manifest` is the surviving coverage map / router (§4.1: the *local*
half is a pijul change-set intersection; the *remote* half is the etcd/iroh-docs
`PURL→CID` table):

```rust
pub trait Manifest {
    fn locate(&self, pkg: &PackageLineageId, at: Resolution) -> Tier;
    fn coverage(&self, closure: &[PackageLineageId]) -> Coverage;  // "how much do we have"
}
pub enum Tier { LocalMem, LocalStore, RemoteMem, RemoteStore, Absent }

/// The ONE seam hiding IPLD-vs-pijul and remote-vs-local. Both backends are two
/// VIEWS of one content-addressed change DAG (§4.2), not two storage models.
#[async_trait]
pub trait IrResolver {
    async fn view(&self, pkg: &PackageLineageId, at: Resolution) -> Result<Arc<IrView>>;
    async fn entry(&self, sref: &StableRef, at: Resolution)      -> Result<Option<OwnedEntryPayload>>;
    async fn neighbours(&self, sref: &StableRef, e: EdgeKind)    -> Result<Vec<Residence<StableRef>>>;
}
```

`IpldResolver` (L4: fetch content-addressed blocks, bao-verify, assemble) and
`PijulResolver` (L1–L2: `output` a channel → pristine → project `IrView`) are the
two views; `TieredResolver` composes them per `Manifest::locate`, faulting-in and
caching downward.

**Query engines sit on top, tier-oblivious:**
- **Trustfall** `resolve_neighbors` → `IrResolver::neighbours` returning
  `Residence<StableRef>` handles; Trustfall's laziness *is* the tier fault-in.
- **Qdrant/vector** returns candidate `IntroId`s; hydration = `IrResolver::entry`
  faulting across tiers. The vector *index* keeps its local-edge/remote-cluster
  split; hydration goes through the resolver.
- **Precise results:** every resolved item carries `Sourced` provenance (tier +
  `generic`/`local-accurate`); the planner consults `Manifest::coverage(closure)`
  to promise, per package, local-accurate or explicitly-generic — never silent
  mixing. The **local-first index is L1+L2** — the projections you already build,
  persisted to sanakirja/redb only if cold-open is slow. No tantivy locally.

## 12. Version-to-version reuse (viability)

- **Near-term (do early): emission-layer reuse.** On re-indexing `v2`, compute
  `(skeleton_hash, dep_closure_hash)` per symbol; if it matches `v1`, **copy v1's
  sealed payload, re-key under v2's generation** — skip re-sealing/embedding/
  diffing. Implementable now (IR is content-addressed per symbol); gated by a
  treesitter-skeleton diff. A patch touching 3 functions reuses 997.
- **Later (harder): oracle-compute reuse** = keeping RA's salsa DB warm across
  versions (Mechanism A on the producer). RA isn't built to persist/reload its DB
  across versions cleanly. The emission-layer piece captures most of the IR-plane
  saving without it.

## 13. Decisions

### Resolved (2026-07-22)

- **Client thickness → thick.** The client **embeds and runs libpijul** for the
  packages it materializes locally (workspace, and any cached registry package it
  wants deep history on), so history/blame/diff/semver work **offline**. libpijul
  is the per-package temporal engine wherever a package's IR lives — client *and*
  remote — while the derived global graph (§3) stays the cross-package plane.
  Consequence: the client ships libpijul + sanakirja; the continuity pipeline
  (below) runs at *record* time on whichever node records the generation, so the
  client runs it for local generations.
- **Continuity pipeline → build at v1.** The candidate→verify→never-merge pipeline
  (§6.4) ships from the start; rename/move continuity edges exist in history from
  day one. Consequence: store a **MinHash sketch per symbol** (~100–200 ints)
  alongside the IR for candidate generation; the **APTED verifier** runs on-demand
  at record time on shortlisted pairs only.

### Still open

1. Arch **B vs D** first — is incremental Willow sync needed at v1, or is
   selector-closure fetch over content-addressed blocks enough to start?
2. Shard boundary — PURL `type` only, or `type` + name-prefix from day one for
   the hot ecosystems (cargo/npm)?
3. Cross-ecosystem edges (a Python pkg wrapping a Rust crate) — thin global edge
   index, or IPLD links that cross shards natively?
