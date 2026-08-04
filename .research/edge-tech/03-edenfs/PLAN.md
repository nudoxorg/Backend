# EdenFS & Virtualized Source Trees — Research for nudox Source Hosting

> **Status (LIBRARIFICATION-PLAN Rev 2, 2026-07-16):** EdenFS verdict is unchanged — rejected (GD-29). Additionally, composefs/EROFS (discussed as a Phase-2 Linux upgrade path in §5.12–5.13 and §8) is also **cut** in Rev 2 (GD-35, "no kernel-mount materialization"); hardlink + range-lazy `.ndpk`/`.ndix` covers materialization and the composefs Phase-2 pod idea is gone. The core CAS + hardlink + `Materializer` trait recommendation (§8.5) remains normative.

**Research date:** 2026-07-16  
**Scope:** EdenFS (Meta/Sapling), related virtual-FS / CAS materialization patterns, and fit for nudox INDEX (S3 CAS) + REGISTRY (local disk) + compiler materialize paths.  
**Method:** Primary docs from `facebook/sapling` EdenFS design pages; Meta engineering posts; open-source status as of mid-2026; comparative research on git sparse/partial, Buck2 deferred materialization, Nix/composefs, JuiceFS/Alluxio, fuse-overlayfs, EROFS/OCI, casync/restic.  
**Non-goals:** Building or shipping EdenFS; monorepo SCM adoption; replacing BLAKE3 CAS design from Plan 13-storage.

**Cross-refs (local):**
- `.research/librarification/13-storage/PLAN.md` — per-file BLAKE3 CAS, re-parse treesitter, optional transfer packs, docs.rs ZIP+range, Nix narinfo
- `.research/librarification/01-compiler-audit/PLAN.md` — daemon materialize tempdir → peel tarball → `generate_with`; producers need real `PathBuf` trees; sandbox RO binds
- `.research/resolution/03-exhaustive-plan.md` §2.3 — `CodebaseInput { root: PathBuf, … }` consumer workspace model

---

## 0. Executive summary

| Question | Answer (2026-07) |
|---|---|
| What is EdenFS? | Meta’s **lazy virtual checkout FS** for massive monorepos; FUSE (Linux), FUSE/NFS (macOS), ProjFS (Windows); thrift control plane; Sapling/hg + limited git backing stores |
| Production outside Meta? | **No meaningful third-party production.** OSS code lives in `facebook/sapling`; README still says **“not yet supported for external usage”**; buildable for experiment only |
| Can a Rust app embed EdenFS? | **Practically no.** Heavy C++ daemon + privhelper + thrift; designed as per-user SCM companion, not a library |
| Fit for multi-version package CAS? | **Poor product fit.** EdenFS keys checkouts to **SCM commits/repos**, not generation manifests of BLAKE3 file hashes |
| Fit for k8s untrusted compile? | **Poor.** Daemon, privhelper, FUSE/NFS complexity, multi-tenant isolation risk, image weight |
| Fit for desktop REGISTRY disk savings? | Conceptually attractive (lazy hydrate) but **wrong stack** — implement lazy hydrate **over our CAS**, not EdenFS |
| **Verdict** | **Do not adopt EdenFS.** Stick to **CAS + selective materialize-to-tmpdir** (+ optional sparse/hardlink/composefs-class mounts later). Steal ideas: lazy hydrate, content-hash thrift, redirections for build output, deferred materialization (Buck2) |

**Per-surface recommendation (preview of §8):**

| Surface | Recommendation |
|---|---|
| **INDEX server** | No virtual FS. Serve `manifest + cas/{blake3}` (+ optional transfer packs). Materialization is a client concern |
| **k8s compiler pods** | Prefetch **declared producer inputs** into RO scratch (or hardlink/erofs snapshot of CAS objects). Seal with existing sandbox. No FUSE-by-default |
| **Desktop REGISTRY** | Keep sqlite + `cas/`; materialize **views** (GUI single-file, open package, consumer workspace) via hardlinks or copy-on-read; optional future FUSE **only if** measured disk/open-latency requires it |

---

## 1. What is EdenFS

### 1.1 One-paragraph definition

**EdenFS** is a userspace virtual filesystem that presents a **full working-tree view** of a source-control repository while **fetching file and directory data lazily**. Developers (and tools) see a complete tree at a mount point; EdenFS only pulls blobs/trees from a **BackingStore** when the kernel requests them. The system is optimized for Meta-scale monorepos (millions of files) where any one engineer touches a small working set.

Primary design doc (in-tree, maintained with Sapling):  
https://github.com/facebook/sapling/blob/main/eden/fs/docs/Overview.md

### 1.2 Historical origins

| Era | Context |
|---|---|
| Pre-2014 | Facebook scales Mercurial; monorepo pain |
| ~2015+ | EdenFS development for working-copy scale |
| Parallel | Mononoke (Rust) as server; later Sapling client |
| 2018 | Windows path starts; ProjFS chosen over custom kernel driver |
| 2022 | Meta open-sources **Sapling client** (CLI); virtual FS + Mononoke promised “in the future” |
| 2023–2026 | EdenFS + Mononoke source appears under `facebook/sapling`; still **unsupported** publicly |

Meta’s public workflow description: EdenFS gives sparse-checkout-like performance with **transparent** access (no manual sparse profiles).  
https://developers.facebook.com/blog/post/2022/11/15/meta-developers-workflow-exploring-tools-used-to-code/

Sapling launch: virtual FS called out as future OSS piece.  
https://engineering.fb.com/2022/11/15/open-source/sapling-source-control-scalable/

### 1.3 Architecture overview

```
┌─────────────────────────────────────────────────────────────┐
│  User tools (editors, buck, rustc, hg/sl, watchman)         │
└───────────────┬─────────────────────────────┬───────────────┘
                │ POSIX / WinFS paths          │ Thrift (UDS)
                ▼                             ▼
        ┌───────────────┐            ┌──────────────────┐
        │ Kernel VFS    │            │ EdenServiceHandler│
        │ FUSE / NFS /  │◄──────────►│ (checkout, status,│
        │ ProjFS        │            │  glob, getSHA1…)  │
        └───────┬───────┘            └────────┬─────────┘
                │                             │
                └──────────┬──────────────────┘
                           ▼
                  ┌─────────────────┐
                  │ EdenServer      │  one daemon per user (typical)
                  │  EdenMount × N  │
                  │  ObjectStore    │
                  │  Overlay/Journal│
                  └────────┬────────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        SaplingBacking  GitBacking   LocalStore
        Store (sl/hg)   Store        (RocksDB cache)
              │
              ▼
        Mononoke / local .sl/.hg / git objects
```

Key docs:
- Process/daemon: https://github.com/facebook/sapling/blob/main/eden/fs/docs/Process_State.md  
- Data model: https://github.com/facebook/sapling/blob/main/eden/fs/docs/Data_Model.md  
- Caching: https://github.com/facebook/sapling/blob/main/eden/fs/docs/Caching.md  
- Redirections: https://github.com/facebook/sapling/blob/main/eden/fs/docs/Redirections.md  
- Windows: https://github.com/facebook/sapling/blob/main/eden/fs/docs/Windows.md  

### 1.4 OS interface: FUSE / NFS / ProjFS

| Platform | Mechanism | Notes (2026 docs) |
|---|---|---|
| **Linux** | **FUSE** | Primary design target; infinite FUSE cache expiry with explicit invalidation |
| **macOS** | **macFUSE** or **NFSv3** | Apple deprecating kexts → trajectory toward **NFS-only** |
| **Windows** | **Projected File System (ProjFS)** | Substantially different semantics; hydrated placeholders on NTFS |

Implications for nudox:
- Cross-platform “one virtual FS” is a **multi-backend project**, not a single FUSE crate.
- macOS NFS path has known limitations (invalidation, client PID visibility) called out in EdenFS macOS docs.
- Windows ProjFS “hydrates” files onto real disk — lazy **download**, not always lazy **disk occupancy** for already-touched files.

### 1.5 Daemon process model

From Process_State.md:

1. **One EdenFS daemon per user** (default), managing **multiple checkouts** (mounts).
2. **Thrift** over Unix domain socket for control/query APIs.
3. **Privhelper** process on Linux/macOS: must start elevated (setuid or sudo), then drop privileges; privhelper only mounts/unmounts.
4. State directory typically `~/.eden` or `~/local/.eden` (Windows: `C:\Users\<user>\.eden`).
5. Per-checkout state under `clients/NAME/` including:
   - `SNAPSHOT` — current commit ID
   - `local/` — **overlay** for materialized (locally modified) files
   - `config.toml` — backing repo location

Trade-off documented by Meta: shared daemon → better cache sharing across checkouts; weaker isolation if daemon wedges.

### 1.6 Data model: commits, trees, blobs

EdenFS mirrors **Git / EdenSCM object model**:

| Object | Role |
|---|---|
| **Commit** | Root tree ID + parents + metadata (EdenFS rarely walks history) |
| **Tree** | Directory: name → child object ID, mode, optional size/SHA-1 |
| **Blob** | File bytes (or symlink target bytes) |

Object IDs are **opaque 20-byte keys** in EdenFS’s model (historically SHA-1 shaped). Git and Sapling are content-addressed; EdenFS only **requires lookup by ID**.

**Parallel inode model:**
- Immutable SCM: `Tree` / `Blob` in `eden/fs/model`
- Mutable working copy: `TreeInode` / `FileInode`
- Unmodified files: inode points at SCM object ID (not on disk as normal file content until read/materialize)
- Modified files: **materialized** into **Overlay**

### 1.7 ObjectStore, BackingStore, LocalStore

| Layer | Function |
|---|---|
| **ObjectStore** | Internal fetch API (cache chain of responsibility) |
| **In-memory blob cache** | Capped LRU (historically ~40 MiB default) + min entry count |
| **LocalStore** | RocksDB on-disk cache of imported objects; `eden gc` only partially solves growth |
| **BackingStore** | Source of truth for missing objects |

Documented backing stores:
- **`SaplingBackingStore`** — primary; Sapling/Mercurial lineage
- **`GitBackingStore`** — can show a git repo view; **git CLI is not EdenFS-aware**, so `git status` / `git checkout` do not integrate cleanly

This is a critical mismatch for nudox: our system of record is **BLAKE3 per-file CAS + generation manifests**, not git/sapling object stores.

### 1.8 Checkouts, mounts, SNAPSHOT

Lifecycle (conceptual):

1. User clones / `eden clone` style workflow → EdenFS creates mount + client state.
2. `SNAPSHOT` file pins **which commit** the virtual tree should present.
3. Directory listings and file opens resolve via trees → blobs.
4. Checkout to another commit updates `SNAPSHOT` and invalidates kernel caches for the **working set**.
5. Checkout cost scales with **loaded/hydrated inodes**, not full repo size — core monorepo win.

Multiple checkouts of the same repository share ObjectStore caches when under one daemon.

### 1.9 Overlay and “materialized” files

**Overlay** = durable store of **locally modified** content under `clients/NAME/local/`.

- File is **non-materialized** iff content is fully recoverable from SCM object ID.
- On first write (or other mutative ops depending on platform), file becomes **materialized** and full contents live in overlay.
- Overlay is **per-checkout**, not global CAS.

Linux/macOS: EdenFS is source of truth; FUSE cache is a cache.  
Windows ProjFS: OS holds hydrated files on NTFS; EdenFS must **FSCK/sync** carefully; offline writes possible when daemon is down.

### 1.10 Redirections

Problem: writes through FUSE/Eden are **slower** (kernel → Eden → overlay → kernel). Build systems write **gigabytes** into `buck-out`, `target/`, etc.

**Redirections** bind real local directories over paths that should never be SCM-tracked:

- Config: `edenfsctl redirect` or repo file `.eden-redirections`
- Linux: primarily **bind mounts**; symlinks optional but weaker (`..` semantics)
- Buck auto-detects Eden and redirects `buck-out`

Nudox parallel: producers write aux outputs to **scratch**, not into sealed package trees — already the design in compiler seal/scratch paths.

### 1.11 Journal, Watchman, build integration

- **Journal** records recent modifying I/O for subscribers.
- **Watchman** integration delivers file change notifications.
- **Content hash thrift** (SHA-1 historically): Buck can get hashes **without reading file bytes** — huge monorepo build win.
- Infinite FUSE attribute/content cache expiry with **explicit invalidation** after Eden-tracked writes/checkouts.

Nudox parallel: we already have **BLAKE3 in manifests**; producers should prefer **manifest digests** over re-hashing materialize trees when possible.

### 1.12 Thrift control plane (API surface)

Documented thrift capabilities (not an exhaustive IDL dump):

| Capability | Purpose |
|---|---|
| Checkout / set revision | Change virtual tree to another commit |
| SCM status compare | Diff working copy vs commit efficiently |
| Glob queries | Filename globs without walking full disk tree |
| Get file hashes (SHA-1) | Build system integration |
| Mount management | Add/remove checkouts |

Thrift is for **Eden-aware** clients (Sapling CLI, Buck, Watchman). Ordinary tools only need the mount.

**Rust embedding angle:** thrift client is possible in theory; the **daemon + FUSE + privhelper** is the real dependency. There is no supported `edenfs-sys` crate for “mount my CAS as a tree.”

### 1.13 hg / Sapling / git integration summary

| Client | Integration quality |
|---|---|
| **Sapling (`sl`)** | First-class; EdenFS is built for Sapling checkouts |
| **Historical Mercurial** | Lineage; Meta moved to Sapling |
| **Git** | BackingStore exists; **CLI unaware** → not a drop-in VFS-for-Git replacement for third parties |
| **Microsoft VFS for Git / Scalar** | Conceptual cousin (ProjFS); Scalar/VFS protocol not broadly evolving as the industry default (sparse + partial clone won mindshare) |

---

## 2. Status in 2026

### 2.1 Open-source health

**Repository:** https://github.com/facebook/sapling  

README (current language, mid-2026):

> **EdenFS** is a virtual filesystem for efficiently checking out large repositories.  
> **Not yet supported publicly**, OSS is buildable for unsupported experimentation.

Same disclaimer for **Mononoke**.

Practical interpretation:
- Source is public and CI builds exist for experimentation.
- There is **no supported install path** for third-party product embedding.
- Breaking changes, missing packaging, Meta-internal assumptions (Buck, Watchman, Sapling server) are expected.
- License: main project **GPL-2.0** (significant for commercial embedding of daemon binaries into a product stack — legal review required even if technically feasible).

### 2.2 Sapling relationship

Sapling ecosystem (three pillars):

1. **Sapling CLI (`sl`)** — user-facing, Git-compatible workflows; **supported** enough for external try-outs; prebuilt packages (Homebrew, Windows zips). Latest release stream active in 2026 (e.g. `0.2.20260522-…` style versioning on sapling-scm.com).
2. **Mononoke** — scalable server; production at Meta; OSS experimental.
3. **EdenFS** — virtual checkout; production at Meta; OSS experimental.

Without Mononoke + EdenFS, external Sapling is essentially a **better client UX over git remotes**, not Meta monorepo scale.

### 2.3 Production use outside Meta

Public evidence of **independent production EdenFS fleets** is effectively **absent** as of this research. Discussion on HN/Lobsters treats EdenFS as Meta-internal scale tech with design docs of interest, not as a community-operated product.

Contrast:
- **git sparse-checkout + partial clone** — widely used
- **JuiceFS / Alluxio** — multi-company cloud data FS
- **composefs / EROFS** — container/OSTree ecosystems
- **EdenFS** — Meta + experimental clones

### 2.4 Platform support matrix (capability vs product-ready)

| Platform | Code exists | Product-ready for third party | Notes |
|---|---|---|---|
| Linux | Yes (FUSE) | No (unsupported) | Best documented path |
| macOS | Yes (FUSE/NFS) | No | NFS migration pressure |
| Windows | Yes (ProjFS) | No | Complex FSCK, invalidation pitfalls |

### 2.5 Installability for a third-party app (e.g. nudox)

To ship EdenFS as part of nudox you would need to:

1. Build C++ EdenFS + dependencies from Sapling monorepo (CMake, thrift, RocksDB, FUSE/NFS/ProjFS).
2. Ship setuid privhelper or require admin for mounts.
3. Run a long-lived daemon per user/machine.
4. Provide a BackingStore for **your** object model (not Sapling commits) — essentially a **fork**.
5. Handle GPL-2.0 compliance if distributing binaries.
6. Own on-call for FS corruption, FSCK, Windows offline-write races, macOS NFS quirks.
7. Accept that Meta can change internals without a stability contract.

**Conclusion:** installable as a research experiment; **not installable as a product dependency** in 2026.

### 2.6 What “2026 status” means for decision-making

EdenFS remains **architecturally relevant** (ideas, papers-in-docs form) and **operationally irrelevant** for external products that need:
- multi-version package trees (not monorepo commits),
- multi-tenant k8s,
- embeddable Rust,
- predictable OSS support.

---

## 3. Fit for nudox

### 3.1 Problem recap (from product plans)

nudox must:

1. **Store** source + IR efficiently (INDEX S3 CAS + REGISTRY disk) — Plan 13: **per-file BLAKE3**, re-parse treesitter, optional transfer packs.
2. **Materialize** trees for **producers** (compiler needs real filesystem for rustc/go/javac/oracles and sandbox binds).
3. Support **multi-version** retention with cross-version file dedupe.
4. Keep **desktop disk** under quota (LRU/pin GC).
5. Run **untrusted compiles** in k8s with sealed RO package trees + scratch.
6. Serve **consumer workspaces** (`CodebaseInput.root: PathBuf`) for resolution (§2.3).

Current-ish approach (compiler audit): daemon **materialize tempdir → peel tarball → `generate_with`**.

### 3.2 Desired EdenFS-like properties (mapped)

| Desired property | EdenFS mechanism | nudox native alternative |
|---|---|---|
| Lazy file fetch | FUSE read → BackingStore | CAS get on open / prefetch plan |
| Fast “checkout” of version | SNAPSHOT swap + invalidate | Point manifest at generation hash; rebuild view |
| Disk savings | Non-materialized inodes | Don’t write untouched files; hardlink from `cas/` |
| Content hash without read | thrift getSHA1 | Manifest already has BLAKE3 |
| Multi-checkout share cache | Shared ObjectStore | Global `cas/` is already shared |
| Build output isolation | Redirections | Scratch dirs outside sealed tree |
| Tool transparency | POSIX mount | Real dirs, or optional FUSE later |

### 3.3 Host multi-version package sources as virtual checkouts keyed by content hash

**What we want:**  
`mount(generation_hash) → tree of paths` where each path’s content is `cas[file_hash]`.

**What EdenFS wants:**  
`mount(repo, commit_id)` where objects come from Sapling/git.

Gaps:
1. **Keying:** generation_hash / BLAKE3 ≠ 20-byte SCM IDs without a full BackingStore rewrite.
2. **Multi-version:** Eden checkouts are working copies of **repos**, not a registry of 10⁶ package versions.
3. **No history need:** Eden optimizes SCM workflows (status, checkout, journal). nudox INDEX is closer to **Nix store + narinfo** or **OCI layers**.
4. **Immutability:** package generations are **sealed**; overlay/mutable WC is the wrong default (except consumer workspaces).

**Fit score:** 2/10 as product; 7/10 as inspiration for a **CAS-backed read-only tree view**.

### 3.4 Materialize only touched files for producers (lazy)

Producers (especially adaptive rustc/RA, full-package walks for CST) often **touch many or all source files**. Lazy FS helps when:

- producer only reads subset (some tools),
- dependency trees are huge but compile unit is small,
- “open package in GUI” should not download entire dep graph.

Lazy hurts when:

- first build pays serial FUSE miss latency,
- tree-walk stages (`source_archive`, CST walk) **stat/read everything**,
- hermetic sandbox needs deterministic full tree for reproducibility audits.

**Plan 13 already assumes** producers may need trees; storage remains CAS. Lazy materialize is a **scheduler policy** (“prefetch closure of package + direct sources”) more than a FS product.

**Fit:** Medium for GUI/consumer; **Low–medium** for full producer pipeline unless we split “oracle needs full tree” vs “snippet needs one file.”

### 3.5 Desktop REGISTRY disk savings

EdenFS saves disk by not writing untouched monorepo files. Desktop REGISTRY already plans:

- shared `cas/` across versions,
- LRU eviction of unpinned generations,
- optional packs for cold sync only.

If user pins 50 versions of 200 packages with high file sharing, **CAS already wins**; virtual FS adds little beyond not creating **directory entries / hardlinks** for unopened packages.

Where virtual FS could still help:
- “browse entire crate graph without materializing,”
- IDEs that walk trees aggressively.

Where it fails:
- shipping a FUSE daemon to every desktop user,
- macOS NFS/macFUSE friction,
- Windows ProjFS complexity.

**Fit:** Nice-to-have UX; **not worth EdenFS**. Prefer hardlink farms or on-demand extract.

### 3.6 Sandbox mount of sealed package trees for untrusted k8s compiles

Compiler audit: seal path uses RO binds for package tree + toolchains + optional `/nix/store`, RW scratch, net-off.

EdenFS in this setting would mean:
- FUSE device in pod / privileged or FUSE-enabled runtime,
- Eden daemon in sidecar or node agent,
- thrift + privhelper privilege story,
- multi-tenant cache poisoning risks if BackingStore shared unsafely.

Better patterns for sealed RO trees:
1. **Materialize to emptydir** from CAS (today’s direction).
2. **Hardlink** from node-local CAS cache into per-job dir (dedupe on node).
3. **erofs / composefs** image of generation → mount RO (kernel path, no userspace FS daemon).
4. **OCI layer** of package sources (if packaging that way) + overlay upper for scratch only.

**Fit:** EdenFS **anti-fit** for untrusted multi-tenant compile. Kernel RO images or plain materialize win.

### 3.7 Alignment with Plan 13 storage

Plan 13 decisions that EdenFS would **fight or ignore**:

| Plan 13 | EdenFS tension |
|---|---|
| Per-file BLAKE3 CAS SoR | Eden LocalStore/RocksDB + SCM IDs |
| Re-parse treesitter, no tree persist | Neutral |
| Transfer packs for cold sync | Eden fetches objects, not packs |
| Desktop sqlite + cas/ | Parallel state dirs `~/.eden` |
| Sync = hash set difference | Eden assumes SCM protocol |
| No git-object compatibility goal | Eden is SCM-shaped |

**Conclusion:** adopting EdenFS would **fork the storage plan**, not complete it.

---

## 4. API / embedding analysis

### 4.1 Can a Rust app drive EdenFS programmatically?

| Approach | Feasibility | Notes |
|---|---|---|
| Embed EdenFS as library | **No** | Multi-process C++ daemon architecture |
| Spawn `edenfs` + thrift client from Rust | **Possible, unsupported** | Need thrift IDL, socket lifecycle, mount privileges |
| Call `edenfsctl` subprocess | **CLI glue only** | Fragile, not embeddable product API |
| Implement custom BackingStore for BLAKE3 CAS | **Major fork** | C++ plugin surface, ongoing merge cost |
| Use only design ideas in pure Rust | **Yes** | Recommended |

### 4.2 Daemon dependency weight

Conservative third-party cost model:

| Cost center | Weight |
|---|---|
| Binary size / deps (RocksDB, thrift, FUSE) | High |
| Privilege model (privhelper setuid) | High (enterprise/desktop trust) |
| Per-user daemon ops (doctor, gc, logs) | High (support burden) |
| Platform matrix (3 backends) | Very high |
| Security review surface | Very high |
| GPL-2.0 distribution | Legal process |
| Alignment with CAS product | Negative (impedance) |

### 4.3 What thrift is good for (if you already run Eden)

- Checkout switching without full tree rewrite
- Status without full walk
- Glob + hash for build graphs

nudox already has stronger primitives for **immutable package graphs** (manifests, JobKey, ContentHash). Thrift does not add unique value once you are not Sapling.

### 4.4 Embedding recommendation

**Do not embed EdenFS.**  
If nudox ever needs a virtual FS, write a **narrow FUSE/NFS/ProjFS adapter** over `DiskCas` + generation manifests (or use composefs/erofs for RO), ~orders of magnitude smaller than EdenFS, licensed and owned by us.

---

## 5. Alternatives in the same class

Each alternative answers: *Does this solve “codebase hosting / materialization” for a multi-version package CAS registry?*

### 5.1 git sparse-checkout

**What it is:** Working tree contains only paths matching sparse rules (cone mode common).  
**Docs:** https://git-scm.com/docs/git-sparse-checkout  

| Pros | Cons |
|---|---|
| Built into git; widely understood | Still a **git repo**, not registry CAS |
| Good monorepo subset UX | Manual or scripted path sets |
| Combines with partial clone | Multi-version packages = many clones or worktrees |

**Codebase hosting?** Partial — for monorepos. **Not** for nudox INDEX multi-version CAS.

### 5.2 git partial clone (`--filter=blob:none`)

**What it is:** Clone commits/trees without blobs; fetch blobs on demand.  
**Docs:** https://git-scm.com/docs/partial-clone ; GitHub blog on sparse+partial.

| Pros | Cons |
|---|---|
| Lazy blob download with standard git | Requires smart server filter support |
| Industry default over VFS-for-Git | Working tree still materializes on checkout |
| Pairs with sparse-checkout | History/object model overhead for registry |

**Codebase hosting?** Good for **git forges**. Wrong object model for nudox generations.

### 5.3 gitoxide materialize

**What it is:** Pure Rust git implementation (`gix`); checkout/worktree features evolving rapidly through 2024–2026.  
**Repo:** https://github.com/GitoxideLabs/gitoxide  

| Pros | Cons |
|---|---|
| Embeddable in Rust | Still git |
| Fast checkout paths improving | Not a lazy VFS |
| Used in cargo-adjacent ecosystem | Past security issues in path traversal during checkout (e.g. RUSTSEC-2024-0349 class) — treat untrusted archives carefully |

**Codebase hosting?** Useful if nudox ingests **git sources** into CAS; not a host layer for CAS itself.

### 5.4 Buck2 / RE deferred materialization

**What it is:** With Remote Execution, Buck2 **avoids downloading action outputs** until a local action needs them (“deferred materialization”). Claims ~2.5× wall-time wins in Meta’s public docs.  
**Docs:** https://buck2.build/docs/users/advanced/deferred_materialization/  
RE CAS: https://buck2.build/docs/users/remote_execution/

| Pros | Cons |
|---|---|
| **Closest product cousin** to “don’t materialize until needed” | Build-system scoped, not general FS |
| Content-addressed action inputs/outputs | Requires RE/CAS service |
| Hash-first thinking | Different hash (Buck2 default SHA-256) |

**Codebase hosting?** Solves **build artifact** laziness. Pattern to copy for **compiler pods**: only materialize inputs for the sealed producer plan, not entire dep universe.

### 5.5 Nix store mounts

**What it is:** `/nix/store` paths are content-addressed (or input-addressed) immutable artifacts; bind-mount closures into sandboxes; narinfo + NAR for binary caches.  
Plan 13 already cites this as prior art.

| Pros | Cons |
|---|---|
| Proven hermetic packaging model | Full path materialize (not lazy file inside store path) |
| RO store + sandbox bind is battle-tested | Store GC and multi-tenant path secrecy need care |
| narinfo ≈ generation manifest | Not a virtual multi-file lazy tree inside one package |

**Codebase hosting?** **Excellent mental model** for INDEX/REGISTRY addressing. Not a FUSE monorepo.  
**composefs** (below) is the modern “Nix/OSTree meets overlay” refinement.

### 5.6 Docker / container volume overlays

**What it is:** Container runtimes stack image layers (overlayfs) with RW upper dir.

| Pros | Cons |
|---|---|
| Ubiquitous in k8s | Layer model ≠ per-file registry CAS |
| RO lower + RW upper matches seal/scratch | Pulling images for every package version is heavy |
| Strong isolation story | Registry duplication unless images share layers carefully |

**Codebase hosting?** Good for **toolchains and sealed environments**; awkward as primary source CAS (use CAS → materialize into emptyDir instead of rebuilding images per generation).

### 5.7 JuiceFS

**What it is:** POSIX distributed FS on object storage + metadata engine (Redis/etc.); FUSE + CSI.  
https://github.com/juicedata/juicefs  

| Pros | Cons |
|---|---|
| Strong cloud-native POSIX | Metadata service ops burden |
| Multi-client shared FS | Not content-addressed package identity |
| CSI for k8s | Wrong granularity for package generations |

**Codebase hosting?** Solves **shared dataset FS**, not **immutable package CAS**. Could back a large scratch cluster; not INDEX SoR.

### 5.8 Alluxio

**What it is:** Data orchestration / caching layer over HDFS/S3; historically big-data / AI training.  
Comparison: https://juicefs.com/docs/community/comparison/juicefs_vs_alluxio/

| Pros | Cons |
|---|---|
| Multi-tier cache, k8s CSI | Not fully POSIX-atomic like JuiceFS (per their comparison) |
| Good for large analytics datasets | Ops-heavy; not source-tree semantics |

**Codebase hosting?** No — wrong domain (data lake acceleration).

### 5.9 vfsgen

**What it is:** Go tool to generate static virtual filesystems into binaries (embed).  

| Pros | Cons |
|---|---|
| Simple static assets | Compile-time only; not multi-version CAS |
| No daemon | Cannot host evolving registry |

**Codebase hosting?** No. Mention only to exclude “vfs” naming collisions.

### 5.10 fuse-overlayfs

**What it is:** Userspace overlayfs for **rootless containers** (Podman).  
https://github.com/containers/fuse-overlayfs  

| Pros | Cons |
|---|---|
| Stack RO lower + RW upper without root | Still need lower layer populated |
| Mature in container ecosystem | FUSE overhead |

**Codebase hosting?** **Composition tool**, not storage. Useful pattern: RO package tree + RW scratch overlay for producers that insist on writing beside sources (discouraged but sometimes forced).

### 5.11 WinFsp

**What it is:** Windows user-mode FS framework (FUSE-like). EdenFS docs even list it as alternative to ProjFS.  
https://winfsp.dev/

| Pros | Cons |
|---|---|
| Practical Windows custom FS | Windows-only |
| Used by many virtual disk products | Driver install UX |

**Codebase hosting?** Platform backend if we ever ship custom VFS; not a hosting solution alone.

### 5.12 Linux overlayfs + EROFS

**EROFS:** read-only compressed Linux FS; used increasingly for container images.  
**containerd erofs snapshotter:** https://containerd.io/docs/2.1/snapshotters/erofs/  
**EROFS image FS overview:** https://erofs.docs.kernel.org/en/latest/imagefs.html  

| Pros | Cons |
|---|---|
| Kernel RO mount, no userspace daemon | Linux-centric |
| Excellent sealed-tree story for k8s | Build step to pack generation → erofs |
| Pairs with overlay upper for scratch | Not lazy network fetch by itself |

**Codebase hosting?** **Strong for sealed compile inputs** on Linux nodes once CAS objects are local.

### 5.13 composefs

**What it is:** Content-addressed file content store + EROFS-like metadata image; mount presents a tree; file bodies live in CAS (`basedir`). fs-verity integration.  
https://github.com/composefs/composefs  
LWN: https://lwn.net/Articles/933616/

| Pros | Cons |
|---|---|
| **Extremely close** to nudox “manifest + cas/” | Linux kernel/ecosystem maturity still evolving |
| Cross-tree file sharing by hash | Not macOS/Windows native |
| RO trusted mounts | Need tooling to build images from generation manifests |

**Codebase hosting?** **Best-in-class Linux mount pattern** for our CAS model — **far better fit than EdenFS**.

### 5.14 OCI mount / containers/storage

**What it is:** containerd/podman graph drivers, layer diffs, snapshotters; `containers/storage` library.

| Pros | Cons |
|---|---|
| Production container plumbing | Image/layer abstraction mismatch for fine-grained source files |
| Multi-snapshot isolation | Symlink attacks historically appear in storage stacks — audit carefully |

**Codebase hosting?** Secondary — if we ever distribute packages as OCI artifacts. Prefer raw CAS for source.

### 5.15 restic / casync (sparse restore / chunked sync)

**casync:** content-defined chunking for FS image distribution (Poettering).  
https://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html  

**restic:** encrypted dedup backups; `restore --sparse` for holey files — not the same as “sparse checkout.”  
https://restic.readthedocs.io/

| Pros | Cons |
|---|---|
| Excellent for **chunk-level** dedupe of large binaries | Plan 13: overkill for typical source files |
| casync ≈ transfer pack philosophy | Not a live virtual tree |
| restic not designed as package registry | Different trust/encryption model |

**Codebase hosting?** Transfer/backup adjacent. Aligns with **optional transfer packs**, not primary SoR.

### 5.16 Microsoft VFS for Git / ProjFS (conceptual cousin)

Historical monorepo approach for Windows; Scalar continued sparse-focused tooling. Industry momentum shifted to **partial clone + sparse-checkout**. Useful as validation that **lazy VFS is hard** (Eden Windows docs are a catalog of ProjFS footguns).

### 5.17 Alternative class summary — who actually solves codebase hosting?

| Technology | Solves monorepo WC? | Solves multi-version package CAS? | Solves sealed k8s compile tree? | Solves desktop disk? |
|---|---|---|---|---|
| EdenFS | **Yes (Meta)** | No | Weak | Partial |
| git sparse + partial | Yes (git) | No | Weak | Partial |
| gitoxide | Checkout only | No | Via materialize | Via materialize |
| Buck2 deferred mat. | Build outputs | Indirect | **Pattern yes** | N/A |
| Nix store | Closures | **Model yes** | **Yes** | Yes with GC |
| composefs/EROFS | RO trees | **Yes (with CAS)** | **Yes** | Linux only |
| JuiceFS/Alluxio | Shared FS | No | Possible but heavy | No |
| fuse-overlayfs | Overlay only | No | Composition | No |
| Docker overlay | Images | Poor | Yes env | No |
| casync/restic | Sync/backup | Partial | No | Partial |
| **nudox CAS+tmpdir** | N/A | **Yes** | **Yes** | **Yes** |
| Custom FUSE over CAS | Possible | Yes | Risky | Possible |

**Winners for nudox:**  
1) **CAS + selective materialize** (baseline)  
2) **composefs/EROFS** (Linux sealed mounts upgrade)  
3) **Buck2-style deferred materialization policy** (what to fetch)  
4) **Nix narinfo mental model** (already in Plan 13)

---

## 6. Security analysis

### 6.1 FUSE in untrusted package context

Threat model for compiler pods:

| Threat | Risk with FUSE VFS | Mitigation without FUSE |
|---|---|---|
| Malicious package triggers pathological FUSE ops | Daemon DoS, latency bombs | Materialize finite file set; cgroups |
| FUSE priv escape / device access | Needs careful runtime policy | No `/dev/fuse` in pod |
| Confused deputy via mount visibility | Cross-pod path leaks if shared | Per-job mount namespaces |
| Metadata oracle (existence of other tenants’ files) | Shared daemon caches | Per-tenant ObjectStore or no share |

**Rule:** untrusted compile **must not** depend on a shared multi-tenant userspace FS daemon.

### 6.2 Symlink attacks

Classic issues when materializing untrusted trees:

1. Symlink pointing outside tree (`../../../etc/passwd`) — producers/tools follow → read host secrets.
2. Symlink + write → overwrite host files if not sealed correctly.
3. TOCTOU races in extractors (gitoxide RUSTSEC-class path issues; containers/storage historical symlink bugs).

**Mitigations (required regardless of EdenFS):**

- Extract with **symlink rejection** or rewrite to contained targets only.
- Open files with `O_NOFOLLOW` where applicable; prefer fd-relative APIs.
- Sandbox: RO tree, no CAP_DAC_OVERRIDE, empty network, minimal mounts.
- Verify every file body against **manifest BLAKE3** after materialize.
- Never materialize as root on multi-tenant hosts.

EdenFS does not solve symlink malice; it can **present** SCM-recorded symlinks like any FS.

### 6.3 Cross-tenant leakage

| Channel | Notes |
|---|---|
| Shared CAS on node | **OK** if content-addressed and immutable; hash doesn’t leak other tenants’ names |
| Shared Eden LocalStore / RocksDB | Risk of timing + residual object presence |
| Shared FUSE daemon logs | Paths may include package names |
| Overlay leftovers | Must wipe per job |
| Hardlink farms | Safe if RO and hashed; beware link count / deletion races |

Content-addressed stores are **naturally multi-tenant-safe for payloads** (same bytes = same hash). Metadata catalogs must stay tenant-scoped.

### 6.4 Privilege and supply chain

- Eden privhelper setuid = high-value target.
- Shipping GPL daemon increases SBOM/review surface.
- ProjFS/WinFsp driver install = enterprise friction and attack surface.

### 6.5 Security posture recommendation

| Environment | Posture |
|---|---|
| INDEX | No FS virtualization; authenticated CAS GETs; optional signed manifests (Plan 13) |
| k8s compiler | Materialize verified files into job-private dir or erofs; sandbox RO; no FUSE default |
| Desktop | User-owned CAS; treat remote packages as untrusted until hash verify; careful extract |

---

## 7. Comparison matrix vs plain CAS materialize-to-tmpdir

Baseline: **Plan 13 CAS + compiler “materialize tempdir”** (current-ish).

Scoring: **5** = excellent fit for nudox need, **1** = poor.

| Dimension | CAS + tmpdir (baseline) | EdenFS | composefs/EROFS | JuiceFS | git sparse+partial | Buck2 deferred mat. |
|---|---|---|---|---|---|---|
| Multi-version package identity | **5** | 1 | **5** | 2 | 1 | 3 |
| Cross-version file dedupe | **5** | 2 | **5** | 2 | 2 | 4 |
| Single-file random access | **5** | 4 | 4 | 4 | 3 | 3 |
| Lazy network fetch | 3 (app-level) | **5** | 2 (local CAS first) | 4 | 4 | **5** |
| Producer full-tree performance | **4** | 3 (FUSE tax) | **5** | 3 | 3 | 4 |
| Desktop disk control | **5** | 3 | 3 (Linux) | 2 | 3 | N/A |
| k8s untrusted seal | **5** | 1 | **5** | 2 | 2 | 4 |
| Embed in Rust product | **5** | 1 | 3 (tooling) | 2 | 3 | 2 |
| Ops simplicity | **5** | 1 | 3 | 2 | 4 | 3 |
| macOS/Windows portability | **5** | 3 (code) / 1 (support) | 1 | 3 | **5** | 3 |
| Matches Plan 13 | **5** | 1 | **4** | 1 | 1 | 3 |
| **Weighted fit (approx)** | **Strong baseline** | **Reject** | **Upgrade path** | **Reject** | **Ingest only** | **Policy borrow** |

### 7.1 Qualitative trade study

**CAS + tmpdir advantages:**
- Deterministic: `verify(hash)` then write path.
- Easy sandbox: bind RO path.
- Simple GC: delete unreferenced cas objects + tmp.
- No kernel modules, no setuid.
- Same code path local and remote (fetch missing hashes).

**CAS + tmpdir disadvantages:**
- Full materialize cost for large packages even if tool reads 2 files (unless we implement sparse materialize).
- Desktop may create many hardlinks/directories for open projects.
- Cold start downloads whole generation unless transfer policy is smarter.

**EdenFS advantages (in its domain):**
- Transparent lazy monorepo WC.
- Checkout speed, status, watchman, hash thrift.

**EdenFS disadvantages for nudox:**
- Unsupported OSS, heavy daemon, wrong object model, GPL, multi-tenant risk, FUSE tax on full walks.

### 7.2 When baseline should grow “virtual” features

Add complexity only when metrics demand:

| Trigger metric | Feature to add |
|---|---|
| Desktop CAS > quota with many open projects | Hardlink views + aggressive pin LRU (already planned) |
| Producer waste: always unpack 50MB for 1-file CST | Sparse materialize API: `materialize(paths|globs)` |
| Node-local k8s cache thrash | Node CAS cache + hardlink into job dir |
| Many Linux jobs, RO integrity important | **composefs/erofs** generation images |
| GUI wants instant tree without download | Virtual tree listing from **manifest only**; fetch blobs on file open in app (not kernel FS) |

Note: GUI can implement “virtual FS” **inside the app** (manifest-driven file list + CAS get) without FUSE. That is usually enough.

---

## 8. Verdict and concrete recommendations

### 8.1 Global verdict

| Option | Decision |
|---|---|
| Adopt EdenFS | **No** |
| Adopt lighter virtual-FS pattern | **Optional later**, Linux-first (**composefs/EROFS** or tiny FUSE over CAS), never as day-1 requirement |
| Stick to CAS + tmpdir (+ sparse) | **Yes — primary architecture** |

**One-liner:** Steal EdenFS’s **ideas** (lazy hydrate, hash without read, redirect build outputs, shared object cache); **do not steal EdenFS**.

### 8.2 INDEX server

**Role:** durable multi-tenant store of generations (manifest + blobs).

**Do:**
1. Implement Plan 13: `GET /cas/{blake3}`, batch `has`, generation manifests (narinfo-like).
2. Optional transfer packs for cold bulk.
3. Optional signed manifests.
4. CDN-cache immutable CAS keys forever.

**Do not:**
1. Run EdenFS or any VFS on INDEX nodes.
2. Serve mutable checkouts from INDEX.
3. Expose FUSE to clients.

**Materialization:** none on server except for internal jobs (compile workers are separate).

### 8.3 k8s compiler pods

**Role:** untrusted sealed producers need a real directory tree.

**Recommended path (phased):**

**Phase A (now → near):**
1. Job receives `generation_hash` + manifest (or package tarball transitional).
2. Fetch missing CAS blobs to **node-local cache** (`/var/cache/nudox/cas` or emptyDir with optional hostPath cache).
3. **Materialize** package tree into job-private directory:
   - Prefer **hardlink** from node CAS when same FS.
   - Else copy.
4. Verify BLAKE3 for every file.
5. Sandbox RO-bind tree + RW scratch (existing cage design).
6. Prefetch policy: **all files in package generation** for producers that walk trees; optional future “sparse” for known single-file tools.

**Phase B (when Linux nodes mature):**
1. Build **erofs/composefs** image for generation (or on-demand pack from CAS).
2. Mount RO into pod; overlay upper = scratch if needed.
3. Still verify root digest.

**Never default:**
- Shared Eden daemon on node
- Privileged FUSE for untrusted jobs

**Borrow from Buck2:** deferred materialization of **dependency IR/blobs** that producers don’t need on disk (keep IR in CAS memory/API); only source trees required by toolchains hit disk.

### 8.4 Desktop REGISTRY

**Role:** offline/local sqlite + cas/, GUI browse, consumer workspace analysis.

**Recommended path:**
1. **Keep** Plan 13 layout (`registry.sqlite`, `cas/`, pins, LRU).
2. **Views, not clones:**
   - “Open package” creates `views/{generation_hash}/` as hardlink farm or copy-on-write dir from `cas/`.
   - GC views independently of cas objects.
3. **GUI virtualization without FUSE:** file tree from `generation_file` rows; load bytes via CAS on editor open; re-parse treesitter on demand (Plan 13).
4. **Consumer workspace (`CodebaseInput`):** real project path on disk is source of truth; deps resolved as IR from REGISTRY, not full source materialize unless oracle needs it.
5. **Optional future:** user-enabled FUSE/WinFsp “NudoxFS” for power users who want `/Volumes/nudox/pkg/...` — **thin**, CAS-backed, read-only, **not EdenFS**.

**Disk savings hierarchy (best → fallback):**
1. Global CAS dedupe + GC  
2. Don’t create views for unopened packages  
3. Hardlink views  
4. Sparse materialize inside a view  
5. (Last) kernel virtual FS  

### 8.5 What to implement instead of EdenFS (design sketch)

```text
struct GenerationView {
    generation_hash: ContentHash,
    root: PathBuf,                 // materialized or mount root
    mode: ViewMode,                // HardlinkFarm | Copy | ErOfs | FuseLazy
}

trait Materializer {
    /// Ensure paths exist under root; fetch CAS as needed; verify hashes.
    fn ensure(&self, gen: &Manifest, paths: PathSet) -> Result<()>;
    /// Full tree for producers that walk everything.
    fn ensure_all(&self, gen: &Manifest) -> Result<()>;
    fn drop_view(&self, view: GenerationView) -> Result<()>;
}

enum PathSet {
    All,
    Prefixes(Vec<PathBuf>),
    Exact(Vec<PathBuf>),
}
```

Wire into:
- `compiler_daemon` materialize step (replace blind tarball peel when CAS path is ready)
- registry GUI open-package
- optional k8s init container

### 8.6 Explicit non-goals (reaffirmed)

- Do not make Sapling/Eden a product dependency.
- Do not require users to install macFUSE/WinFsp for core features.
- Do not store source primarily as git packs in INDEX.
- Do not persist tree-sitter Trees (orthogonal; Plan 13).

---

## 9. Detailed EdenFS component glossary (for readers of Meta docs)

| Term | Meaning |
|---|---|
| **Checkout** | Virtual working directory mount managed by Eden |
| **Repository / backing repo** | Where objects are fetched from |
| **ObjectStore** | Cache + fetch façade |
| **BackingStore** | Sapling/Git (etc.) object source |
| **LocalStore** | RocksDB persistent object cache |
| **Overlay** | Storage for materialized/modified files |
| **Journal** | Recent mutation log for watchers |
| **Materialized** | File contents live in overlay (not pure SCM pointer) |
| **Redirection** | Bind-mount escape hatch for write-heavy dirs |
| **Privhelper** | Privileged mount helper |
| **SNAPSHOT** | Current commit id file |
| **Working set** | Inodes/placeholders OS currently knows about |
| **Hydrated placeholder** | (Windows) ProjFS file with content on NTFS |

---

## 10. Mapping EdenFS workflows to nudox workflows

| EdenFS / Sapling workflow | nudox analogue |
|---|---|
| `clone` large monorepo | `sync generation` hash set difference |
| lazy file open | CAS get on demand |
| `checkout` commit | switch `generation_hash` pointer / rebuild view |
| `status` | N/A for sealed packages; consumer WC uses ordinary FS or future journal |
| `eden redirect buck-out` | producer scratch + sandbox RW |
| thrift getSHA1 | read digest from manifest |
| Watchman | tantivy/GUI watchers on real views if needed |
| Shared ObjectStore across checkouts | global `cas/` |
| `eden doctor` / FSCK | `nudox cas verify` + view rebuild |

---

## 11. Implementation roadmap (nudox-facing, Eden-inspired)

### Phase 0 — Document invariants (done in Plan 13 + this doc)
- CAS key = blake3(raw)
- Manifest lists paths
- Materialize verifies

### Phase 1 — Materializer trait (1 week)
- Replace/augment tarball peel with manifest+CAS materialize
- Hardlink when possible
- Metrics: files touched vs files in package

### Phase 2 — Prefetch policies (1 week)
- `All` for full producers
- `Exact/Prefix` for GUI and specialized tools
- Parallel fetch with concurrency limits

### Phase 3 — Node-local CAS cache for k8s (1–2 weeks)
- hostPath or cache volume
- Quota + GC
- Job isolation for views

### Phase 4 — Optional Linux RO images (optional)
- composefs/erofs builder from generation
- Pod mount path
- Compare wall time vs hardlink farm

### Phase 5 — Optional desktop FUSE (only if metrics demand)
- Read-only NudoxFS
- No write/overlay complexity
- Feature-flagged, non-default

**No phase includes EdenFS adoption.**

---

## 12. Risk register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Someone proposes EdenFS as “free monorepo FS” | Medium | High schedule sink | This document; unsupported OSS status |
| Full materialize too slow for huge packages | Medium | Medium | Sparse ensure + parallel CAS + packs |
| FUSE desktop support tickets | High if shipped | High | Don’t ship FUSE default |
| Symlink escape in extract | Medium | Critical | Strict extract; sandbox; verify |
| Node cache cross-tenant paranoia | Low | Medium | Hash-only sharing; no path metadata in shared store |
| composefs immature on target distro | Medium | Low | Keep hardlink path forever |
| GPL contamination via accidental Eden link | Low | High | Do not vendor Eden |

---

## 13. Open questions

1. **Producer touch fraction:** For each language producer, what % of package files are read on a typical compile? (Determines value of lazy materialize.)
2. **Node cache economics:** Is hostPath CAS cache allowed on the target k8s distro/security policy?
3. **Hardlink across volumes:** Do desktop and pod layouts guarantee same filesystem for `cas/` and views?
4. **composefs availability:** Minimum Linux kernel/distro for compiler fleet?
5. **Windows desktop:** Is hardlink farm + copy enough, or is WinFsp ever required for UX parity?
6. **GUI-only virtualization:** Can the app avoid all kernel mounts forever by streaming CAS into the editor buffer?
7. **Transfer pack vs lazy:** For first-time open of popular packages, is one seekable pack faster than N small GETs? (Plan 13 already flags this.)
8. **Consumer oracle tier:** Which oracles require full dependency **source** trees vs IR-only? (Affects workspace materialize scope.)
9. **Legal:** Any interest in Sapling CLI as a *user* tool separate from Eden? (Out of scope for storage, but product may care.)
10. **Benchmark plan:** Define golden packages (small crate, huge generated sources, monorepo-style tree) and measure materialize strategies.

---

## 14. References (primary)

### EdenFS / Sapling
- https://github.com/facebook/sapling  
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Overview.md  
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Process_State.md  
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Data_Model.md  
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Caching.md  
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Redirections.md  
- https://github.com/facebook/sapling/blob/main/eden/fs/docs/Windows.md  
- https://engineering.fb.com/2022/11/15/open-source/sapling-source-control-scalable/  
- https://developers.facebook.com/blog/post/2022/11/15/meta-developers-workflow-exploring-tools-used-to-code/  
- https://sapling-scm.com/docs/introduction/installation/  

### Git sparse / partial
- https://git-scm.com/docs/git-sparse-checkout  
- https://git-scm.com/docs/partial-clone  
- https://github.blog/open-source/git/bring-your-monorepo-down-to-size-with-sparse-checkout/  

### Buck2
- https://buck2.build/docs/users/advanced/deferred_materialization/  
- https://buck2.build/docs/users/remote_execution/  
- https://engineering.fb.com/2023/04/06/open-source/buck2-open-source-large-scale-build-system/  

### Nix / composefs / EROFS
- https://nix.dev/manual/nix/2.26/ (store / content-address)  
- https://github.com/composefs/composefs  
- https://lwn.net/Articles/933616/  
- https://erofs.docs.kernel.org/en/latest/imagefs.html  
- https://containerd.io/docs/2.1/snapshotters/erofs/  

### JuiceFS / Alluxio
- https://github.com/juicedata/juicefs  
- https://juicefs.com/docs/community/comparison/juicefs_vs_alluxio/  

### Overlay / containers
- https://github.com/containers/fuse-overlayfs  
- https://winfsp.dev/  

### casync / restic
- https://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html  
- https://restic.readthedocs.io/  

### gitoxide
- https://github.com/GitoxideLabs/gitoxide  

### Local plans
- `.research/librarification/13-storage/PLAN.md`  
- `.research/librarification/01-compiler-audit/PLAN.md`  
- `.research/resolution/03-exhaustive-plan.md`  

---

## 15. Appendix A — EdenFS OS request path (Linux simplified)

```
open("src/lib.rs")
  → kernel VFS
  → FUSE upcall to EdenFS
  → resolve FileInode
  → if blob cached in memory: return slices
  → else ObjectStore → LocalStore (RocksDB)
  → else BackingStore (Sapling/Git) network/disk
  → populate blob cache; respond FUSE READ
  → kernel page cache retains with infinite timeout until Eden invalidates
```

Write path (non-redirected):

```
write()
  → FUSE → EdenFS
  → materialize into Overlay on local disk
  → journal entry
  → FUSE response
  (extra kernel round-trips vs native FS)
```

### Appendix B — Windows ProjFS simplified

```
first open → PRJ_GET_PLACEHOLDER_INFO → placeholder on NTFS
first read → PRJ_GET_FILE_DATA → hydrate content to NTFS
later reads → served by NTFS without Eden
writes → NTFS then notify Eden (cannot deny)
checkout → invalidate placeholders (can fail if handles open)
startup → FSCK every time (offline modifications)
```

### Appendix C — nudox materialize path (target)

```
manifest = load(generation_hash)
for entry in select(manifest, PathSet):
    bytes = cas.get(entry.content_hash)  // local or INDEX
    assert blake3(bytes) == entry.content_hash
    place(view_root / entry.path, bytes) // hardlink or write
sandbox.ro_bind(view_root)
sandbox.rw_bind(scratch)
run_producer()
drop_view() // or retain if pinned
```

### Appendix D — Decision record (ADR-style)

**Title:** Reject EdenFS for nudox source materialization  
**Status:** Accepted (research 2026-07-16)  
**Context:** Need efficient multi-version source hosting and producer trees.  
**Decision:** Do not adopt EdenFS; implement CAS-native materializer; consider composefs/EROFS later on Linux.  
**Consequences:** No FUSE daemon dependency; must implement sparse/prefetch ourselves; monorepo-scale transparent WC is out of scope.

### Appendix E — “What we learned that is still gold”

1. **Laziness is a policy**, not necessarily a kernel FS.  
2. **Hash-first build integration** beats re-reading files (we have BLAKE3 manifests — use them).  
3. **Redirect write-heavy outputs** off the sealed tree (we have scratch).  
4. **Shared content cache across checkouts/versions** is the big disk win (global CAS).  
5. **Working-set-aware invalidation** matters if you virtualize; full-tree producers destroy the working-set advantage.  
6. **Platform virtualization backends are 3× the work** of a single Linux FUSE prototype.  
7. **Unsupported Meta OSS** is research gold, product poison.

### Appendix F — FAQ

**Q: Meta open-sourced the code — why not run it?**  
A: Unsupported, SCM-shaped, heavy, GPL, multi-OS landmines, poor multi-tenant story.

**Q: Isn’t lazy VFS the future of all large trees?**  
A: For interactive monorepos, maybe. For content-addressed package registries, **manifest + CAS + selective materialize** is the future (Nix, OCI, composefs, Buck CAS).

**Q: Could EdenFS BackingStore speak S3 BLAKE3?**  
A: Only with a major fork; still leaves daemon/FUSE/ops costs.

**Q: Does rejecting EdenFS reject virtualization forever?**  
A: No. App-level virtualization and composefs-class RO mounts remain on the table.

**Q: What about VFS for Git?**  
A: Same class as Eden; industry shifted to sparse+partial for third parties.

**Q: Should INDEX store erofs images?**  
A: Optional derivative artifact, never replace per-file CAS (dedupe + single-file access).

---

## 16. Final recommendations (copy-ready)

### For engineering leadership
- **Reject EdenFS** as a product dependency in 2026.  
- **Double down** on Plan 13 CAS architecture.  
- Invest in a small **Materializer** component with hardlink + verify.  
- Revisit **composefs/EROFS** only for Linux compiler fleets after baseline metrics.

### For INDEX
- Serve manifests + CAS (+ packs). No mounts.

### For compiler pods
- Prefetch + materialize sealed trees into job dirs; sandbox as today.  
- Node-local CAS cache.  
- No FUSE-by-default.

### For desktop REGISTRY
- sqlite + cas/ + pin/LRU.  
- Hardlink views for open packages.  
- GUI reads CAS without kernel VFS.  
- Optional future RO NudoxFS behind a flag — **not Eden**.

### For research follow-ups
- Benchmark materialize strategies on golden packages.  
- Measure producer file-touch fractions.  
- Spike composefs image build from generation manifest (Linux only).  
- Do **not** spike EdenFS beyond reading docs unless a strategic monorepo SCM decision appears (out of scope).

---

## 17. Summary table for the edge-tech series

| ID | Tech | Nudox take |
|---|---|---|
| 03 | **EdenFS** | **Reject for product; harvest patterns** |
| — | composefs/EROFS | **Promising Linux upgrade** |
| — | Buck2 deferred mat. | **Policy inspiration** |
| — | JuiceFS/Alluxio | **Out of domain** |
| — | git sparse/partial | **Ingest/forge only** |
| — | CAS+tmpdir | **Adopt / keep** |

---

*End of research note. Dual path: `.research/edge-tech/03-edenfs.md` and `.research/edge-tech/03-edenfs/PLAN.md` (identical).*
