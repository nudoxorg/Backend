# 07 — Materializer + SOCI-like Archive Index

**Research date:** 2026-07-16  
**Status:** Design ADR + implementable plan (ADOPT CasHardlink + LazyArchive MVP)  
**Kind:** edge-tech streamlining → concrete storage/materialize subsystem  
**Scope:** Eden-class lazy materialization benefits **without EdenFS** — external archive indexes (SOCI/eStargz-shaped), CAS hardlink views, producer integration, INDEX/REGISTRY/ORCH wiring, security, crate placement, PR-sized steps.  
**Audience:** storage, compiler-daemon, registry/client-sync, orch assemblers.  
**Non-goals:** FUSE desktop daemon; EdenFS; tree-sitter Tree persistence (GD-11); public IPFS; shared mutable language build caches (06 L2).

**Cross-refs (local):**
- [13-storage](../../librarification/13-storage/PLAN.md) — per-file BLAKE3 CAS SoR; optional transfer packs; docs.rs ZIP+range; Nix narinfo
- [03-edenfs](../03-edenfs/PLAN.md) — **REJECT EdenFS**; steal lazy hydrate, redirections, hardlink views
- [05-surrounding-edge](../05-surrounding-edge/PLAN.md) — SOCI/eStargz highest-ROI lazy materialize; composefs Phase-2
- [06-compiler-fleet-cache](../06-compiler-fleet-cache/PLAN.md) — L5 source hardlink CAS; L0/L1 relationship
- [00-DECISIONS](../00-DECISIONS.md) — locked materialization stack preference order
- [14-client-sync](../../librarification/14-client-sync/PLAN.md) — want/have; BlobTransport
- [12-orchestration](../../librarification/12-orchestration/PLAN.md) — sealed RO trees + scratch
- [22-crate-topology](../../librarification/22-crate-topology/PLAN.md) — blob / materialize placement

**Live code anchors (2026-07-16):**
- `workspace/compiler/generate/source_archive.rs` — `SourceArchive` / `FileDigest` (path + BLAKE3 + size)
- `workspace/compiler/generate/mod.rs` — `PackageInput { root: PathBuf }`, `generate_with`
- `workspace/compiler/bin/compiler_daemon.rs` — **full write of request.files into tempdir** (no lazy path)
- `workspace/compiler/compile/isolate.rs` — `seal` / RO binds / scratch
- `workspace/registry/blob/mod.rs` — `BlobManifest.files: NonEmpty<FileEntry>`
- `workspace/registry/store.rs` — `cas/{hex}`, `get_section_range`
- `workspace/heart/cache/disk.rs` — local CAS envelope + hardlink publish
- `workspace/compiler/daemon/forge.rs` — optional `NUDOX_CAS_ROOT`; no materializer trait yet

---

## 0. Executive summary (read this first)

### 0.1 One-paragraph answer

nudox already stores source as **per-file BLAKE3 CAS** (plan 13) and needs **real directory trees** for producers (`PackageInput.root: PathBuf`). Today the daemon **writes every file into a tempdir** on every compile. That is correct-but-wasteful: desktop REGISTRY pays full extract for “open package,” and k8s pods re-extract popular packages despite shared node disk. We adopt a **`Materializer` trait** with two MVP backends — **`CasHardlinkMaterializer`** (when file hashes are already in local `cas/`) and **`LazyArchiveMaterializer`** (when a generation **pack + SOCI-like external index** is present) — plus **`FullExtractMaterializer`** as fallback when touch ratio or missing pack makes laziness lose. No FUSE, no EdenFS. composefs/EROFS is Phase-2 Linux only. System of record remains **per-file CAS**; packs/indexes are **transfer/lazy optimizations**, never the only replica of source bytes.

### 0.2 Decision (locked)

| Item | Verdict |
|---|---|
| **CasHardlink + LazyArchive** as MVP pair | **ADOPT** |
| **FullExtract** fallback | **ADOPT** |
| **composefs / EROFS** | **PHASE-2** Linux orch pods only |
| **EdenFS / production FUSE desktop** | **REJECT** (03, 00-DECISIONS) |
| **Per-file CAS as SoR** | **UNCHANGED** (13) |
| **Pack + index as optional transfer artifacts** | **ADOPT** |
| **Tree-sitter Tree persistence** | **REJECT** (GD-11 / 13 §3) |

### 0.3 Preference order (matches 00-DECISIONS §5)

1. CAS objects + generation manifest (always)  
2. Hardlink / copy-on-read views and compile tmpdir trees  
3. SOCI-like external archive indexes (highest-ROI lazy path)  
4. composefs/EROFS on Linux orch (later)  
5. App-level virtual tree in GUI (no kernel FS)  
6. ~~EdenFS~~ out  

### 0.4 Success metrics (normative)

| Metric | Target (MVP) | Notes |
|---|---|---|
| **Cold open p95** (desktop, popular crate, empty local pack cache, index present) | ≤ first-file interactive + progressive hydrate; **p95 time-to-first-source-file ≤ 200 ms** LAN / ≤ 800 ms WAN class | Not full tree |
| **Cold open p95** (all files needed for RA rustc-class producer) | ≤ FullExtract baseline × 1.2 | Laziness must not regress full-touch producers badly |
| **Bytes hydrated / package size** (GUI browse one file) | ≤ **0.05** typical; ≤ **0.15** p95 | Index + one member + parents |
| **Bytes hydrated / package size** (compile, hardlink hit) | **0** network; disk = hardlink metadata only | L5 hit |
| **Disk for N versions high-overlap** | Match plan 13 CAS model (~0.85 + 0.15N)×S | Views must not copy full trees |
| **Hash verify fail rate** | 0 tolerated in prod; any fail = quarantine path + metric | Integrity gate |

### 0.5 What ships in MVP vs later

| Capability | MVP | Phase-2 |
|---|---|---|
| `Materializer` trait + `MaterializeSpec` / `MaterializedTree` | Yes | — |
| CasHardlink views | Yes | — |
| ArchiveIndex (`.ndix`) + transfer pack (`.ndpk`) formats | Yes | — |
| Lazy range hydrate local seek | Yes | — |
| Lazy HTTP range GET via presign | Yes (client + INDEX) | — |
| FullExtract fallback + touch-ratio policy | Yes | — |
| Producer `ForgeContext` injection | Yes | — |
| Metrics (hydrate bytes, open latency) | Yes | — |
| Index generation at produce time | Yes | Post-hoc rebuild tool |
| composefs mount backend | No | Yes Linux |
| FUSE virtual tree | No | Maybe never |
| Seekable solid-zstd alternative to frame-per-file | Optional | Tune |

---

## 1. Problem & goals

### 1.1 The tension

| Force | Pulls toward |
|---|---|
| Compilers/oracles need **real paths** | Full directory trees on disk |
| CAS stores **files**, not trees | Materialize is a *view*, not storage |
| Desktop disk + multi-version retention | Dedupe + lazy open |
| Untrusted k8s seal | RO tree + separate scratch (Eden **redirection** analogue) |
| Cold network | Few large range-GETs or many small CAS GETs |
| Cross-version identity | BLAKE3 per file (already) |

Plan 13 resolved **storage**: per-file CAS SoR + optional transfer packs.  
Edge-tech 03 resolved **not EdenFS**.  
Edge-tech 05 ranked **SOCI/eStargz external indexes** as highest-ROI lazy materialization.  
**This plan resolves the missing subsystem:** who builds trees, when, from what, with which integrity rules.

### 1.2 Concrete user/system stories

1. **Desktop REGISTRY — open package in GUI**  
   User opens `serde@1.0.210`. Catalog already has manifest. Local `cas/` has some files from older versions. GUI needs `src/lib.rs` first. Must not download/extract the whole crate.

2. **Desktop — open package for trusted local project**  
   Project is already on disk. Prefer hardlink or direct path view; no pack.

3. **k8s compiler pod — first compile of popular crate on warm node**  
   Node L5 source CAS has 80% of file hashes from prior versions. Materializer hardlinks hits; range-fetches misses into CAS; builds job-private RO view.

4. **k8s — cold node, pack present**  
   Prefer single pack stream or ranged members vs N tiny S3 GETs (small-file tax).

5. **Producer that walks entire tree** (`source_archive`, CST walk)  
   Touch ratio ≈ 1.0 → FullExtract or proactive prefetch of entire generation is correct; lazy must not thrash.

6. **Untrusted sealed job**  
   View is RO bind; writes go to scratch outside package root (03 redirections lesson).

### 1.3 Goals (ranked)

1. **Integrity** — every hydrated file verified to BLAKE3 in index/manifest before publish into view.  
2. **Lazy open** multi-version sources without full extract when working set is small.  
3. **Disk savings** — views are hardlinks into global `cas/`, not copies.  
4. **Producer compatibility** — real `PathBuf` trees; no producer API rewrite to virtual FS.  
5. **Fleet L5** — node-shared RO CAS of source bodies (06).  
6. **Observable** — cold open p95, bytes hydrated / package size, hydrate error rate.  
7. **No FUSE required for MVP.**

### 1.4 Non-goals

- Replace per-file CAS with archive-only storage.  
- Persist tree-sitter Trees (GD-11).  
- EdenFS, ProjFS, macFUSE product dependency.  
- Chunk-level FastCDC day one (13 §4.3).  
- Shared mutable `target/` across tenants (06 L2 reject).  
- Perfect single-file latency under adversarial WAN without CDN (CDN is ops, not format).

### 1.5 Live gap analysis

| Path | Today | Gap |
|---|---|---|
| Daemon compile | Write all `request.files` bytes to tempdir | No CAS reuse; no pack; no hardlink |
| `source_archive::build` | Walks full tree, hashes each file | Assumes full materialize already happened |
| REGISTRY `Store` | Per-hash get + range | No tree view builder |
| Seal / isolate | RO binds of `cwd`/mounts | Needs materializer to supply RO root |
| Fleet L5 (06) | Designed, not landed | This plan is L5’s implementation surface |
| Transfer packs (13) | Spec sketch only | Need concrete `.ndpk`/`.ndix` + materializer consumer |

---

## 2. Prior art (concrete)

### 2.1 SOCI (AWS soci-snapshotter)

**What it is:** Lazy loading for **existing** OCI image layers without recompressing them. An **external** index (zTOC) is attached via OCI **referrers**; the original layer blob stays valid.

**zTOC roughly contains:**
- Per-file (tar member) metadata: name, offset, compressed size, uncompressed size, type, mode
- Compression checkpoints / span descriptors so a range GET can start mid-stream for gzip-ish layers
- Enough structure for a FUSE/snapshotter to serve `open`/`read` by fetching only needed byte ranges

**Range-GET semantics:**
```
Client knows: layer_digest, file path
→ lookup zTOC → (offset, length)
→ HTTP GET layer with Range: bytes=offset-(offset+length-1)
→ decompress span → present file
```

**Lesson for nudox:** Prefer **external index** that does **not** rewrite the pack format after the fact (SOCI > eStargz for “already published” artifacts). Generate index at produce time when cheap; support post-hoc index build for old generations.

**URLs:** https://github.com/awslabs/soci-snapshotter · production on EKS/Fargate · 2026 comparisons (Grab et al. via 05).

### 2.2 eStargz / stargz-snapshotter

**What it is:** Recompress OCI layers so **each file is an independent gzip member** (or stargz entry) with a **TOC embedded** (or at a known location) in the layer. Still a valid tar.gz-class object for many tooling paths.

**Difference from SOCI:**
| | eStargz | SOCI |
|---|---|---|
| Layer rewrite | Yes (recompress) | No |
| Index location | Inside/alongside layer format | External referrer |
| Compatibility with “dumb” pullers | Often still works as tar.gz | Original layer always works |
| Best when | You control build pipeline | You index existing artifacts |

**Lesson:** For **new** nudox packs we control the writer → we can choose frame-per-file (eStargz-like) **or** external index over a simple concat format (SOCI-like). **MVP: external `.ndix` over `.ndpk`** (SOCI-shaped), with pack layout designed for seekability from day one (so we get eStargz benefits without OCI machinery).

### 2.3 docs.rs archive + range

From plan 13 §1:
- Migrated from per-HTML S3 objects to **one archive per crate-version** for PUT/cost reasons.
- **ZIP** preferred over tar.gz for random member access (central directory).
- Optional **archive index of member offsets** + S3 Range for single-page fetch.
- Catalog stays in DB.

**Critical difference for nudox:** docs.rs HTML is **not** cross-version shared. Our **source is**. Therefore:
- **Primary storage remains per-file CAS** (dedupe).
- **Archive pack is cold-transfer / lazy-hydrate optimization**, not SoR.
- Index can point at **pack offsets** *and* **content hashes** so hydrate always lands in CAS.

### 2.4 Nix store paths / narinfo

| Nix | nudox |
|---|---|
| `.narinfo` (metadata + NAR hash + refs + sig) | Generation manifest / `BlobManifest` + optional pack pointer |
| `.nar.xz` payload | Optional `.ndpk` transfer pack |
| `/nix/store/<hash>-name` full unpack | MaterializedTree view (hardlink or extract) |
| Substituters | INDEX presign + CDN |

Nix typically **fully unpacks** a store path; laziness is at **path** grain, not **file within path**. We need **finer** laziness for large packages when only a few files are touched — hence archive index.

### 2.5 composefs / EROFS (Phase-2 mount)

composefs = EROFS metadata image + content-addressed objects + optional overlay upper; fs-verity friendly; RO mounts without userspace FUSE daemon for the data path.

| Property | Fit |
|---|---|
| Linux k8s RO package tree | Excellent |
| macOS desktop | Weak / unavailable first-class |
| Same object naming as `cas/{blake3}` | Design goal — share objects |
| MVP | **No** — API slot only (`ComposefsMaterializer`) |

### 2.6 How this differs from EdenFS / git sparse

| Dimension | EdenFS | git sparse/partial | **nudox Materializer** |
|---|---|---|---|
| Keying | SCM commit + repo | git OID | **generation + BLAKE3 file hashes** |
| Kernel FS | FUSE/NFS/ProjFS | real checkout | **real dirs** (hardlink/extract); no FUSE MVP |
| Daemon | Required | git only | **Library in-process** |
| Mutability | Overlay WC | WC | **Immutable generations**; scratch separate |
| Multi-version registry | Poor fit | many clones | **Native** |
| Hash without read | thrift SHA-1 | git cat-file | **Manifest already has BLAKE3** |
| OSS product readiness | Unsupported | excellent | **We own the code** |

**Stolen Eden ideas (03) only:**
- Lazy hydrate on demand  
- Content-hash before/without full read (we already have digests)  
- **Redirections** → scratch outside sealed tree  
- Deferred materialization (Buck2 cousin) → ensure_file / ensure_tree policies  
- Shared object cache across checkouts → global `cas/`

### 2.7 Prior art mapping table (normative)

| Prior art piece | Steal | Reject |
|---|---|---|
| SOCI external zTOC | External index next to pack; range GET | containerd snapshotter on laptop |
| eStargz frame-per-file | Seekable pack layout | Mandatory recompress of historical CAS |
| docs.rs ZIP+range | Index + Range pattern | Archive as only storage for source |
| Nix narinfo | Manifest + optional pack; verify hash | Full-NAR-only access model |
| composefs | Phase-2 RO mount | Cross-platform MVP dependency |
| EdenFS | Lazy + redirections mental model | Product dependency |
| git sparse | Pathset prefetch policy idea | git as package SoR |
| Buck2 deferred materialize | ensure-on-demand for producers | RE service requirement |

---

## 3. Core types (Rust sketches)

> Sketches are **implementable shapes**, not final crate paths. Prefer `ContentHash` from `heart`, paths relative as `PathBuf`/`SmolStr` consistent with `FileEntry`.

### 3.1 Archive index

```rust
/// External table-of-contents for a generation transfer pack (SOCI-shaped).
/// CAS key of this blob is blake3(raw index bytes) OR path-addressed by generation
/// (see §4); prefer content-addressed index object + pointer from generation row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveIndex {
    pub format_version: u16,          // = 1
    pub package_gen: ContentHash,     // generation_hash (Hash ①) this index covers
    pub pack_hash: ContentHash,       // blake3 of entire .ndpk file (integrity of pack)
    pub pack_codec: PackCodec,        // NdPkV1 frame-per-file zstd | ZipStore | …
    pub file_count: u32,
    /// Sorted by path for deterministic encoding / binary search.
    pub files: BTreeMap<PathBuf, FileEntryExt>,
    /// Optional: total uncompressed package bytes (metrics / policy).
    pub total_uncompressed: u64,
    /// Optional: blake3 of canonical path→hash list (must match BlobManifest files).
    pub files_manifest_fingerprint: ContentHash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntryExt {
    pub content_hash: ContentHash,    // SoR key into cas/
    pub uncompressed_size: u64,
    pub mode: Option<u32>,            // unix mode bits if we care; default 0o644 file
    pub kind: EntryKind,              // File | Symlink(target) — MVP: File only; reject escaping links
    /// Present when this file is findable inside the pack.
    pub pack_locator: Option<PackLocator>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    // Symlink { target: PathBuf }, // Phase-1.5 if needed; path-jail target
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackLocator {
    pub compressed_offset: u64,       // byte offset in .ndpk
    pub compressed_length: u64,       // exact range length for HTTP Range
    pub frame_codec: FrameCodec,      // ZstdFrame | Raw | ZipDeflate …
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PackCodec {
    /// Concat of independent frames; index has per-file offsets. MVP default.
    NdPkV1,
    /// ZIP with stored or deflated members; index may duplicate central directory.
    ZipV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum FrameCodec {
    Raw,
    Zstd,
    // ZipDeflate,
}
```

**Relation to `FileEntry` / `FileDigest`:**
```text
FileDigest / FileEntry  ⊂  FileEntryExt
  path, hash, size          + pack_locator, mode, kind
```

`ArchiveIndex.files` must be a **superset projection** of `BlobManifest.files` / `SourceArchive.files` for the same generation: same path→content_hash, same sizes. Divergence is a **hard error** at index validate.

### 3.2 Materializer trait

```rust
#[async_trait]
pub trait Materializer: Send + Sync {
    /// Ensure a usable tree for `spec`. May be sparse (lazy) or full.
    async fn ensure_tree(&self, spec: &MaterializeSpec) -> Result<MaterializedTree, MaterializeError>;

    /// Hydrate one file into CAS (if needed) and return a real filesystem path
    /// under the active view (or an absolute path into cas/ for trusted callers).
    async fn ensure_file(
        &self,
        spec: &MaterializeSpec,
        path: &Path,
    ) -> Result<PathBuf, MaterializeError>;

    /// Open a view handle (hardlink farm, archive-backed dir, temp extract).
    /// Drop/close policy is implementation-defined via `MaterializedTree::pin`.
    async fn open_view(&self, spec: &MaterializeSpec) -> Result<ViewHandle, MaterializeError>;

    /// Prefetch a pathset (optional optimization for known producer inputs).
    async fn prefetch(
        &self,
        spec: &MaterializeSpec,
        paths: &[PathBuf],
    ) -> Result<(), MaterializeError> {
        for p in paths {
            let _ = self.ensure_file(spec, p).await?;
        }
        Ok(())
    }
}

pub struct ViewHandle {
    pub tree: MaterializedTree,
    // optional: watch / metrics id
}
```

### 3.3 Spec & result types

```rust
#[derive(Debug, Clone)]
pub struct MaterializeSpec {
    pub generation: ContentHash,           // Hash ①
    pub package: Option<PackageId>,        // for logging / ACL
    /// Catalog truth: path → content_hash (+ size). From BlobManifest or SourceArchive.
    pub files: Arc<ManifestFiles>,         // thin wrapper over BTreeMap/NonEmpty
    pub prefer: MaterializePrefer,
    pub trust: TrustTier,
    pub view_root_parent: PathBuf,         // where to create view dirs
    pub pin: PinPolicy,
    /// Optional pack + index locations (local paths or remote URLs/presigns).
    pub pack: Option<PackRef>,
    pub index: Option<IndexRef>,
    /// Remote CAS getter (presigned or BlobTransport).
    pub remote: Option<Arc<dyn CasFetcher>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializePrefer {
    /// All file hashes expected local; build hardlink farm.
    CasHardlink,
    /// Use pack+index; hydrate on demand into cas/ then hardlink.
    LazyArchive,
    /// Extract everything to tmpdir (or hardlink all if present).
    FullExtract,
    /// Implementation chooses using local presence + pack availability + policy.
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustTier {
    /// Local project paths may be used as-is; hardlinks OK; user-controlled.
    TrustedLocal,
    /// Sealed registry generation; RO view; no follow external paths.
    UntrustedSealed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinPolicy {
    /// Delete view on drop (job scratch).
    Ephemeral,
    /// Keep view until explicit release / LRU (desktop open package).
    Session,
    /// Keep for active project pin (pin_level=2).
    Project,
}

#[derive(Debug, Clone)]
pub struct MaterializedTree {
    pub root: PathBuf,
    pub kind: TreeKind,
    pub generation: ContentHash,
    pub pin: PinPolicy,
    /// Paths known present under root (for sparse trees). None = "treat as full".
    pub materialized_paths: Option<Arc<Mutex<BTreeSet<PathBuf>>>>,
    /// Stats for metrics aggregation.
    pub stats: Arc<MaterializeStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKind {
    TempDir,
    HardlinkView,
    ArchiveBacked,   // sparse dir filled on ensure_file
    ComposefsMount,  // Phase-2
}

#[derive(Debug, Default)]
pub struct MaterializeStats {
    pub files_hydrated: AtomicU64,
    pub bytes_hydrated: AtomicU64,
    pub bytes_package: AtomicU64,
    pub hardlink_hits: AtomicU64,
    pub range_gets: AtomicU64,
    pub full_extracts: AtomicU64,
    pub verify_failures: AtomicU64,
}
```

### 3.4–3.5 CAS fetch + errors

`CasFetcher`: `get_blob`, `get_range`, `put_local_verified`.  
`PackRef` / `IndexRef`: `Local(PathBuf) | Remote(RemoteObject) | Inline(index)`.  
Errors: `PathJail`, `UnknownPath`, `HashMismatch`, `MissingLocator`, `IndexSkew`, `Io`, `Remote`, `Unsupported`.

### 3.6 Selection policy (`Auto`)

```text
function select_backend(spec, local_cas):
  present = count(files whose content_hash in local_cas)
  ratio_present = present / files.len()

  if spec.prefer != Auto: return spec.prefer

  if ratio_present >= 0.95:
    return CasHardlink          # fill holes via per-file GET if any

  if spec.index.is_some() && spec.pack.is_some():
    if expected_touch_ratio(spec) >= 0.70:
      return FullExtract        # lazy loses for full-tree producers
    return LazyArchive

  if ratio_present >= 0.50:
    return CasHardlink          # fetch missing files individually

  return FullExtract            # or bulk pack download if pack without needing lazy
```

`expected_touch_ratio` defaults:
- GUI single-file / code view: **0.01–0.05** → LazyArchive  
- `source_archive` + CST full walk: **1.0** → FullExtract / hardlink-all  
- Unknown producer: instrument (see §6); start **FullExtract** for sealed compile until metrics say otherwise  

---

## 4. On-disk formats

### 4.1 Layout (local REGISTRY)

```
$NUDOX_HOME/
  registry.sqlite
  cas/
    ab/cd/{64hex}.blob          # per-file SoR (existing direction)
  packs/
    {generation_hash_hex}.ndpk  # optional transfer pack
    {generation_hash_hex}.ndix  # ArchiveIndex (postcard or json)
    # OR content-addressed:
    # cas/packs/{pack_hash}.ndpk
    # cas/indexes/{index_hash}.ndix
  views/
    {view_id}/                  # hardlink farms / sparse trees
  tmp/
```

### 4.2 Layout (remote INDEX / S3)

```
s3://nudox-index/
  cas/{blake3-hex}                 # file bodies, IR, manifests (existing)
  ptr/{package-uuid}               # manifest pointer (existing)
  packs/{generation_hash}.ndpk     # OPTIONAL
  packs/{generation_hash}.ndix     # OPTIONAL ArchiveIndex bytes
  # alternative content-addressed:
  # packs/by-hash/{pack_hash}.ndpk
  # packs/by-hash/{index_hash}.ndix
```

**Pointer fields** (catalog / generation row / sync JSON projection):

```jsonc
{
  "generation_hash": "...",
  "manifest_hash": "...",
  "pack": { "hash": "...", "size": 123456, "codec": "ndpk-v1" },
  "index": { "hash": "...", "size": 4096, "format_version": 1 }
}
```

Pack/index absence is **valid** — clients fall back to per-file CAS GET.

### 4.3 `.ndpk` — NdPkV1 (MVP pack)

**Design goals:**  
- Independent per-file compression (range GET without solid-window dependency)  
- Simple to write at produce time  
- Verify whole pack via `pack_hash`  
- Streamable build (one file at a time)

```text
NdPkV1 file:
  magic: b"NDPK"                   # 4 bytes
  u16 format_version = 1
  u16 reserved = 0
  u64 file_count
  // then file_count frames in the same order as ArchiveIndex.files sorted by path:
  frame:
    u32 path_len
    path UTF-8 bytes
    32 bytes content_hash
    u64 uncompressed_size
    u8  frame_codec                # 0=raw, 1=zstd
    u64 payload_len
    payload_len bytes              # compressed or raw
  // optional trailer: blake3 of all frame payloads only? — prefer hash whole file for pack_hash
```

**Index locators** store `compressed_offset` at the start of each frame’s **payload** (or start of frame — pick one and document; recommend **start of frame** so reader can re-parse path/hash and verify).

**Why not ZIP-only MVP?** ZIP works (docs.rs) and has central directory; Rust `zip`/`piz` exist. NdPkV1 is slightly simpler for **content_hash in-band** and BLAKE3-native tooling. Accept **ZipV1** as alternate `PackCodec` if export interop matters; materializer switches on codec.

**Seekable solid zstd:** allowed as **optional PackCodec later** (`ZstdSeekableV1`) using zstd seekable format (13 §4.4.3). Not MVP — frame-per-file is enough.

### 4.4 `.ndix` — ArchiveIndex encoding

| Encoding | Use |
|---|---|
| **postcard** | Production CAS object (compact, matches IR stack) |
| **JSON** | Debug, `nudox pack inspect`, multi-language tools |

`format_version` in struct gates breaking changes. Filename extension `.ndix` for both; content-type / magic:

```text
postcard: no magic required if addressed by hash; optional prefix b"NDIX" || u16 ver
json: { "format": "nudox.archive-index/1", ... }
```

### 4.5 Relationship to SourceArchive / BlobManifest

```text
Produce pipeline:
  PackageInput tree
       │
       ├─ source_archive::build → SourceArchive { files: [FileDigest] }
       │         │
       │         └─ becomes BlobManifest.files: NonEmpty<FileEntry>
       │
       └─ (new) pack_builder::build(tree, digests)
                 → writes .ndpk
                 → builds ArchiveIndex { files: FileEntryExt with locators }
                 → puts index + pack to cas/packs (or local packs/)
                 → records pack/index hashes on generation

Materialize pipeline:
  BlobManifest.files  ──truth for path→hash──┐
  ArchiveIndex        ──locators + same hashes──┼→ Materializer
  local cas/ + optional pack                  ─┘
```

**Invariant:**  
`∀ path: ArchiveIndex.files[path].content_hash == BlobManifest.files[path].hash`  
and sizes match. Validate at index load.

**SourceArchive stage (JobKey `archive`):** remains postcard of digests only (06 §8.7). Bodies live in L5/CAS. Pack build can be a **post-stage** or part of emit to INDEX.

### 4.6 zstd frame boundaries

MVP: **one zstd frame (or raw) per file** inside NdPkV1.  
- `compressed_offset` / `compressed_length` exactly cover the range to fetch.  
- Decompress → verify BLAKE3 → `CasFetcher::put_local_verified`.

Do **not** use one solid zstd stream for MVP (breaks independent range without seek table).

### 4.7 Index generation timing

| Timing | When | Pros | Cons |
|---|---|---|---|
| **Produce time (preferred)** | After tree available in compiler/emit | One pass with hashing; index always matches | Slightly longer produce |
| **Post-hoc** | INDEX job / offline tool over existing pack or per-file CAS | Backfill old gens | Need pack first or synthetic pack from files |
| **Client-side** | Never required for correctness | — | Don’t rely on clients to invent indexes |

**MVP:** generate at produce/emit when uploading a generation.  
**Tool:** `nudox pack build --from-cas --generation G` for backfill.

### 4.8 Compression policy (aligned 13 GD-12)

| Object | Codec |
|---|---|
| Per-file CAS body | raw or zstd envelope (13) — independent of pack |
| Pack frame | zstd level 3–5 interactive; 9–12 cold offline rebuild |
| Tiny files | frame_codec=Raw if zstd expands |
| Index | postcard; optionally zstd if large (rare) |

---

## 5. Materializer implementations

### 5.1 Matrix

| Impl | When | Mechanism | Platforms |
|---|---|---|---|
| **CasHardlinkMaterializer** | REGISTRY/node has (almost) all file hashes | `linkat`/`std::fs::hard_link` from `cas/` into view dir; fetch missing via per-file GET | All (hardlink); copy-on-fail (Windows/cross-device) |
| **LazyArchiveMaterializer** | pack+index present; working set small | Range GET or local seek → decompress one file → CAS put → hardlink into sparse view | All |
| **FullExtractMaterializer** | fallback; high touch ratio; no pack; first producer needing many files | Extract all frames / copy all CAS files into tmpdir or hardlink-all | All |
| **ComposefsMaterializer** | Phase-2 Linux | Build composefs metadata from index; mount RO; objects = cas/ | Linux only |

### 5.2 CasHardlinkMaterializer

**Algorithm `ensure_tree`:**
```text
1. create view_root (views/{id}/ or scratch/job/tree/)
2. for each (path, hash) in manifest.files:
     if !path_jail_ok(path): error
     src = cas_path(hash)
     if !src.exists():
       if remote: get_blob(hash) → put_local_verified
       else: error Missing
     create_parent(view_root/path)
     hardlink(src, view_root/path) or copy if EXDEV/EPERM
3. kind = HardlinkView
4. mark all paths materialized
```

**TrustedLocal shortcut:** if `spec` includes `override_root: Some(project_path)` and trust allows, return that path as view **without** copying (optional extension). Still verify optional sampling of hashes for safety on CI.

**UntrustedSealed:** never follow user-supplied roots; only CAS-backed files.

### 5.3 LazyArchiveMaterializer

**Algorithm `open_view`:**
```text
1. load ArchiveIndex; validate vs manifest fingerprint
2. create empty view_root with directory structure OPTIONAL:
   - MVP: create dirs lazily on ensure_file
   - optional: create full dir skeleton (no file bodies) for tools that readdir
3. kind = ArchiveBacked
```

**Algorithm `ensure_file(path)`:**
```text
1. jail path; lookup FileEntryExt
2. if view path exists and hash ok: return
3. if cas has content_hash: hardlink into view; return
4. if pack_locator:
     bytes = local_seek(pack, offset, len) OR remote.get_range(pack, range)
     raw = decompress(frame_codec, bytes)
     verify blake3(raw) == content_hash
     put_local_verified(hash, raw)
     hardlink into view
5. else fallback: remote.get_blob(content_hash)  // per-file CAS
6. stats.bytes_hydrated += raw.len()
```

**Directory listing:** For producers that `read_dir` the whole tree, either:
- (A) materializer pre-creates **empty files** or **directory entries with xattr placeholder** — complex; or  
- (B) policy flips to FullExtract when producer class needs full walk; or  
- (C) prefetch all paths from index into skeleton + hydrate on read via interceptor — **no FUSE in MVP**, so (B) is the honest answer for full-walk producers.

**MVP rule:** LazyArchive is for **known pathsets** (GUI file open, targeted prefetch). Full-tree producers use CasHardlink/FullExtract.

### 5.4 FullExtractMaterializer

```text
1. if pack present: stream all frames → verify each → cas put → hardlink farm
2. else: for each manifest file get_blob → cas → hardlink
3. kind = TempDir or HardlinkView
4. stats.full_extracts += 1
```

This is today’s daemon behavior, upgraded to CAS intermediate (so second job on node hits hardlinks).

### 5.5 ComposefsMaterializer (Phase-2)

```text
1. Generate composefs metadata image from ArchiveIndex (path, mode, blake3)
2. Objects directory = node cas/ layout mapped to composefs object names
3. mount RO at view_root
4. upper/scratch is separate bind (never in composefs lower)
```

Feature-gate `materialize-composefs`. Fallback to hardlink if mount fails.

### 5.6 Composite router

```rust
pub struct RoutingMaterializer {
    pub hardlink: CasHardlinkMaterializer,
    pub lazy: LazyArchiveMaterializer,
    pub full: FullExtractMaterializer,
    // pub composefs: Option<ComposefsMaterializer>,
}

impl Materializer for RoutingMaterializer {
    async fn ensure_tree(&self, spec: &MaterializeSpec) -> Result<MaterializedTree, MaterializeError> {
        match select_backend(spec, &self.hardlink.cas)? {
            MaterializePrefer::CasHardlink => self.hardlink.ensure_tree(spec).await,
            MaterializePrefer::LazyArchive => self.lazy.ensure_tree(spec).await,
            MaterializePrefer::FullExtract => self.full.ensure_tree(spec).await,
            MaterializePrefer::Auto => unreachable!("select resolves Auto"),
        }
    }
    // ensure_file: if tree is ArchiveBacked, delegate lazy; else path must exist or error
}
```

### 5.7 Cross-device / Windows / APFS notes

| Platform | Hardlink | Fallback |
|---|---|---|
| Linux same FS | Yes | — |
| macOS APFS same volume | Yes | — |
| Cross-device | Fail EXDEV | **clonefile/reflink** if available, else copy |
| Windows | Privilege/FS dependent | Copy; optional reflink on ReFS later |

**Copy fallback still wins** vs re-download: CAS holds one body; views may duplicate bytes only when hardlink impossible.

---

## 6. Producer integration

### 6.1 Today’s path (daemon)

```text
CompileRequest.files[] 
  → write all to tempdir 
  → peel_single_top_level 
  → PackageInput { root } 
  → generate_with(forge, input)
  → source_archive walks ALL files again (hash)
```

### 6.2 Target path

```text
MaterializeSpec { generation, files from seal/manifest, prefer: Auto, trust: UntrustedSealed }
  → materializer.ensure_tree(spec) → MaterializedTree
  → PackageInput { root: tree.root, ... }
  → generate_with(forge, input)
  → on drop / job end: release pin; keep cas/ bodies
```

### 6.3 ForgeContext injection

Extend forge/runtime (conceptual):

```rust
pub trait ForgeContext {
    // existing: cas, cage, toolchains, ...
    fn materializer(&self) -> Option<&dyn Materializer> { None }
}

// Daemon:
// forge.materializer = Some(RoutingMaterializer { ... node cas, pack cache ... })
```

Seal path (`isolate::seal`):
- RO bind `tree.root` into cage  
- RW scratch at `job_scratch/out` (Eden redirection analogue)  
- Never mount pack files RW  

### 6.4 How many files do producers touch?

| Producer class | Expected touch ratio | Policy |
|---|---|---|
| `source_archive::build` | **1.0** (walks tree) | FullExtract or hardlink-all **before** stage |
| CST / treesitter per package | **~1.0** source files | Same |
| rust-analyzer / surface | **high** (most sources + deps?) | Prefer full package tree; deps may be separate generations |
| oxc / TS path | **medium–high** | Measure; default full for sealed package root |
| go oracle | **high** for module | Full module tree |
| GUI code view | **≪0.05** | LazyArchive / single ensure_file |
| Snippet / go-to-def one file | **1 file** | ensure_file only |

**Unknown → instrument:**

```rust
// Debug/metrics wrapper
struct TouchCountingRoot { /* log read_dir/open under view */ }
// Metric: materialize_touch_ratio = unique_paths_opened / manifest.file_count
```

**Policy gate (MVP):**  
If `producer_kind` ∈ full-walk set → force `FullExtract` or `CasHardlink` complete tree.  
If GUI → `LazyArchive` / single-file.  
If `touch_ratio` observed > `0.6` on LazyArchive sessions → auto-upgrade to full for that generation session.

### 6.5 Untrusted k8s specifics

```text
/var/lib/nudox/source-cas/          # L5 node CAS (hostPath), RO to package
/scratch/job-{id}/
    tree/                           # hardlink view (RO bind into cage)
    scratch/                        # RW upper for builds
    packs/                          # optional local pack cache
```

- Package process: **no network** (existing seal story).  
- Hydrate **before** cage or via helper **outside** cage (host-side materializer).  
- Prefer: materializer runs in daemon/helper; cage only sees finished RO tree (+ on-demand is harder without FUSE).  

**Important MVP constraint without FUSE:**  
For sealed compiles, **lazy-on-open inside the cage is not available** unless we pre-hydrate or use composefs/FUSE. Therefore:

| Surface | Lazy inside process? |
|---|---|
| Desktop GUI (our code opens files) | **Yes** — ensure_file in app |
| Compiler daemon before seal | **Yes** — hydrate then seal full or partial tree |
| Untrusted binary inside cage | **No** — must pre-materialize declared inputs |

**Declared inputs:** seal bundle should list package generation files needed; materializer prefetches that set. If producers need full tree, prefetch all.

### 6.6 Trusted local projects

```text
TrustTier::TrustedLocal
  → open_view may return project root PathBuf directly
  → optional background hash verify vs lockfile/generation
  → no pack required
```

Hardlink views used when materializing **dependency packages** into a sandbox still RO.

---

## 7. INDEX / REGISTRY / ORCH wiring

### 7.1 INDEX

| Responsibility | Detail |
|---|---|
| Store per-file CAS | Unchanged |
| Store optional pack+index | `packs/{gen}.ndpk` + `.ndix` |
| Catalog fields | `pack_hash`, `index_hash`, sizes |
| Presign API | `GET pack`, `GET index`, `GET cas/{hash}`, **Range** on pack |
| Authz | Same as cas; private packages ACL |

```
GET /v1/generations/{gen}/index → ArchiveIndex bytes
GET /v1/generations/{gen}/pack  → .ndpk (Accept-Ranges: bytes)
GET /v1/cas/{hash}              → file body
POST /v1/cas/has                → missing subset (13)
```

### 7.2 SyncEngine (REGISTRY client)

**Cold package prefer order:**

```text
1. Fetch generation manifest (small)
2. If index+pack advertised AND (want full tree OR many missing files):
     GET index (small)
     decision:
       a. many missing (>K files or >M bytes): GET pack (whole) OR ranged multi-GET
       b. few missing: per-file cas GET OR ranged pack members
3. Else classic: want/have on file hashes → N GETs (HTTP/2 multiplex)
4. Always verify hashes; put into local cas/
5. Optionally retain pack in packs/ for future lazy views
```

**Why pack can beat N small GETs:** S3/LIST/request overhead + TTFB per object dominates for 500–5000 tiny files (docs.rs lesson inverted for **download** side).

**Why per-file still wins for warm cache:** cross-version dedupe; second version may need only 10% pack equivalent.

### 7.3 ORCH / compiler fleet (L5)

From 06 §8 — this plan implements L5:

```text
Admission L0 miss → pod starts
  materializer (node cas + optional pack cache)
    → job tree RO
    → L1 stage CAS produce
    → upload outputs
```

Env (aligned 06):
```yaml
NUDOX_SOURCE_CAS_ROOT: /var/lib/nudox/source-cas
NUDOX_PACK_CACHE: /var/lib/nudox/packs
NUDOX_MATERIALIZE_PREFER: auto
```

Fleet shared **read-only** hydrated cas files via node FS; no cross-tenant mutable views.

### 7.4 Desktop REGISTRY open package

```text
GUI open(package, gen):
  load manifest from sqlite
  spec.prefer = LazyArchive if index local or remote else CasHardlink
  tree = materializer.open_view(spec)
  on user open file: ensure_file
  code view: read path; re-parse treesitter (no tree persist)
  pin = Session; release on tab close → delete view dir, keep cas
```

### 7.5 Metrics emission points

| Metric | Labels | Source |
|---|---|---|
| `materialize_cold_open_seconds` | backend, surface | histogram |
| `materialize_bytes_hydrated` | backend | counter |
| `materialize_package_bytes` | — | gauge per op |
| `materialize_hydrate_ratio` | backend | histogram of bytes_hydrated/package |
| `materialize_hardlink_hits` | — | counter |
| `materialize_range_gets` | — | counter |
| `materialize_verify_failures` | — | counter |
| `materialize_full_extract_total` | reason | counter |

---

## 8. Security

### 8.1 Path jail

```text
On every index/manifest path:
  - reject absolute paths
  - reject empty, `.`, `..` components
  - reject NUL / Windows device names if applicable
  - normalize with a strict relative component walk
  - resolve view path; ensure starts_with(view_root) after canonicalize of parent
```

Apply at **index validate**, **pack build**, and **ensure_file**.

### 8.2 Hash verify every hydrate

```text
raw = decompress(range_bytes)
actual = blake3(raw)
if actual != entry.content_hash:
  stats.verify_failures++
  do not put into cas/
  do not hardlink
  return HashMismatch
```

Whole-pack `pack_hash` verified when downloading full pack. Partial range relies on **per-file content_hash** (sufficient).

### 8.3 No FUSE required for MVP

Reduces attack surface (no privhelper, no kernel module dance). Sealed jobs get real directories.

### 8.4 Symlinks

MVP: **do not materialize symlinks** from packs (match `source_archive` drop behavior). If later needed: only relative targets that jail within root; never absolute.

### 8.5 TOCTOU / view races

- Views are private directories per job/session.  
- CAS puts use atomic publish (DiskCas hardlink publish pattern).  
- Do not mutate files in-place under `cas/`.

### 8.6 Multi-tenant presence oracle

Public packages: shared node CAS OK.  
Private packages: tenant-scoped cas prefix or encrypt-at-rest + ACL on fetch; do not hardlink private into shared world-readable path.

### 8.7 Untrusted code

- Cannot write to cas/  
- Cannot call materializer APIs  
- Only sees RO tree + RW scratch  
- Network off in cage  

### 8.8 Index poisoning

Index is content-addressed or generation-signed via INDEX authority. Client checks `files_manifest_fingerprint` against trusted manifest from catalog. Skew → reject index, fall back to per-file CAS.

---

## 9. API placement

### 9.1 Crate decision

| Option | Pros | Cons |
|---|---|---|
| **`materialize` new crate** | Clear boundary; compiler+registry depend | More topology |
| Types in **`blob`** / registry | Near manifest | Pulls FS into blob |
| Types in **`heart`** | Universal | heart should stay lean |

**Recommendation:**

```text
workspace/materialize/          # NEW crate (or blob/materialize module if topology freeze forbids crate)
  src/
    lib.rs                      # Materializer trait, Spec, Tree, errors
    index.rs                    # ArchiveIndex types + validate
    pack_ndpk.rs                # NdPkV1 read/write
    pack_zip.rs                 # optional
    hardlink.rs                 # CasHardlinkMaterializer
    lazy_archive.rs             # LazyArchiveMaterializer
    full_extract.rs
    routing.rs
    path_jail.rs
    metrics.rs

heart:                          # ContentHash only (already)
registry/blob:                  # FileEntry, BlobManifest; optional IndexRef on emit
compiler:                       # depends on materialize; daemon wires RoutingMaterializer
client-core / registry local:   # SyncEngine uses pack prefer; GUI open_view
```

If 22-crate topology wants fewer crates: put modules under `registry` as `registry::materialize` and re-export trait for compiler. **Prefer dedicated `materialize` crate** so compiler does not import full registry DB.

### 9.2 Traits in heart or blob?

- **`ContentHash`, `JobKey`:** heart (unchanged)  
- **`FileEntry` / `BlobManifest`:** registry blob (unchanged)  
- **`Materializer` trait:** **materialize crate** (not heart — FS/async heavy)  
- Optional: `ManifestFiles` conversion from `SourceArchive` / `BlobManifest` in materialize via thin adapters

### 9.3 Feature flags

```toml
[features]
default = ["ndpk"]
ndpk = ["zstd"]
zip = ["dep:zip"]
composefs = []  # linux
```

---

## 10. Implementation steps (PR-sized)

### S-M.1 — Types + path jail + tests  
**PR:** `materialize` crate skeleton  
- `ArchiveIndex`, `FileEntryExt`, `MaterializeSpec`, `MaterializedTree`, errors  
- `path_jail` unit tests (reject `..`, abs, etc.)  
- postcard round-trip tests for index  

### S-M.2 — NdPkV1 writer/reader  
**PR:** pack format  
- write pack from directory + digests  
- read frame by locator  
- fuzz path lengths / truncated frames  

### S-M.3 — CasHardlinkMaterializer  
**PR:** hardlink farm  
- integrate DiskCas layout  
- EXDEV copy fallback  
- unit tests on tempdirs  

### S-M.4 — FullExtractMaterializer  
**PR:** parity with daemon tempdir path  
- extract from pack or CAS list  
- used as baseline bench  

### S-M.5 — LazyArchiveMaterializer (local seek)  
**PR:** local pack hydrate  
- ensure_file path  
- stats counters  
- property test: lazy full walk ≡ full extract hashes  

### S-M.6 — Index generation in produce/emit  
**PR:** compiler or emit stage  
- after `source_archive::build`, optional pack+index  
- upload hooks for INDEX  
- does not change JobKey identity (pack is not in generation Hash ①)  

### S-M.7 — Daemon integration  
**PR:** replace write-all tempdir  
- `RoutingMaterializer` with Auto  
- sealed jobs: FullExtract/CasHardlink complete tree  
- metrics  

### S-M.8 — INDEX presign Range + catalog fields  
**PR:** server  
- store pack/index  
- Range GET support verification on object_store backend  
- generation JSON includes pack/index pointers  

### S-M.9 — SyncEngine prefer pack/index  
**PR:** client sync  
- cold path decision tree §7.2  
- still want/have for warm  

### S-M.10 — GUI open_view + ensure_file  
**PR:** desktop  
- session pins  
- cold open metrics  

### S-M.11 — Fleet L5 wiring  
**PR:** orch/k8s  
- hostPath source-cas  
- warm+cold pods  
- document ops  

### S-M.12 — Benchmarks & policy thresholds  
**PR:** benches  
- cold open p95 harness  
- hydrate ratio  
- tune Auto thresholds  

### S-M.13 — (Phase-2) ComposefsMaterializer  
**PR:** linux-only feature  
- mount helper  
- fallback  

### Dependency order

```text
S-M.1 → S-M.2 → S-M.3 → S-M.4 → S-M.5
                ↘ S-M.6 → S-M.8 → S-M.9
S-M.3+S-M.4+S-M.5 → S-M.7 → S-M.11
S-M.5 → S-M.10
S-M.7+S-M.10 → S-M.12
S-M.13 later
```

---

## 11. Risks & open questions

### 11.1 Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Full-walk producers thrash LazyArchive | High | Force FullExtract for compile producers; no in-cage lazy without FUSE |
| Pack becomes accidental SoR | High | Policy: GC may drop packs; never drop unique cas files still referenced |
| Hardlink cross-device copies explode disk | Med | Detect FS; prefer single cas volume; metrics on copy fallback |
| Index/manifest skew after partial upload | Med | Atomic publish: cas files → pack → index → catalog pointer |
| Windows hardlink support weak | Med | Copy fallback; test on CI windows |
| Range GET cost vs whole pack for medium files | Med | Benchmark; threshold switch to full pack GET |
| Double compression (cas zstd + pack zstd) | Low | Pack from raw logical bytes; cas stores raw or single envelope |
| TOCTOU on view paths | Low | Private view dirs; atomic link |
| Producer reads mtime/size before hydrate (sparse) | Med | Don’t use sparse views for those producers |
| Large index memory | Low | Indexes small (path+hash+8+8)×N; 10k files ~ MB |

### 11.2 Open questions

1. **ZipV1 vs NdPkV1 only?** NdPkV1 MVP; add Zip if external interop demanded.  
2. **Should pack_hash enter any generation identity?** **No** — optional optimization artifact.  
3. **Dir skeleton for ArchiveBacked?** Implement empty dirs for readdir UX in GUI? Probably yes for GUI; no for sealed compile (full tree anyway).  
4. **Touch instrumentation sampling rate** in prod pods?  
5. **Private pack cache eviction** vs source cas LRU — shared quota or separate?  
6. **composefs object naming** map blake3 → composefs file names — exact encoding?  
7. **Should `source_archive` skip re-hash when materializer guarantees verified CAS?** Optimization: trust manifest hashes if `TreeKind::HardlinkView` from verified cas (big win). Requires careful API.  
8. **HTTP/2 multiplexing** of many cas GETs vs pack on WAN — need real numbers (S-M.12).  
9. **Whether daemon CompileRequest still ships full file bytes** in-band vs pass generation+presign only (wire size). Materializer prefers generation-addressed inputs.  
10. **GD-27 CAS layouts:** shard depth `ab/cd/` vs flat — materializer must use same `cas_path` helper as DiskCas/Store.

### 11.3 Alignment with master plan GDs

| GD | Topic | This plan |
|---|---|---|
| GD-11 | No tree-sitter Tree persistence | Re-parse only; materializer supplies source bytes paths |
| GD-12 | Compression | zstd frames in pack; cas envelope per 13 |
| GD-27 | CAS layouts | Shared path helper; packs under `packs/` or `cas/packs/` |

---

## 12. Decision record

### 12.1 ADOPT (MVP)

1. **`Materializer` trait** with `ensure_tree` / `ensure_file` / `open_view`.  
2. **`CasHardlinkMaterializer`** for warm local CAS.  
3. **`LazyArchiveMaterializer`** for pack+`.ndix` SOCI-like indexes.  
4. **`FullExtractMaterializer`** fallback and full-walk producers.  
5. **NdPkV1 + ArchiveIndex** formats; produce-time index generation.  
6. **Per-file CAS remains SoR**; packs optional.  
7. **Hash verify on every hydrate**; path jail.  
8. **No FUSE / no EdenFS.**  
9. **Metrics:** cold open p95, hydrate ratio.  
10. **Crate:** `materialize` (or `registry::materialize` if topology constrained).

### 12.2 PHASE-2

- ComposefsMaterializer (Linux pods)  
- Seekable solid zstd pack codec  
- ZipV1 export  
- In-cage lazy (only if composefs/FUSE ever justified)  
- Optional CID/CAR boundary export (04) using same file lists  

### 12.3 REJECT

- EdenFS / production desktop FUSE as dependency  
- Archive-only storage without per-file CAS  
- Trusting index without manifest fingerprint check  
- Shared mutable view dirs across jobs  

### 12.4 One-line decision

**ADOPT CasHardlink + LazyArchive as the MVP materializer pair; FullExtract fallback; SOCI-like external `.ndix` over seekable `.ndpk`; composefs later; never EdenFS.**

---

## 13. Worked examples

1. **Desktop cold one file:** GET index → ensure_file(`src/lib.rs`) range → hydrate_ratio ≪ 0.05.  
2. **k8s warm second version:** CasHardlink ~90% hits; range/GET remainder; full tree for RA.  
3. **Cold full producer:** Auto → FullExtract (touch≈1.0); subsequent jobs CasHardlink 100%.  
4. **Trusted local:** `TrustTier::TrustedLocal` returns project root; deps hardlinked RO into sandbox.

---

## 14. Test plan (normative)

| Kind | Coverage |
|---|---|
| Unit | path jail; index skew; NdPkV1 round-trip; hash mismatch refuses cas put |
| Integration | lazy-all-paths ≡ full extract; daemon fixture IR golden; sync pack path ≡ per-file cas end state |
| Fuzz | pack frames; index postcard; path strings |
| Bench (S-M.12) | `cold_open_one_file` p50/p95; full extract 1k files; hardlink 1k; range vs whole-pack crossover |

---

## 15. Migration, ops, compatibility

| Topic | Rule |
|---|---|
| Generations without pack | Valid forever; CasHardlink + per-file GET |
| Pack/index in Hash ① | **Never** — catalog/sync JSON pointers only |
| Daemon in-band files | Keep during transition; prefer generation+presign |
| DiskCas layout | Shared `cas_path` helper (GD-27) |
| GC | Evict **packs first**, then unpinned cas (13 LRU) |
| Corrupt view | Delete view dir; rebuild from cas |
| Verify failure | No cas put; quarantine pack; fallback per-file |
| Node L5 wipe | Safe; rehydrate from INDEX |

---

## 16. Bytes & latency model (sketch)

Assumptions: S=2 MB, N=150 files, WAN RTT=50 ms, ~20 MB/s.

| Strategy | Cold full tree | Cold one file | 10 versions @ 15% churn disk |
|---|---|---|---|
| Daemon write-all today | full S | full if wire has all | copies per job |
| N per-file GET | RTT-dominated | 1×RTT | CAS ~3.5×S |
| Whole pack GET | 1×(RTT+S/bw) | overkill | pack optional + CAS |
| Lazy range | — | 1×RTT + frame | CAS grows with touch |
| Hardlink warm | ~ms × N | ms | CAS shared |

**Conclusion:** LazyArchive → interactive single-file; CasHardlink → warm multi-version; FullExtract/whole-pack → cold full-tree first paint.

---

## 17. Appendices (compact)

### A — Format constants

```text
NDPK_MAGIC = b"NDPK"
NDIX_FORMAT = "nudox.archive-index/1"
ARCHIVE_INDEX_FORMAT_VERSION = 1
DEFAULT_ZSTD_LEVEL_PACK = 5
TOUCH_RATIO_FULL_THRESHOLD = 0.70
CAS_PRESENT_HARDLINK_THRESHOLD = 0.95
```

### B — Eden redirections → scratch

| Eden | nudox |
|---|---|
| redirect `buck-out` | `job_scratch/` RW outside `tree/` |
| Overlay dirty WC | **Disallowed** for sealed generations |
| thrift getSHA1 | Manifest BLAKE3 digests |
| SNAPSHOT switch | New `MaterializeSpec.generation` |

### C — Producer policy (copy-ready)

```text
producer_kind          default_prefer
gui_open_file          LazyArchive
gui_open_package       LazyArchive
source_archive         CasHardlink/Full
cst_walk               CasHardlink/Full
surface_ra / oxc       CasHardlink/Full  (until measured)
oracle_*               CasHardlink/Full
trusted_project_root   TrustedLocal path
dep_package_sandbox    CasHardlink RO
```

### D — PR review checklist

- [ ] path jail tests · verify before cas put · pack not in Hash ①  
- [ ] metrics hooked · Auto policy documented · Windows copy fallback  
- [ ] Untrusted: hydrate outside cage · GC packs first  

### E — Glossary

| Term | Meaning |
|---|---|
| **.ndix / ArchiveIndex** | External SOCI-like TOC |
| **.ndpk** | NdPkV1 transfer pack |
| **Hydrate** | Fetch+verify+store into cas/ |
| **View** | Hardlink/copy tree for a job/session |
| **L5** | Node-local source materialize cache (06) |
| **Touch ratio** | files opened / package file count |

### F — ensure_file sequence

```text
ensure_file(path) → jail → manifest hash
  → cas hit? hardlink
  → else index locator? range GET → decompress → verify → cas put → hardlink
  → else per-file GET → verify → cas put → hardlink
```

---

## 18. Decision (restated)

**ADOPT CasHardlink + LazyArchive as the MVP materializer pair; FullExtract fallback; SOCI-like external `.ndix` over seekable `.ndpk`; per-file CAS remains SoR; composefs later; never EdenFS; no FUSE for MVP.**

---

## 19. Summary recommendations

1. Ship **`materialize` crate** with types + path jail (S-M.1) immediately.  
2. **CasHardlink + daemon/L5** before LazyArchive if serial schedule.  
3. **LazyArchive + INDEX pack** for desktop cold open wins.  
4. Keep **FullExtract** for full-walk producers — do not fake laziness in-cage without FUSE.  
5. **Instrument touch ratio** and hydrate ratio; freeze Auto thresholds after S-M.12.  
6. Packs are **disposable caches**; never the only replica of source.  
7. Shared **`cas_path`** with DiskCas/Store (GD-27).  
8. Do not put pack pointers into **BlobManifest Hash ①**.  
9. Prefer **generation-addressed** daemon inputs over shipping all bytes on the wire.  
10. composefs spike only after hardlink L5 metrics exist.

---

## 20. Open questions

1. `materialize` crate vs `registry::materialize`? → **Prefer crate.**  
2. ZipV1 at MVP? → **No; NdPkV1 only.**  
3. Sparse seal for untrusted jobs later? → **Full tree MVP.**  
4. Skip re-hash in `source_archive` when view is verified CAS? → **S-M.7.1 yes.**  
5. Pack vs cas quota? → **Separate LRU; packs first.**  
6. composefs when? → After L5 hardlink data.  
7. GUI dir skeleton without bodies? → Optional.  
8. WAN crossover pack vs N GETs? → Measure S-M.12.  
9. Private multi-tenant L5 isolation: prefix vs no-share? → ACL + tenant prefix.  
10. Dual-write pack at produce vs async INDEX post-job? → Prefer produce/emit atomic publish order: files → pack → index → catalog.

---

*End of report — Materializer + SOCI-like archive index (07).*
