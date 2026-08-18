# The Generation Root — one structure for storage, diff, fault-in, and reuse

> **One line.** Stop storing a package's IR as one opaque blob. Store a
> **generation root** — a sorted `Vec<(IntroId, ContentBlake3)>` — plus the entry
> payloads it names. That single structure is simultaneously the storage index,
> the diff basis, the fault-in manifest, and the reuse key, which is why it
> collapses four currently-missing mechanisms into one. Crucially it decouples
> *logical addressing* from *physical layout*, so the layout question that has
> stalled this decision can be deferred behind a measurement instead of guessed.

---

## 0. Why the obvious answers all fail

Four constraints have to hold simultaneously. Every candidate so far satisfies
three and breaks the fourth.

| Constraint | Broken by |
|---|---|
| **C1.** A one-symbol edit must not re-store the package | today's single postcard blob (`ir_stream.rs:184`) |
| **C2.** Object count must not explode | naive per-entry CAS objects: ~1,736+ entries/package (params are first-class entries too) × 227 packages × versions ⇒ millions of objects, at 32 B of hash on a 68–211 B payload |
| **C3.** *Remote* storage must be flat, immutable PUT/GET | `ir-vcs` **as a remote store**: sanakirja mmap'd B-tree + local FS changestore. You cannot mmap an S3 object. (This is not an argument against `ir-vcs` — see §1a.) |
| **C4.** GC must stay tractable | any pack granularity: packs are sealed whole, so reclaiming one dead member means resealing survivors — and there is no GC at all today (`poll.rs:329-372`) |

IPLD is rejected on separate grounds: its value is interop with IPFS/Filecoin
tooling this system will never touch. It owns its transport (iroh), its hash
(raw BLAKE3 `ContentHash`, not a CID), and its codec (postcard, which beats
self-describing dag-cbor at the measured 68–1349 B entry sizes). The one
transferable idea — root node listing hash-addressed children — is **already
hand-rolled here**: `BlobManifest.files: NonEmpty<FileEntry>`
(`blob/mod.rs:49-53`) is exactly that shape. The IR section is the one section
that never got it.

---

## 1a. The split this plan sits inside: `ir-vcs` is the LOCAL store

Before anything else, correct a misreading that this document previously
encoded. "`ir-vcs` cannot deploy because you cannot mmap S3" is true and
irrelevant — **`ir-vcs` was never the remote store.** The deployment vocabulary
says so directly (`heart/deployment.rs`):

```rust
pub enum DeploymentKind {
    /// Multi-compiler, multi-host, HA, enterprise remotes.
    Remote,
    /// One process, one compiler, local catalog clone, offline branches.
    Embedded,
}
...
    /// Root of the local libpijul `IrRepository`.
    pub ir_repo_root: PathBuf,
```

and `TrustedRemote` names the exchange unit:

```rust
    /// This device may `provide` ObjectPacks / IR changes to the remote.
    pub can_provide: bool,
    pub can_fetch: bool,
```

So the intended architecture is:

| Plane | Store | Why it fits |
|---|---|---|
| **Local** (`Embedded`, the GUI machine) | libpijul `IrRepository` at `ir_repo_root` | It has a real POSIX disk. mmap + in-place B-tree is *correct* here. "Offline branches" is the stated requirement, and branches/merges/generation ordering is exactly what a VCS is for. |
| **Remote** (`Remote`, the index) | flat, immutable, content-addressed objects over `object_store` | Must be universal: S3/GCS/Azure/local, many hosts, HA, no shared mutable state. |

Two consequences that reframe the rest of this document:

1. **The local engine's "nothing survives a process restart" problem and
   `IrRepository` being unwired are the same problem.** The corpus is an
   in-memory `BTreeMap` (`store/corpus.rs`) and `PackageView` has no
   `Serialize`, so every launch recompiles the world — while the store designed
   to hold exactly that, at exactly that path, sits unwired. `ir_repo_root` is
   provisioned by `DeploymentProfile::embedded()` and never opened.

2. **`GenerationRoot` is the interchange format between the two planes**, not a
   replacement for either. The remote publishes roots + entry payloads (flat,
   universal, mmap-free); the local node fetches the entries it is missing and
   *applies them as a change* into its `IrRepository`, where it can then progress
   past that base with real VCS semantics.

This is also where `LOCAL-REMOTE-PLAN.md` §6 — "where local progressed past
remote is actually resolved" — gets its answer. It is resolved by libpijul,
locally, because that is what libpijul is for:

| `Residence` | Meaning under this split |
|---|---|
| `Promised` | the remote holds a root; this node lacks the entries |
| `Synced { generation }` | fetched, verified, and applied into the local `IrRepository` |
| `Overlaid` | the local repository has branched past the fetched base — ordered by libpijul's own generation/channel machinery, not by a hand-rolled comparison |

## 1. The structure

The two fields a git tree entry needs — a stable name and a content hash —
**already exist on the type**. `OwnedEntryPayload` (`ir/vcs/wire.rs:536-543`)
carries `payload_hash: ContentBlake3`, and `IntroId` is documented as the
"forever-stable identity of a symbol introduction," derived by domain-separated
BLAKE3 and explicitly stable across versions. Nothing needs inventing. The
structure simply is not being stored.

```rust
/// One package generation's complete symbol table, by identity and content.
///
/// Sorted by `IntroId` so the encoding is deterministic and two roots diff by a
/// linear merge rather than a hash join.
pub struct GenerationRoot {
    pub package: PackageLineageId,
    pub entries: Vec<(IntroId, ContentBlake3)>,
}
```

Size: ~1,736 entries × 48 B ≈ **83 KB**, one object. And because `IntroId`s are
stable, consecutive roots are near-identical — the single best possible input to
zstd `--patch-from` or to any chunk-sharing engine.

That is the whole idea. What follows is why it is worth more than it looks.

---

## 2. Why one structure resolves four separate gaps

The root is not just a storage index. The same bytes answer four questions the
system currently has no mechanism for.

### 2.1 Storage — C1 and C2 together

Entries are addressed by `ContentBlake3`. An unchanged symbol has an unchanged
hash and is **not rewritten**, satisfying C1. And because the root names its
entries by hash rather than inlining them, the *physical* layout of the entry
store becomes a free variable (§4) — which is how C2 stops being a blocker
rather than a trade.

### 2.2 Diff — `ir-vcs`'s semantics without `ir-vcs`'s storage

Two roots, both sorted by `IntroId`, diff by linear merge into exactly the three
cases `ir-vcs`'s `IrOp` already models: present-in-both-same-hash (unchanged),
present-in-both-different-hash (modified), present-in-one (added/removed).

This is the resolution of C3, and per §1a it is a *boundary*, not a rejection.
`ir-vcs`'s identity model (ground-truth `IntroId`, not rolling-hash rediscovery)
is right on **both** planes; only its on-disk representation is local-only.
`diff_tables`/`PackageDelta` (`ir/vcs/diff/`) is the shared algorithm — it runs
over two `GenerationRoot`s on the remote, and over channel tips inside
`IrRepository` on the local side. Sanakirja stays behind `ir_repo_root` and never
enters the remote deployment.

The payoff of using the *same* diff on both sides: a root-diff computed on the
remote and a change computed locally describe the same three cases, so a fetched
delta can be applied into the local repository without translation.

### 2.3 Fault-in — the missing `locality()` from LOCAL-REMOTE-PLAN §3.3

`LOCAL-REMOTE-PLAN.md` names Pattern ② — "a store with a *locality query*, never
binary hit/miss" — as the one genuinely missing piece, because `ContentIo::has()`
returns `bool`.

A root makes locality computable exactly, with no new machinery: a node holding
any older root diffs it against the wanted root and gets precisely the set of
missing `ContentBlake3`s. That is Bazel RE's `FindMissingBlobs` and git's
promisor walk, for free, as a consequence of the structure rather than as a
feature to build.

### 2.4 Reuse — partial, which is the part that matters

"Do I have this package's IR?" becomes "do I have this root hash?" — but more
importantly, reuse stops being all-or-nothing. A node holding v1 can serve v2 by
fetching **only the 8 entries that changed**, rather than choosing between a full
recompile and a full download. That is the difference between a cache that helps
on exact hits and one that helps on every version bump.

This is also what makes `Residence::Synced { generation }` (already built in
`heart::surface`) honest: an entry either is or is not materialized here, per
entry, and the root says which.

---

## 3. What this does *not* fix

Stated plainly, because the value of the plan depends on not overclaiming it.

- **It does not create a reader.** Nothing decodes the IR section today
  (`save/mod.rs:127` and `save/blobs.rs:177` fetch it and discard the bytes).
  This is a write-amplification fix, not a serving feature. Sequence accordingly.
- **It does not fix the empty-on-macOS problem.** `compile_inprocess.rs:229`
  writes an empty IR section unconditionally because there is no forward
  semantic→wire encoder. §5 addresses that separately, and the answer is *not*
  to write the encoder.
- **It does not deliver GC.** It makes GC *easier* than any pack granularity
  (storage unit stays == GC unit if the flat layout is kept — a dead entry is a
  single-key delete, no reseal), but the mark-sweep still has to be built.
- **It is not free at the small end.** A 32 B hash against a 68 B minimal entry
  is 47% overhead. §4 is how that stops mattering.

---

## 3a. MEASURED (P1) — what the numbers actually say

Both gating measurements are done. Real producers (`nudox_languages::produce`)
over the real corpus, diffed by `IntroId` across 14 adjacent-version pairs in 4
ecosystems.

### 3a.1 A position-sensitive content hash would violate C1 by construction

**This is the most important finding in the plan and it is a live defect.**

`entry_content_hash` (`ir/model/src/content/mod.rs:366-370`) encodes
`sym.source` and `sym.span.start`/`.end` — the declaration's **byte offsets in
its file**. Editing anything earlier in a file shifts every later declaration's
span, so this hash reports "modified" for byte-identical code.

Measured effect on one pair (testify v1.9.0 → v1.11.1):

| hash | reported "modified" |
|---|---|
| `entry_content_hash` as-is | **75.6%** |
| same, with `source`/`span` excluded | **1.3%** |

A 58× inflation of apparent churn, entirely from code moving rather than
changing. This matters because `ir/model/src/manifest/mod.rs:384-386` already
builds prototype `(IntroId, ContentBlake3)` pairs using exactly this hash — the
shape `GenerationRoot` wants. **Reusing it unchanged would make a real
deployment look catastrophically worse than it is, for reasons having nothing to
do with real edits.**

Note this is not necessarily a bug *in* `entry_content_hash`: including position
is defensible for "did anything about this entry change, including where it is"
(its own comment at `:852` says "the source changed", which is true). It is
simply the wrong hash for a *storage identity*. Two questions, two hashes — the
same split `BlobManifest` already draws between Hash① and Hash②
(`index/blob/mod.rs:67-105`). `GenerationRoot` needs a **position-independent**
entry hash, and that is a prerequisite, not a follow-up.

### 3a.2 Churn is bimodal — revise the headline number down

Per-pair new-storage share (`(modified + added) / union`), semantic pass:

| pair | bump | new storage |
|---|---|---|
| memchr 2.8.0→2.8.3 | patch | 0.1% |
| lodash 4.17.20→4.17.21 | patch | 0.4% |
| memchr 2.7.6→2.8.0 | minor | 0.7% |
| syn 2.0.119→3.0.3 | major | 5.0% |
| testify v1.9.0→v1.11.1 | minor | 5.4% |
| click 8.0.4→8.1.7 | minor | 8.0% |
| zod 3.22.4→3.23.8 | minor | 11.0% |
| hashbrown 0.16.1→0.17.1 | minor | 11.3% |
| serde_derive → 1.0.229 | patch | 19.2% |
| pkg/errors v0.8.1→v0.9.1 | minor | 20.7% |
| syn 1.0.109→2.0.119 | major | 20.8% |
| serde 1.0.196→1.0.229 | patch | 21.6% |
| log 0.4.17→0.4.33 | patch | 35.2% |
| pydantic 1.10.14→2.6.1 | major | 72.0% |

**Median 11.2%, mean 16.5%, range 0.05%–72%.** Genuinely bimodal: mature stable
APIs on patch bumps sit under 1%; rewrites spike past 70% (pydantic v1→v2 is a
known ground-up rewrite; serde's 77% "removed" is the real `serde_core`
extraction, hiding inside a *semver-patch* bump).

So **the "~15×" is real but is a best case**, not a planning number — it is
`ir-vcs`'s own benchmark output at its assumed Δ=2%, a rate only the mature-patch
cluster actually hits. Expect **~5–9× on typical releases and near-1× on majors
and rewrites**. Still worth having, since P0–P2 cost nothing extra to obtain it,
but any capacity planning should use the median, and "15×" should not appear
unqualified anywhere.

Caveat stated plainly: "adjacent" means adjacent among the versions pinned in
`nix/corpus.nix`, not consecutive upstream releases — serde's pair spans ~33
point releases, log's ~16. These are cumulative multi-release drift, so **real
per-release churn is likely lower** than the table. C#/Java pairs were not run.

### 3a.3 DoltLite does not give free dedup — use flat CAS

§4C's hypothesis is **refuted**. Prototype: 8,000 rows, 12 generations of 2%
mutation, driven through `rusqdoltlite` directly.

- ~670 KB per generation to mutate 160 rows (~55 KB raw changed) — **~12×
  amplification**, because scattered edits dirty far more prolly-tree structure
  than the rows themselves.
- `dolt_gc()` after 12 linear commits reclaimed **0 bytes**. Every commit stays
  reachable from HEAD, like git — so committing per generation would run a
  **second full version-control history underneath `ir-vcs`'s own**.
- It beats whole-blob-per-generation by ~4.1×, which is real but modest, and it
  is strictly worse than flat CAS on the number that matters: bytes per changed
  entry.

The falling marginal cost in `storage_catalog_scaling.rs` (3,830 → 844 B/package)
is about *adding new structurally-similar packages to a growing catalog* — a
different property entirely from *repeatedly editing existing rows*.

**Decision: layout A (flat `cas/{hash}`).** §4's "measure C first" is resolved;
C is out.

---

## 4. The layout question, deferred behind a measurement

This is the part that unblocks the decision. Because the root addresses entries
by `ContentBlake3`, **the physical layout is swappable without touching the
logical model.** Three candidates, in ascending order of effort:

| Layout | Object count | Effort | When it wins |
|---|---|---|---|
| **A. Flat `cas/{hash}`** | one per distinct entry | none — works today | small corpora; local FS; the honest default |
| **B. Per-generation pack of *changed* entries** | one per generation | needs a `ContentBlake3 → (pack, offset)` locator | object count actually bites |
| **C. DoltLite rows keyed `(lineage, intro_id)`** | zero new objects | schema work | if the prolly tree's own chunk sharing beats all of this |

**Measure C first.** `storage_catalog_scaling.rs` already measures DoltLite's
per-package marginal cost *falling* — 3,830 → 844 B/package as the catalog grows
— because a prolly tree is a content-addressed, history-independent B-tree whose
structural sharing between versions is automatic. If storing entry payloads as
rows gets cross-version sharing from the engine, it beats every bespoke Merkle
scheme here on effort, and it needs no locator, no pack management, and no GC
redesign. This is currently unverified and is the single highest-value
measurement available.

Start on **A** regardless. It is the layout that exists, it satisfies C1 and C4
immediately, and switching to B or C later changes no logical structure and no
consumer — which is the whole point of separating the two.

---

## 4a. MEASURED — the wire format cannot be the storage format

P5 wired `IrRepository` as the local store by routing IR through a new
semantic→wire encoder (`ir/vcs/raise.rs`). It works, and `local_persistence.rs`
passes 4/4 with real producer runs. It is also **lossy in a user-visible way**,
which that spec could not see because it asserted only `symbol_count > 0`.

`tests/persistence_fidelity.rs` compares *rendered signatures* instead — the
declaration the GUI actually paints — and fails 3/3:

| declaration | produced | after restart |
|---|---|---|
| `fn base_method(&self) -> u32` | `-> ty(u32,-)` | `-> id(?)` |
| `fn generic_fn<T: Derived>(input: T) -> impl Base` | `(input: T) -> impl Base` | `(_) -> ?` |
| `type BoundedAlias<T: Base> = Option<T>` | `= Option<T>` | `= any` |

**Every function loses its return type; every parameter loses its name and
type.** `symbol_count` is 22 before and after — which is exactly why a
count-based test passes while this happens.

What *does* survive, stated precisely so the fix is not over-scoped: entry count,
names, kinds, visibility, and trait-link targets. And the two `impl`s for
different nominal self-types did **not** collide, despite `raise.rs` warning they
could. The damage is **type erasure** via `TypeWire::Any`, not entry loss.

### The rule this establishes

`ir_vcs::wire` exists for libpijul's **patch** layer. Promoting it to a
**storage** layer imports its expressiveness ceiling — a ceiling that is
invisible until a user restarts the app and every signature says `?`.

`IrRepository` already stores per-symbol files, so its versioning machinery does
not depend on the payload *encoding*. The payload should be a lossless
serialization of the semantic `Entry` (what `IrSnapshot` already does), keeping
per-symbol byte diffing — unchanged symbol ⇒ unchanged bytes ⇒ no rewrite —
without the ceiling.

**And the standing bar, for every reuse mechanism in this plan: reuse must be
indistinguishable from recomputation.** A store that cannot hold a symbol
losslessly must refuse it loudly rather than serve a degraded copy. Silent
degradation is worse than recomputing, because the user cannot tell it happened.

---

## 5. The macOS empty-IR problem, resolved by not solving it

`compile_inprocess.rs` writes an empty IR section because there is no forward
`Entry → OwnedEntryPayload` encoder (`lower.rs` is wire→semantic only; verified,
zero functions go the other way).

**Do not write that encoder.** It is only needed because the storage format *is*
the wire format. The compile phase already holds the semantic sealed table; it
can hash and store semantic `Entry` values directly. `IrSnapshot`
(`nudox-engine/src/store/remote.rs`) already proves the shape works —
`Vec<(IntroId, Entry, Option<IntroId>)>`, semantic entries with parent edges,
round-tripping through a content-addressed store.

So `GenerationRoot`'s entries name hashes of **semantic `Entry`** payloads, not
wire payloads. The missing encoder dissolves, both platforms produce real IR, and
the consumer (`PackageView`, which needs `Entry` anyway) stops needing a
per-entry lowering step it currently cannot perform.

The `Option<IntroId>` parent edge from `IrSnapshot` must be carried — `lower.rs:71`
notes the wire format does not encode parent edges inline, and a consumer cannot
rebuild the tree without them. Either widen the root's tuple or make the parent
part of the hashed payload; §7 leaves this open deliberately.

---

## 6. Phases

Ordered so each lands independently and nothing is a wire break until P3.

| Phase | Deliverable | Break? | Risk |
|---|---|---|---|
| **P0** | Parallelise `blob::emit` (serial `for` over `put_section`, which is itself HEAD-then-PUT). Corpus p90 = 535 files ⇒ 10–25 s of serial latency per package on an S3-class backend. | none | none — no format, no addressing, no bytes change |
| **P1** | Measure: DoltLite row-storage sharing (§4C); real per-entry payload sizes end-to-end; **and the actual churn rate** (§7.1). Prototype only. | none | none |
| **P2** | `GenerationRoot` type + encoder/decoder + the linear-merge diff, in `heart` or `ir`. Written and tested against fixtures; nothing emits it yet. | none | none — pure addition |
| **P3** | Emit path writes semantic `Entry` payloads + a `GenerationRoot` **alongside** today's `ir_ref`. Both platforms produce real IR (§5). Dual-write. | additive | low — old readers untouched |
| **P4** | Readers move to the root. `verify_reproducible`/`verify_blobs` audit per-entry. Retire `ir_ref`. Backfill is optional since P3 dual-wrote. | **format break** | medium |
| **P5** | **Wire `IrRepository` as the local store** (§1a): open it at `ir_repo_root`, record each produced generation as a change, materialize the corpus from it at startup. This alone ends "every launch recompiles the world" — independent of anything remote. | none | medium |
| **P6** | Fault-in: `locality()` over root-diff (§2.3) → fetch missing entries → apply as a change into the local `IrRepository`. `Residence::Synced`. This is where S6 actually lands, and it needs P5 to have somewhere to put the result. | none | medium |
| **P7** | Push: local-only generations `provide`d back to a `TrustedRemote` (`can_provide`), closing the loop the deployment vocabulary already describes. `Overlaid` becomes orderable. | none | medium |
| **P8** | GC: root-set mark-sweep. Roots = live `ptr/` → generation roots → entry hashes. Only after P4, when there is one storage shape rather than two. | none | high — destructive |

P0 is running now and is independent of everything else. P1 gates P2's design.

---

## 7. Open questions — decide before P2

1. **What is the real churn rate?** The Δ=8-of-400 (2%) figure driving the ~15×
   estimate is `ir-vcs`'s *own benchmark's assumption*, not a production
   measurement. If IR is re-emitted wholesale per release, there is no unchanged
   remainder to credit and this plan's premise weakens sharply. **Measure before
   building.**
2. **Are function parameters their own entries in the root?** `KindWire::Param`
   makes them first-class with their own `IntroId`. Inlining them into the owning
   function cuts entry count substantially and raises mean payload size — both
   good for C2 — at the cost of losing per-parameter sharing. Probably inline;
   needs a decision.
3. **Where do parent edges live** — a third tuple element, or inside the hashed
   payload? (§5.) Affects whether a parent-only change rewrites the child.
4. **Does `identity_bytes()` survive?** `BlobManifest`'s two-hash scheme
   (`blob/mod.rs:67-105`) was designed for one opaque `ir_ref`. With a root, "has
   this package changed" is just "is this the same root hash" — the Hash①/Hash②
   split may collapse.
5. **GC mechanism** — refcount side table transactional with the pointer write,
   or snapshot-fenced mark-sweep? `poll.rs:356-359` names both and picks neither.
   Decide before P4, so P6 is designed for the right shape rather than retrofitted.

---

## 8. One-line architecture

**Two stores, one interchange format: the remote keeps IR as flat
content-addressed objects named by a `GenerationRoot` (universal, mmap-free,
S3-shaped); the local node keeps it in libpijul (`ir_repo_root`), where offline
branches and local-ahead-of-remote are native rather than emulated; and the root
— a sorted list of (stable identity, content hash) — is what crosses between
them, so incremental storage, structural diff, exact locality, and partial reuse
all fall out of one structure instead of four mechanisms.**
