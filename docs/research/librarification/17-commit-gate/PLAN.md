# Commit-Gate & Desktop Git Integration for nudox

**Research date:** 2026-07-16  
**Scope:** Generation identity; desktop commit detection; dirty-tree policy; file→package mapping; multi-root/monorepo/submodules; trusted compile vs untrusted INDEX; generations schema & REGISTRY GC; `CommitGate`/`Generation`/`DirtySet` APIs; prior art (Sourcegraph gitserver, rust-analyzer VFS, Cursor Merkle, SCIP, GitHub Blackbird).  
**Gap filled:** [08-incremental](../08-incremental/PLAN.md) declares *git is the Merkle* and sketches gix tree-diff → SymbolDelta, but is thin on desktop integration, multi-root, dirty state, GUI triggers, and ecosystem package mapping. This report is that missing layer.  
**Companions:** [14-client-sync](../14-client-sync/PLAN.md) (local REGISTRY vs remote INDEX), [15-gui-references](../15-gui-references/PLAN.md) (Jobs / project panel), [13-storage](../13-storage/PLAN.md) (CAS + generation tables), [06-symbol-identity-industrial](../06-symbol-identity-industrial/PLAN.md) (gix renames).  
**Live code anchors:** `workspace/compiler/compile/vcs.rs` (gix clone/fetch/materialize/revwalk), `workspace/heart/content.rs` (`ContentHash`), `workspace/compiler/generate/` (source archive + generation stamp).

---

## 0. Problem statement and non-goals

### 0.1 Product requirement (authoritative)

The whole librarification pipeline must be **heavily incremental** and must generate embeddings / index updates **only for symbols changed on a COMMITTED change**.

| Event | Durable pipeline? | Notes |
|---|---|---|
| File save / keystroke | **No** | rust-analyzer territory; not nudox REGISTRY |
| Dirty working tree | **No** (auto) | Optional explicit preview only |
| `git commit` (any parent graph) | **Yes** | Primary gate |
| `git amend` / rebase / force-push | **Yes** (re-evaluate lineage) | Same OID replace or new OIDs; see §1 |
| Branch checkout (no new commit) | **Maybe** | Switch active generation pointer; no re-embed if already sealed |
| Dep lockfile change on commit | **INDEX sync**, not local recompile of untrusted | See §6 |
| Explicit "Index now" | Optional **ephemeral** generation | Non-durable id; never confusable with commit OID |

### 0.2 What 08 already settled (do not re-litigate)

From [08-incremental](../08-incremental/PLAN.md):

1. Constructive-trace spine + early cutoff at **symbol content hash** (L3).
2. Salsa is **producer-local only**, not the orchestrator.
3. `git` tree is the Merkle; inventing a second working-tree Merkle for the **durable** path is wrong.
4. gix tree-to-tree diff → dirty files → over-approx packages → producers → `SymbolDelta`.
5. SQLite `symbol_heads` + `stage_traces` + `generations` sketch.

### 0.3 What this report adds

| Gap | Section |
|---|---|
| Exact generation identity (OID + worktree + submodule policy) | §1 |
| How the GUI **detects** commits on desktop | §2 |
| Dirty-tree policy + product UX | §3 |
| File → package mapping across ecosystems | §4 |
| Multi-root, nested repos, monorepos, submodules | §5 |
| Trusted local compile vs INDEX refresh on commit | §6 |
| Schema, parent links, GC survival | §7 |
| Concrete Rust API sketches | §8 |
| Prior art contrast table | §9 |

### 0.4 Non-goals

- Implementing the pipeline (research only; no build).
- Keystroke-level LSP intelligence (delegate to editors / rust-analyzer).
- Remote k8s fleet scheduling details (see orchestration research).
- Full moniker / lineage edge semantics (05/06).

---

## 1. Generation identity

### 1.1 Core identity tuple

A **durable** generation for a trusted project root is keyed by:

```text
GenerationKey {
  project_id:      ProjectId,        // stable local UUID for the opened root
  repo_id:         RepoId,           // content id of (normalized remote URL | local path digest)
  commit_oid:      ObjectId,         // 20-byte git SHA-1 (or SHA-256 when repos migrate)
  tree_oid:        ObjectId,         // commit.tree — defensive; catches empty commits
  // optional components — policy flags, not always part of the primary key:
  worktree_id:     Option<WorktreeId>,
  submodule_set:   SubmodulePinSet,  // sorted list of (path, commit_oid)
}
```

**Primary durable id (recommended):**

```text
durable_generation_id = BLAKE3(
  "nudox.gen.v1" ‖
  project_id ‖
  repo_id ‖
  commit_oid ‖
  encode(submodule_set)   // empty if policy = ignore
)
```

Rationale:

- **`commit_oid` alone is insufficient** across multi-root workspaces (two projects can share a monorepo commit while being different trust planes) and across nested repos.
- **`tree_oid` alone is insufficient** for lineage (two commits can share a tree with different parents / messages; lineage cares about commit graph).
- Including **`submodule_set`** only when policy = `PinSubmodules` (§1.5). Default v1: **ignore nested submodule contents** unless the user adds them as separate project roots.

Align with existing registry dual-hash pattern ([14 §2.6](../14-client-sync/PLAN.md)): generation-stamp ≠ CAS key of IR blobs. Commit OID is the **source identity**; IR `BlobManifest.generation` stamp remains content-addressed over produced artifacts.

### 1.2 HEAD states

| HEAD state | `commit_oid` resolution | Pipeline behavior |
|---|---|---|
| **Attached branch** (`ref: refs/heads/main`) | Peel HEAD → commit | Normal; record `branch_name` as advisory metadata only |
| **Detached HEAD** | Peel HEAD → commit | **Allowed.** Generation is still commit-keyed. UI shows "detached @ abc1234". Do **not** refuse to index. |
| **Unborn branch** (no commits yet) | None | No durable generation; UI "make first commit" |
| **Corrupt / missing objects** | Error | Do not invent synthetic ids; surface `GitError` |

**Branch names are never part of the generation primary key.** Two branches at the same commit share one sealed generation. Branch tip movement without commit change is a no-op for embeddings.

### 1.3 Worktrees

Git worktrees (`git worktree add`) share an object database but have independent HEAD and index.

| Policy option | Behavior | Recommendation |
|---|---|---|
| **A. One project = one worktree path** | Each opened path is its own `project_id`; generations keyed by that path's HEAD | **v1 default** |
| **B. Collapse by repo_id** | All worktrees of same repo share generation store by commit | Correct for CAS dedup; UI must not mix dirty state |
| **C. Encode worktree_id in key** | Separate generations even for same commit | Wasteful; reject |

**v1:** Policy A for *project open*, Policy B for *CAS / stage_traces* (content-addressed, already shared by hash). The `generations` row is per `(project_id, commit_oid)` so each worktree has its own "active HEAD" pointer, but sealed IR/embeds for commit C are shared via `stage_traces` and CAS.

Live code already materializes arbitrary commits into a destination directory without mutating the user's worktree (`materialize_commit` in `workspace/compiler/compile/vcs.rs:420-461`). Desktop pipeline should **prefer reading blobs from the ODB by tree walk** for pure producers, and only materialize when a language toolchain requires a real FS (rustc, go build, etc.).

### 1.4 Amend, rebase, force-push — lineage implications

| Operation | Commit graph effect | Generation store effect |
|---|---|---|
| **`git commit --amend`** | Old OID *O* replaced by *N*; *O* may become unreachable | Seal generation *N*. Keep *O* until GC if still referenced by history UI / bookmarks. Parent link: prefer `N.parents[0]` (often *O*'s parent, not *O*). |
| **Interactive rebase** | Many OIDs rewritten | Each new tip commit is a new generation. Parent selection: **first-parent of the new tip**, or nearest sealed ancestor in the **new** graph reachable by first-parent walk (see §1.6). |
| **`git push --force`** | Remote history rewrite | Desktop only cares about **local** ODB. Remote INDEX for *published packages* is separate (server re-indexes published versions). |
| **Reset hard to ancestor** | HEAD moves backward | Active generation pointer moves; do not delete newer sealed gens immediately (user may checkout again). |
| **Cherry-pick / revert** | New commit, unusual parentage | Treat as normal new commit; tree-diff vs chosen parent generation. |
| **Empty commit** | New OID, same tree | Tree diff empty → **empty SymbolDelta** → seal cheap no-op generation (still record for UI timeline). |

**Critical rule:** Generation identity is **immutable once sealed**. Amending does not mutate generation *O*; it creates *N*. Lineage edges (symbol level, research 06) connect symbols across *O*→*N* via moniker/hash/rename signals, not by rewriting history.

### 1.5 Submodule policy

| Policy | Meaning | When |
|---|---|---|
| **`Ignore` (v1 default)** | Tree entries of type `commit` (gitlink) appear in tree-diff only as "pin changed"; **do not** recurse into submodule ODB | Most app repos |
| **`PinOnly`** | Record `(path, submodule_commit_oid)` in generation metadata; still no recurse | Want lock fidelity for docs |
| **`RecursiveProject`** | Auto-open each submodule as a nested `RepoBinding` with its own CommitGate | Power users / superrepos |
| **`VendorAsFiles`** | If submodule is checked out, treat working files as normal paths under parent (breaks pure-commit purity) | **Forbidden** for durable path |

Detection: gix tree entry mode `Commit` / gitlink (mode `160000`). Parent commit change of a gitlink ⇒ `DirtyKind::SubmodulePin { path, old_oid, new_oid }`. Under `Ignore`, this does **not** dirty packages inside the submodule; under `RecursiveProject`, enqueue that submodule's CommitGate.

### 1.6 Parent commit selection for tree-diff (the load-bearing algorithm)

Given new HEAD commit `C`, choose parent generation `P` for incremental diff:

```text
fn choose_parent(C, store) -> Option<SealedGeneration>:
  // 1. Prefer first-parent if sealed
  for p in C.parents():  // git order; first-parent is parents[0]
    if let Some(g) = store.sealed_for_commit(project, p):
      return Some(g)

  // 2. Nearest sealed ancestor on first-parent spine (bounded walk)
  for anc in first_parent_walk(C).take(256):
    if let Some(g) = store.sealed_for_commit(project, anc):
      return Some(g)

  // 3. Any sealed ancestor (merge-base with last active branch tip) — optional
  if let Some(prev) = store.last_active_generation(project):
    if let Some(base) = merge_base(C, prev.commit_oid):
      if let Some(g) = store.sealed_for_commit(project, base):
        return Some(g)

  // 4. Full rebuild
  return None
```

**Merge commits:** Diff against **first parent only** for the primary incremental path (same as `git show` default / GitHub PR "Files changed" first-parent convention). Optionally compute a second diagnostic dirty set vs second parent for UI, but do not double-embed.

**Orphan / unrelated histories:** Full rebuild. Metrics: `generation_full_rebuild_total`.

### 1.7 Repo identity (`repo_id`)

```text
fn repo_id(repo: &gix::Repository, open_path: &Path) -> RepoId:
  if let Some(url) = primary_fetch_url(repo):  // prefer origin
    return RepoId::Remote(normalize_git_url(url))
  return RepoId::Local(BLAKE3(canonical_path(open_path)))
```

**URL normalization** (must be stable):

- Lowercase host for github.com / gitlab.com
- Strip `.git` suffix
- Prefer `https://host/org/repo` over SSH form for equality (`git@host:org/repo.git` → same)
- Drop credentials / trailing slashes

Live precedent: `repository_matches_remote` in `vcs.rs:335-345` compares configured fetch URL strings — **tighten** for desktop with a shared normalizer so the same GitHub repo cloned via SSH vs HTTPS shares `repo_id`.

### 1.8 Ephemeral (non-durable) generation ids

For "Preview uncommitted" / "Index now":

```text
ephemeral_id = BLAKE3(
  "nudox.ephemeral.v1" ‖
  project_id ‖
  base_commit_oid ‖          // dirty base
  worktree_fingerprint       // see §3
)
```

**Hard rules:**

1. Ephemeral ids **never** written to REGISTRY package generations that can sync / publish.
2. Separate tables: `ephemeral_generations` vs `generations`.
3. GC on process exit or TTL (e.g. 24h) or explicit dismiss.
4. UI badge: **PREVIEW** — never confusable with commit timeline.

---

## 2. How the GUI detects commits

### 2.1 Design goals

| Goal | Target |
|---|---|
| Latency from `git commit` complete → pipeline enqueue | **< 2s** p95 on local SSD |
| False negatives (missed commits) | ~0 (bounded by watcher reliability + poll fallback) |
| CPU when idle | Near zero; no full-tree hash loops |
| Works when hooks cannot be installed | Yes (managed repos, sandbox) |
| Works offline | Yes (all local) |
| Multi-root | Independent watchers per `RepoBinding` |

### 2.2 Strategy matrix

| Strategy | Mechanism | Pros | Cons | v1 role |
|---|---|---|---|---|
| **A. Watch `.git` via `notify`** | FSEvents (macOS), inotify (Linux), ReadDirectoryChangesW (Windows) on `GIT_DIR` | No repo config; catches GUI clients (Tower, GitHub Desktop, JetBrains) | Event storms during fetch; need debounce; some editors pack aggressively | **Primary** |
| **B. Poll `HEAD` + `refs/` mtimes / OIDs** | 1–5s timer: read `HEAD`, peel, compare to last seen | Simple; recovers missed events | Latency up to poll period; wakeups | **Fallback + reconcile** |
| **C. `core.hooksPath` helper / `post-commit` + `post-rewrite` + `reference-transaction`** | Install nudox hook scripts or `gix-hook` discovery | Low latency for CLI git | **Cannot install** in many corporate repos; misses GUI tools that bypass hooks; security sensitivity | **Optional accelerator** |
| **D. `gix` continuous watch API** | (none production-grade as of 2026 for "watch refs") | — | gitoxide has **hooks discovery** (`gix-hook` status) but **not** an FS watch loop | Do not depend |
| **E. Git `fsmonitor-watchman` / built-in FSMonitor** | Speeds `git status` | Helps dirty detection, not commit seal | Orthogonal | Optional later for dirty UI |
| **F. Shell out `git rev-parse HEAD`** | Subprocess | Easy debug | Process spam; prefer gix | Avoid hot path |

**Recommendation:** **A + B**. Optional C for power users who opt into "Install nudox git hooks".

Sources:

- [notify-rs/notify](https://github.com/notify-rs/notify) — FSEvents / inotify / ReadDirectoryChangesW
- [notify-debouncer-full](https://docs.rs/notify-debouncer-full) — rename stitching, debounce
- [githooks](https://git-scm.com/docs/githooks) — `post-commit`, `post-rewrite`, `reference-transaction`
- [gitoxide crate-status — gix-hook](https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md) — hook discovery/execution plumbing

### 2.3 What to watch under `.git`

```text
GIT_DIR/
  HEAD                          # branch switch, detach, unborn
  refs/heads/**                 # new commits, resets
  refs/tags/**                  # optional; usually ignore for pipeline
  packed-refs                   # repack collapses loose refs
  reflogs (optional)            # noisy; skip
  objects/pack/*.pack           # fetch/clone; means refs may change soon
  commondir / worktrees/*       # linked worktrees (see gitdir files)
```

For **linked worktrees**, `gitdir: ...` in `.git` file points at `main/.git/worktrees/<name>/`. Watch **both** the worktree gitdir (HEAD) and the shared object store's `refs/` + `packed-refs`.

**Ignore** for pipeline triggers:

- `index`, `index.lock` (staging only — dirty UI, not durable pipeline)
- `objects/tmp_*`, `*.lock` files (use lock-aware debounce)
- `FETCH_HEAD`, `ORIG_HEAD` alone without HEAD change (fetch without merge)

### 2.4 Debounce and batch algorithm

```text
on_fs_event(paths):
  mark_dirty_git_meta(paths)
  schedule_reconcile(debounce = 300ms)   // coalesce burst

on_reconcile():
  for binding in project.repo_bindings:
    head = peel_head(binding.repo)       // gix
    if head == binding.last_seen_oid:
      continue
    // Coalesce rapid rewrites (rebase): wait extra quiet period
    if events_in_last(2s) > threshold:
      schedule_reconcile(debounce = 1s)
      continue
    binding.last_seen_oid = head
    emit CommitObserved { binding, commit: head, prev: old }
```

| Parameter | Default | Rationale |
|---|---|---|
| Meta debounce | **300 ms** | FSEvents batches; lock files settle |
| Rewrite quiet period | **1–2 s** if >N ref events | Interactive rebase fires many ref updates |
| Poll interval | **5 s** | Safety net; also when watcher unavailable |
| Offline | No change | All local; "offline" only affects INDEX sync (14) |

### 2.5 Event types emitted to the pipeline

```rust
pub enum GateEvent {
    /// Durable: HEAD peeled to a new commit OID.
    CommitObserved {
        repo: RepoId,
        project: ProjectId,
        commit: ObjectId,
        previous: Option<ObjectId>,
        branch: Option<String>, // advisory
        source: ObserveSource,  // Watcher | Poll | Hook | Manual
    },
    /// Active branch tip moved to an already-sealed commit (checkout).
    CheckoutSealed {
        project: ProjectId,
        commit: ObjectId,
        generation: GenerationId,
    },
    /// Working tree / index dirty bit flipped (UI only).
    DirtyStateChanged {
        project: ProjectId,
        dirty: DirtySnapshot,
    },
    /// Repo vanished, permissions lost, etc.
    RepoFault {
        project: ProjectId,
        error: GateError,
    },
}
```

GUI ([15 Jobs panel](../15-gui-references/PLAN.md)): `CommitObserved` → JobStore `index project @ abc123f`.

### 2.6 Hook helper design (optional)

If user enables hooks:

```text
# $GIT_DIR/hooks/post-commit  (or core.hooksPath/nudox/*)
#!/bin/sh
# Prefer unix socket / named pipe to running lindsey process:
nudox-gate-notify post-commit "$(git rev-parse HEAD)"
```

Also install:

- `post-rewrite` — amend/rebase (stdin lists old→new)
- `post-checkout` — update active generation pointer
- `reference-transaction` (committed) — low-level ref updates ([githooks](https://git-scm.com/docs/githooks))

**Security:** never run arbitrary hook content from the repo as elevating path; the helper is a fixed binary shipped with nudox that only pings the app. Do not set `core.hooksPath` to a remote-controlled directory.

### 2.7 Interaction with concurrent pipeline runs

```text
CommitObserved(C1) → start job J1
CommitObserved(C2) while J1 running:
  if C2 is descendant of C1 (fast-forward spine):
    mark J1 as "superseded after seal" OR cancel J1 if still in producer phase
  else:
    queue J2; single-flight per (project_id) — only one active durable job
```

**Single-flight key:** `project_id` for durable jobs (not global). Ephemeral preview jobs use a separate lane and are always cancellable.

Reuse `heart::cache::SingleFlight` pattern from [14](../14-client-sync/PLAN.md).

### 2.8 App lifecycle

| Moment | Action |
|---|---|
| Project open | Open gix repo; peel HEAD; if no sealed gen for HEAD → enqueue initial index; start watchers |
| App resume (macOS) | Force poll reconcile (FSEvents may drop while suspended) |
| Project close | Drop watcher; abort ephemeral; leave durable jobs option to finish background |
| First-run no git | Offer "initialize git" or "unversioned folder" mode — **unversioned = ephemeral-only** or refuse durable pipeline (product choice; recommend **require git for durable**) |

---

## 3. Dirty working tree policy

### 3.1 Hard product rule (v1)

> **Never** auto-run the durable embed / tantivy / Terminus / REGISTRY publish pipeline on uncommitted changes.

This is the user's stated requirement and aligns with 08 R8.

### 3.2 What "dirty" means

```rust
pub struct DirtySnapshot {
    pub head_commit: Option<ObjectId>,
    pub index_dirty: bool,      // staged vs HEAD
    pub worktree_dirty: bool,   // unstaged vs index
    pub untracked_count: u32,   // optional; expensive if full enumerate
    pub conflicted: bool,       // merge/rebase in progress
    pub fingerprint: ContentHash, // see below
}
```

**Fingerprint for ephemeral generations** (when user opts in):

```text
worktree_fingerprint = BLAKE3(
  sorted(
    for each path in (tracked_diff ∪ explicitly_included_untracked):
      path ‖ blob_or_worktree_hash(path)
  )
)
```

Use gix status / index comparison; prefer **not** hashing ignored build artifacts (honor `.gitignore`).

### 3.3 UI surfaces ([15](../15-gui-references/PLAN.md) alignment)

| Surface | Dirty behavior |
|---|---|
| Project panel | Badge: `main @ a1b2c3d · 3 local changes` |
| Jobs / pipeline stepper | Pipeline shows last **sealed** commit; not dirty |
| Omni-search | Default scope = last sealed generation |
| Symbol page | Banner if file has local edits not in index: "Showing committed abc1234" |
| Explicit actions | **"Index uncommitted preview…"** / **"Index now"** in command palette |

### 3.4 Optional product features (explicit only)

#### 3.4.1 "Index now" (snapshot)

1. User confirms.
2. Build ephemeral generation from worktree fingerprint.
3. Run producers on dirty packages only (or full if no base).
4. Write to **ephemeral** tantivy segment / vector namespace.
5. Search toggle: `scope=Preview`.
6. On next durable commit, drop or ignore ephemeral automatically.

#### 3.4.2 "Preview uncommitted" (lighter)

Same as above but **skip embeddings** (Tantivy + IR only) to keep cost low.

#### 3.4.3 Partial editor buffer index

**Out of scope v1.** That is LSP. Do not bridge dirty buffers into REGISTRY.

### 3.5 Merge / rebase in progress

| State | Durable pipeline | Preview |
|---|---|---|
| `MERGE_HEAD` present | Pause auto CommitObserved application until merge completes (still record commits if any) | Disabled or warn |
| Interactive rebase | Debounce (§2.4); index final tip only | Disabled |
| Conflicted index | No ephemeral index | Show conflicts count |

### 3.6 Contrast: rust-analyzer vs nudox

| | rust-analyzer | nudox durable |
|---|---|---|
| Trigger | Keystroke / VFS change | Commit seal |
| Persistence | Session-only ([issue #4712](https://github.com/rust-lang/rust-analyzer/issues/4712)) | SQLite + CAS across restarts |
| Consistency | Soft; best-effort mid-edit | Atomic generation seal |
| Granularity | ItemTree / body queries | Symbol content hash stages |

Sources: [RA architecture](https://rust-analyzer.github.io/book/contributing/architecture.html), [VFS blog](https://rust-analyzer.github.io/blog/2020/05/18/next-few-years.html), [durable incrementality](https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html).

---

## 4. File → package mapping after tree-diff

### 4.1 Pipeline position

```text
gix tree-diff(parent, C) → Vec<PathChange>
    → PackageMapper::map(changes) → DirtySet
    → producers for each dirty package (trusted)
    → SymbolDelta
```

**Correctness rule (08 R9):** over-approximate packages; never under-approximate.

### 4.2 `PathChange` (file-level)

```rust
pub struct PathChange {
    pub path: RepoRelativePath,      // UTF-8, `/` separated, no leading `./`
    pub kind: ChangeKind,            // Add | Delete | Modify | Rename { from }
    pub old_blob: Option<ObjectId>,
    pub new_blob: Option<ObjectId>,
    pub entry_mode: EntryMode,       // blob / executable / symlink / gitlink
}

pub enum EntryMode {
    File,
    Executable,
    Symlink,
    Gitlink, // submodule pin
    Tree,    // rare in path list; usually expanded
}
```

**File-level early cutoff:** if `old_blob == new_blob` and not a rename, drop (gix may still report mode-only changes — treat mode-only as Modify if executable bit affects build, else ignore for IR).

### 4.3 Package identity (trusted local)

```rust
pub struct LocalPackageRef {
    pub ecosystem: Ecosystem,
    pub name: String,                 // manifest name
    pub manifest_path: RepoRelativePath,
    pub root: RepoRelativePath,       // package root directory
    pub workspace_root: Option<RepoRelativePath>,
}
```

### 4.4 Ecosystem rules

#### 4.4.1 Rust / Cargo

| Signal | Package mapping |
|---|---|
| `Cargo.toml` changed | That package dirty; if virtual workspace, dirty **member discovery** (re-read workspace) |
| File under package root | Owning package = nearest ancestor `Cargo.toml` with `[package]` (not virtual-only) |
| `[workspace.members]` changed | Re-resolve all members; dirty removed members as **Removed packages** |
| `Cargo.lock` changed | Does **not** dirty trusted local packages' IR by itself; may dirty **dep set** for INDEX sync (§6) |
| `build.rs` / `include!` / `env!` | Over-approx: treat whole package dirty (correctness) |
| Path dependency outside repo | Separate `RepoBinding` or warn unsupported |

**Workspace discovery:** parse root and member manifests (live compiler tests already write workspace Cargo.toml layouts). Cache `path → package` map per sealed commit; invalidate map entries when any `Cargo.toml` in the ancestor chain changes.

#### 4.4.2 JavaScript / TypeScript (npm, pnpm, yarn, bun)

| Signal | Mapping |
|---|---|
| Nearest `package.json` | Package root (workspace protocol: also read `pnpm-workspace.yaml` / `package.json#workspaces` / `lerna.json`) |
| `package.json` exports / types change | That package dirty |
| `tsconfig.json` project references | Dirty this project + **downstream** references that path-depend on it (over-approx graph) |
| `package-lock.json` / `pnpm-lock.yaml` / `yarn.lock` | INDEX dep set refresh, not local package IR (unless package is private workspace) |
| Source under `src/` | Nearest package.json |

Live code: `workspace/compiler/compile/typescript/oxc/entry.rs` already resolves from `package.json` exports.

#### 4.4.3 Go

| Signal | Mapping |
|---|---|
| Module = directory of `go.mod` | All `.go` files under module (excluding other modules) |
| File in package dir | Dirty **Go package** (directory); producer may re-typecheck whole module |
| `go.mod` / `go.sum` | Module dirty + INDEX deps |
| Nested modules | Nearest `go.mod` wins |

#### 4.4.4 Python

| Signal | Mapping |
|---|---|
| `pyproject.toml` / `setup.cfg` / `setup.py` | Project root |
| Src layout vs flat | Configure roots from `[tool.setuptools.packages]` / hatch / poetry |
| Namespace packages | Over-approx to project root |
| `requirements.txt` / lock | INDEX deps |

#### 4.4.5 Java / Kotlin (Maven / Gradle)

| Signal | Mapping |
|---|---|
| Nearest `pom.xml` | Maven module |
| `build.gradle(.kts)` + `settings.gradle(.kts)` | Gradle project; multi-project from settings |
| `src/main/java` … | Owning module |

#### 4.4.6 C / C++ (weak v1)

| Signal | Mapping |
|---|---|
| Compile commands / cmake | Optional later |
| Heuristic: top-level project | Whole repo as one package if no manifest | Accept coarse dirty |

#### 4.4.7 Multi-language monorepo

One `RepoBinding` has **many** `LocalPackageRef`s across ecosystems. A path maps to **at most one** package per ecosystem mapper; a path might theoretically match both (rare) — pick most specific (deepest root).

### 4.5 Mapper algorithm

```text
fn map_files_to_packages(tree_at_C, changes, cache) -> DirtySet:
  ensure_package_index(tree_at_C, cache)  // scan manifests if cache miss

  dirty: Set<LocalPackageRef>
  removed_packages: Set<LocalPackageRef>
  dep_lock_changed: bool
  submodule_pins: Vec<...>

  for ch in changes:
    if ch.entry_mode == Gitlink:
      submodule_pins.push(...); continue
    if is_lockfile(ch.path):
      dep_lock_changed = true; continue
    if is_manifest(ch.path):
      // reparse; may add/remove packages
      reparse_manifests(ch, &mut dirty, &mut removed_packages)
    if let Some(pkg) = cache.owning_package(ch.path):
      dirty.insert(pkg)
    else if is_source_like(ch.path):
      // unknown ownership — dirty all packages whose root prefixes this path's ancestors
      dirty.extend(cache.packages_possibly_affected(ch.path))

  // Workspace-level manifest edits already expanded members

  return DirtySet { dirty_packages: dirty, removed_packages, dep_lock_changed, submodule_pins, files: changes }
```

### 4.6 Caching the path → package index

```sql
CREATE TABLE package_index (
  project_id     BLOB NOT NULL,
  commit_oid     BLOB NOT NULL,
  package_key    TEXT NOT NULL,  -- ecosystem + name + manifest_path
  root_path      TEXT NOT NULL,
  manifest_path  TEXT NOT NULL,
  ecosystem      TEXT NOT NULL,
  PRIMARY KEY (project_id, commit_oid, package_key)
);

CREATE TABLE path_prefix_index (
  project_id  BLOB NOT NULL,
  commit_oid  BLOB NOT NULL,
  prefix      TEXT NOT NULL,      -- package root
  package_key TEXT NOT NULL,
  PRIMARY KEY (project_id, commit_oid, prefix)
);
```

Build once per sealed commit; for parent→child incremental update, only re-walk manifests that changed + reassign paths under affected prefixes.

### 4.7 Rename handling

If gix rewrite tracking reports `Rename { from, to }`:

1. Both paths map to packages (possibly different after move).
2. Dirty **both** old and new packages.
3. Feed rename to symbol lineage layer (06) as file-level evidence — **not** sufficient alone for symbol identity.

Sources: [gix::diff](https://docs.rs/gix/latest/gix/diff/index.html), [gix-diff rewrites](https://lib.rs/crates/gix-diff), 06 industrial identity §9.

---

## 5. Multi-root, monorepo, nested repos, submodules

### 5.1 Vocabulary

```rust
pub struct Project {
    pub id: ProjectId,
    pub display_root: PathBuf,           // what user opened
    pub bindings: Vec<RepoBinding>,      // ≥1
    pub trust: TrustBoundary,            // 14/16
}

pub struct RepoBinding {
    pub repo_id: RepoId,
    pub workdir: PathBuf,                // worktree root
    pub git_dir: PathBuf,
    pub gate: CommitGateHandle,
    pub package_index_head: Option<ObjectId>,
    pub submodule_policy: SubmodulePolicy,
}
```

### 5.2 Topologies

| Topology | Detection | Handling |
|---|---|---|
| **Single repo root** | `gix::open(display_root)` ok | One binding |
| **Monorepo multi-package** | One git root; many manifests | One binding; many `LocalPackageRef` |
| **Multi-root workspace** (user adds folders) | Each path may be distinct git | N bindings; N gates; unified search across sealed gens |
| **Nested repo** (inner `.git`) | Walk parents; also scan children carefully | **Inner is separate RepoBinding**; paths under inner not attributed to outer packages |
| **Git submodule** | gitlink + `.gitmodules` | Policy §1.5 |
| **Subtree / vendor dir** | Normal files | Normal mapping (content is owned by parent) |
| **Bare repo** | No worktree | Support read-only index via ODB only; rare for desktop |
| **Non-git folder** | open fails | Ephemeral-only or prompt `git init` |

### 5.3 Discovering git roots

```text
fn discover_bindings(open_path) -> Vec<RepoBinding>:
  // 1. Outermost: walk parents for .git
  if let Some(repo) = find_git_upwards(open_path):
     primary = binding(repo)

  // 2. Nested: optional scan (depth-limited) for child .git
  //    Default: do NOT auto-scan entire monorepo for nested clones (expensive / surprising)
  //    Enable if .gitmodules present → list submodules

  // 3. Multi-root: user-added paths each run discover independently
```

**Nested repo pitfall:** outer tree-diff may show the inner directory as a single gitlink **or** as ignored content. If someone cloned a repo inside a monorepo without submodule metadata, outer git may see untracked files. Policy: if inner `.git` exists, **exclude** that path prefix from outer package mapping.

### 5.4 Monorepo scale switches

| Package count | Strategy |
|---|---|
| < 50 | Full package index in SQLite; fine |
| 50–500 | Prefix trie in memory per project; lazy manifest parse |
| 500+ (mega-monorepo) | Require path filters / "index only these globs"; still commit-gated |

Cursor indexes whole workspaces with Merkle sync ([Cursor blog](https://cursor.com/blog/secure-codebase-indexing)); nudox differs by **symbol IR + commit gate**, but still needs monorepo path filters for CPU.

### 5.5 Multi-root search sessions

From 14: `DepSet` pins dependency generations. For multi-root **trusted** projects:

```text
ProjectSession = {
  trusted: Vec<(LocalPackageRef, GenerationId)>,  // per package at each binding's HEAD
  delegated: DepSet,                              // INDEX
}
```

A commit on root A does **not** re-produce packages on root B.

### 5.6 Shared object databases / partial clones

- **Partial clone / sparse checkout:** tree-diff only sees present paths; missing blobs must not crash — mark package `IncompleteSource` and skip or fetch.
- **LFS:** treat LFS pointers as content; if real blobs needed for producer, smudge or skip binary.

---

## 6. Trusted local compile vs untrusted INDEX refresh

### 6.1 Two planes on the same commit

When the user commits on a project:

```text
Commit C on trusted project
├── Plane T (Trusted): local producers for dirty LocalPackageRefs
│     → IR / tantivy / embeds / lineage for *first-party* code
└── Plane U (Untrusted): dependency resolution delta
      → if lockfile / manifest dep edges changed:
            recompute DepSet → SyncEngine.ensure (14)
      → do NOT re-embed all third-party packages locally
```

| Input change on commit | Trusted local | INDEX / sync |
|---|---|---|
| First-party source | Re-produce dirty packages | No |
| First-party manifest version | Re-produce; update local package coordinates | Optional publish later |
| Lockfile only | No local IR change | **Yes** — refresh delegated set |
| Manifest dep bump | May change which path-deps are trusted | Yes for registry deps |
| Submodule pin | Per policy | If submodule is delegated content, sync |

### 6.2 Trust boundary types (align 14)

```rust
// Trusted: CompileLocally exists
// Untrusted: only delegate() to INDEX
```

Commit gate **must not** call `compile_locally` on untrusted coordinates even if sources are in `vendor/` unless user explicitly **trusts** that path (dangerous; confirm in UI per 15).

### 6.3 `Cargo.lock` / lockfile-only commits

Common case: dependabot bumps transitive deps.

```text
DirtySet.dep_lock_changed = true
DirtySet.dirty_packages = {}  // maybe empty

→ skip local producer fan-out
→ run dependency resolver at commit C
→ diff old DepSet vs new DepSet
→ SyncEngine.ensure(new) + optionally GC unreferenced delegated gens
→ search session migrates to new DepSet when user accepts ("Deps updated")
```

Do **not** silently change mid-session DepSet (14 §3.3 generation pin).

### 6.4 Path dependencies

| Path dep location | Trust | On commit |
|---|---|---|
| Inside same repo | Trusted (workspace member) | Local produce if dirty |
| Sibling directory outside repo | Separate binding or warning | If other binding commits, its own gate |
| Git dep in Cargo.toml | Untrusted | INDEX / fetch materialize on server story |

### 6.5 When local HEAD commit does not change deps

Most application commits: Plane U is a **no-op**. Metrics should show `dep_set_unchanged_total` high. Avoid waking SyncEngine.

---

## 7. Schema: generations, parents, REGISTRY GC

### 7.1 Desktop REGISTRY tables (extension of 08 + 13)

```sql
-- One row per sealed or in-flight durable generation for a project binding.
CREATE TABLE generations (
  id              INTEGER PRIMARY KEY,          -- local monotonic
  generation_uid  BLOB    NOT NULL UNIQUE,      -- durable_generation_id (§1.1)
  project_id      BLOB    NOT NULL,
  repo_id         BLOB    NOT NULL,
  commit_oid      BLOB    NOT NULL,              -- 20 or 32 bytes
  tree_oid        BLOB    NOT NULL,
  parent_id       INTEGER REFERENCES generations(id),
  parent_commit   BLOB,                         -- denorm for debug
  branch_name     TEXT,                         -- advisory at seal time
  status          TEXT    NOT NULL,             -- running|sealed|failed|superseded
  kind            TEXT    NOT NULL DEFAULT 'durable', -- durable|ephemeral
  full_rebuild    INTEGER NOT NULL DEFAULT 0,
  created_at      INTEGER NOT NULL,
  sealed_at       INTEGER,
  stats_json      TEXT                          -- files_dirty, symbols_*, timings
);
CREATE UNIQUE INDEX generations_project_commit
  ON generations(project_id, commit_oid) WHERE kind = 'durable';
CREATE INDEX generations_repo_commit ON generations(repo_id, commit_oid);

-- Active HEAD pointer for UI / search default scope
CREATE TABLE project_active (
  project_id      BLOB PRIMARY KEY,
  generation_id   INTEGER REFERENCES generations(id),
  head_commit     BLOB,
  dirty_flag      INTEGER NOT NULL DEFAULT 0,
  updated_at      INTEGER NOT NULL
);

-- Submodule pins recorded at seal (PinOnly / Recursive)
CREATE TABLE generation_submodules (
  generation_id   INTEGER NOT NULL REFERENCES generations(id),
  path            TEXT    NOT NULL,
  commit_oid      BLOB    NOT NULL,
  PRIMARY KEY (generation_id, path)
);

-- symbol_heads, stage_traces, file_blobs: as in 08 §8.4
-- package_index: as in §4.6

-- Ephemeral isolation
CREATE TABLE ephemeral_generations (
  id              INTEGER PRIMARY KEY,
  ephemeral_uid   BLOB NOT NULL UNIQUE,
  project_id      BLOB NOT NULL,
  base_commit     BLOB NOT NULL,
  fingerprint     BLOB NOT NULL,
  status          TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  expires_at      INTEGER NOT NULL
);
```

### 7.2 Parent links semantics

- `parent_id` points at the generation used for **tree-diff and SymbolDelta**, not necessarily `commit.parents[0]` if that parent was never sealed (we may have walked to an older sealed ancestor).
- Store `parent_commit` for forensics.
- Multiple children can share one parent (branching). Schema allows **DAG of generations**, not only a line.
- `UNIQUE(project_id, commit_oid)` ensures one durable row per commit per project even if two branches point at it.

### 7.3 What survives REGISTRY GC

| Artifact | GC policy |
|---|---|
| CAS blobs (IR, source slices, embeds) keyed by content hash | Survive if **any** remaining generation_file / stage_traces / pin references hash |
| `stage_traces` | Survive across commits (constructive cache); GC LRU by `created_at` if disk budget |
| `symbol_heads` for generation G | Drop when generation G dropped |
| `generations` sealed rows | Keep: HEAD, branch tips user viewed, user pins, last N=50, all in last 14 days |
| Ephemeral | Always evictable |
| Tantivy segments | Generation-scoped segments merged; drop segments only referenced by GC'd gens |
| Delegated INDEX packages | Per 14 DepSet pins + LRU |

**Pin levels** (align 13 storage sketch):

```text
0 evictable
1 recently used
2 branch tip / HEAD
3 user pin
4 active search session
```

### 7.4 Server INDEX vs desktop project generations

| | Desktop project gen | INDEX package gen |
|---|---|---|
| Key | project + commit_oid | package coordinates + generation-stamp |
| Trigger | Local CommitGate | Publish / CI / fleet compile |
| CAS | Local DiskCas | Object store |
| Overlap | Same IR hash ⇒ shared embed bytes if ever synced | Content-addressed reuse |

Local project commits for private code **do not** automatically create INDEX rows.

### 7.5 Crash recovery

1. Rows `status=running` older than TTL → mark `failed` or restart if `commit_oid == HEAD`.
2. Staging symbol tables not promoted → discard on restart.
3. Idempotent `run_stage` via `stage_traces` PRIMARY KEY (08).

---

## 8. Concrete API sketches

### 8.1 Module layout (proposed)

```text
workspace/
  heart/           # ContentHash, ObjectId newtype?, SingleFlight
  gitgate/         # NEW: CommitGate, watchers, DirtySet, PackageMapper traits
  compiler/compile/vcs.rs  # existing gix plumbing — reuse materialize/open
  client/          # wires gate events → producers → REGISTRY
  gui/             # subscribes to GateEvent via client
```

Pragmatic v1: put types in `heart` or `client` modules without a new crate if packaging cost hurts; keep **modules** pure.

### 8.2 Object and generation types

```rust
use std::path::{Path, PathBuf};
use heart::content::ContentHash;

/// Git object id (SHA-1 20 bytes today; SHA-256-ready).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ObjectId([u8; 32]); // store width 32; high bytes zero for sha1

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ProjectId(pub uuid::Uuid);

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RepoId {
    Remote(String), // normalized
    Local(ContentHash),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GenerationId(pub i64); // local sqlite row id

#[derive(Clone, Debug)]
pub struct Generation {
    pub id: GenerationId,
    pub uid: ContentHash,           // durable_generation_id
    pub project: ProjectId,
    pub repo: RepoId,
    pub commit: ObjectId,
    pub tree: ObjectId,
    pub parent: Option<GenerationId>,
    pub status: GenerationStatus,
    pub kind: GenerationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationStatus { Running, Sealed, Failed, Superseded }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationKind { Durable, Ephemeral }
```

### 8.3 DirtySet

```rust
#[derive(Clone, Debug, Default)]
pub struct DirtySet {
    pub files: Vec<PathChange>,
    pub dirty_packages: Vec<LocalPackageRef>,
    pub removed_packages: Vec<LocalPackageRef>,
    pub dep_lock_changed: bool,
    pub submodule_pins: Vec<SubmodulePinChange>,
    pub full_rebuild: bool,
}

impl DirtySet {
    pub fn is_empty(&self) -> bool {
        !self.full_rebuild
            && self.dirty_packages.is_empty()
            && self.removed_packages.is_empty()
            && !self.dep_lock_changed
            && self.submodule_pins.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct SubmodulePinChange {
    pub path: String,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
}
```

### 8.4 PackageMapper trait

```rust
pub trait PackageMapper: Send + Sync {
    fn ecosystem(&self) -> Ecosystem;

    /// Build or refresh path ownership for the tree at `commit`.
    fn reindex(
        &self,
        repo: &gix::Repository,
        commit: ObjectId,
    ) -> Result<PackageIndex, MapError>;

    /// Map a file delta into package dirtiness. May over-approximate.
    fn map_changes(
        &self,
        index: &PackageIndex,
        changes: &[PathChange],
    ) -> Result<DirtySet, MapError>;
}

pub struct CompositePackageMapper {
    pub mappers: Vec<Box<dyn PackageMapper>>,
}

impl CompositePackageMapper {
    pub fn map_all(&self, index: &PackageIndex, changes: &[PathChange]) -> Result<DirtySet, MapError> {
        let mut out = DirtySet::default();
        for m in &self.mappers {
            out.merge(m.map_changes(index, changes)?);
        }
        Ok(out)
    }
}
```

### 8.5 CommitGate trait

```rust
pub trait CommitGate: Send + Sync {
    /// Current peeled HEAD, if any.
    fn head(&self) -> Result<Option<ObjectId>, GateError>;

    /// Observe source of truth and emit events (non-blocking start).
    fn start(&self, sink: GateSink) -> Result<(), GateError>;

    fn stop(&self);

    /// Force reconcile (app resume, user Refresh).
    fn poll_now(&self) -> Result<Option<GateEvent>, GateError>;

    /// Tree-diff helper used by pipeline.
    fn changed_paths(
        &self,
        old: ObjectId,
        new: ObjectId,
    ) -> Result<Vec<PathChange>, GateError>;

    /// Working tree dirty snapshot for UI / ephemeral.
    fn dirty_snapshot(&self) -> Result<DirtySnapshot, GateError>;
}

pub type GateSink = Box<dyn Fn(GateEvent) + Send + Sync>;
```

### 8.6 gix-backed implementation sketch

```rust
pub struct GixCommitGate {
    project: ProjectId,
    repo_id: RepoId,
    repo: gix::Repository,
    last_head: std::sync::Mutex<Option<ObjectId>>,
    // notify watcher join handle, debounce state, …
}

impl GixCommitGate {
    pub fn open(project: ProjectId, workdir: &Path) -> Result<Self, GateError> {
        let repo = gix::open(workdir).map_err(...)?;
        let repo_id = compute_repo_id(&repo, workdir)?;
        Ok(Self { project, repo_id, repo, last_head: Mutex::new(None) })
    }
}

impl CommitGate for GixCommitGate {
    fn changed_paths(&self, old: ObjectId, new: ObjectId) -> Result<Vec<PathChange>, GateError> {
        let old_tree = self.repo.find_commit(old_to_gix(old))?.tree_id()?;
        let new_tree = self.repo.find_commit(new_to_gix(new))?.tree_id()?;
        // gix_diff::tree::Changes / for_each — record Add/Delete/Modify by blob OID
        // optional: tree_with_rewrites for renames (06)
        todo!("see 08 §7.2 sketch")
    }
    // ...
}
```

Reuse existing:

- `open_or_clone_repository` / `materialize_commit` / revwalk from `vcs.rs` for **registry package** acquisition; desktop gate uses `gix::open` on the **user's** worktree (no clone wipe).

### 8.7 Pipeline orchestration hook

```rust
pub async fn on_commit_observed(
    evt: GateEvent,
    store: &GenerationStore,
    mapper: &CompositePackageMapper,
    pipeline: &IncrementalPipeline,
    sync: &SyncEngine,
) -> Result<(), PipelineError> {
    let GateEvent::CommitObserved { project, commit, previous, .. } = evt else {
        return handle_non_commit(evt, store).await;
    };

    if store.sealed_uid(project, commit)?.is_some() {
        store.set_active(project, commit)?;
        return Ok(()); // checkout of known gen
    }

    let parent = choose_parent_generation(store, project, commit)?;
    let gen = store.begin_durable(project, commit, parent)?;

    let dirty = if let Some(p) = parent {
        let paths = gate.changed_paths(p.commit, commit)?;
        let idx = mapper.ensure_index(commit)?;
        mapper.map_all(&idx, &paths)?
    } else {
        DirtySet { full_rebuild: true, ..Default::default() }
    };

    if dirty.dep_lock_changed {
        let dep_delta = resolve_dep_set_at(commit).await?;
        sync.ensure_background(&dep_delta.new_set);
        // session migration is UX-driven
    }

    if dirty.full_rebuild || !dirty.dirty_packages.is_empty() || !dirty.removed_packages.is_empty() {
        pipeline.produce_and_fanout(gen, dirty).await?;
    } else {
        store.seal_empty(gen)?; // lock-only or empty commit
    }

    store.set_active(project, commit)?;
    Ok(())
}
```

### 8.8 GUI-facing store API

```rust
impl ProjectStore {
    pub fn active_generation(&self) -> Option<Generation>;
    pub fn is_worktree_dirty(&self) -> bool;
    pub fn request_ephemeral_preview(&self, mode: PreviewMode) -> JobId;
    pub fn cancel_ephemeral(&self, job: JobId);
}

pub enum PreviewMode {
    IrAndSearchOnly,
    FullIncludingEmbeds,
}
```

---

## 9. Prior art

### 9.1 Sourcegraph gitserver + commit graph

**Architecture:** Sourcegraph caches all code hosts' repos in **gitserver** (sharded); **repo-updater** keeps them eventually consistent with the host. Code intel is separated from pure git storage.

Sources:

- [Sourcegraph architecture (code syncing)](https://github.com/nmpowell/sourcegraph/blob/main/doc/dev/background-information/architecture/index.md)
- [Optimizing a code intelligence commit graph (part 1)](https://www.eric-fritz.com/articles/optimizing-commit-graph-part-1/)
- [Part 2 (Sourcegraph blog)](https://sourcegraph.com/blog/optimizing-a-code-intelligence-commit-graph-part-2)

**Lessons for nudox:**

| Sourcegraph | nudox desktop |
|---|---|
| gitserver = durable bare clones | User's local clone is source of truth; no separate gitserver for v1 |
| Commit graph in Postgres for nearest-indexed-commit queries | `generations` DAG + `choose_parent` |
| SCIP/LSIF upload may lag HEAD; queries **fall back** to nearest indexed ancestor | Same: search can serve last sealed while new job runs |
| UploadMeta shadowing by commit distance | Active generation pointer + optional "index coverage" UI |
| Auto-indexing clones into executors | Local trusted compile in-process; untrusted on INDEX fleet |

**Do not** port gitserver to desktop. **Do** port the **nearest sealed ancestor** serving model.

### 9.2 SCIP upload per commit

Sources:

- [Announcing SCIP](https://sourcegraph.com/blog/announcing-scip)
- [Auto-indexing](https://sourcegraph.com/blog/announcing-auto-indexing)
- [Cross-repository code navigation (2026)](https://sourcegraph.com/blog/cross-repository-code-navigation)
- [scip-python upload guidance](https://sourcegraph.com/github.com/sourcegraph/scip-python)

**Model:** Indexer produces a **full snapshot** SCIP for repo@commit (or subdirectory root). CI uploads after build; or executors auto-index. **No standard delta SCIP** in production (08 also notes this).

**Contrast with nudox:**

| SCIP upload | nudox CommitGate |
|---|---|
| Whole index replace per commit | SymbolDelta incremental fan-out |
| Often CI / executor | Desktop local + server fleet |
| Precise nav primary | Docs + search + lineage + embeds |
| Root + indexer identity for shadowing | `project_id` + tool_digest in stage_traces |

Borrow: **commit as unit of precise index publication**; nearest-ancestor fallback UX.

### 9.3 rust-analyzer VFS + change loop

Sources:

- [Next Few Years — VFS](https://rust-analyzer.github.io/blog/2020/05/18/next-few-years.html)
- [Architecture](https://rust-analyzer.github.io/book/contributing/architecture.html)
- [files.watcher config](https://rust-analyzer.github.io/book/configuration.html)
- [salsa durability](https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html)

**Model:** VFS applies **transactional** file changes (editor buffers + disk watch). Salsa revision bumps; red-green revalidate. Client or server file watcher (`files.watcher`).

**Contrast:**

| Dimension | RA | nudox CommitGate |
|---|---|---|
| Latency target | ms after keystroke | seconds after commit |
| Snapshot | Soft mid-edit | Hard commit OID |
| Persistence | No | Yes |
| Dirty | Continuous | Explicit / ignored for durable |

**Borrow:** transactional apply of a change set (our generation seal); durability tiers idea (stdlib vs user) maps to **delegated deps vs trusted packages**.

### 9.4 Cursor Merkle sync

Source: [Securely indexing large codebases](https://cursor.com/blog/secure-codebase-indexing) (2026-01-27).

**Model:** Client builds Merkle tree over **working files** (respect ignore rules); syncs hashes with server; embeds changed syntactic chunks; caches embeddings by chunk content; team index reuse via simhash + content proofs.

**Contrast:**

| Cursor | nudox |
|---|---|
| Working tree / continuous | **Commit gate** for durable |
| File/chunk granularity | **Symbol IR** granularity |
| Server VDB | Local + INDEX hybrid (14) |
| Invents Merkle over FS | **Git tree is Merkle** |
| Minutes-scale resync (third-party reports) | Event-driven on commit |

**Borrow:** chunk/content-hash embed cache (equals our stage_traces on symbol hash); ignore rules; optional ephemeral FS Merkle **only** for preview feature.

### 9.5 GitHub code search (Blackbird)

Source: [Technology behind GitHub's new code search](https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/).

**Model:** Shard by **git blob OID**; push events → Kafka; tree diff for new blobs; content-addressed dedup across repos; consistent commit view.

**Borrow:** blob OID short-circuit at file layer; commit-consistent search serving; never serve partial push.

### 9.6 Zoekt delta builds

Source: [zoekt design](https://github.com/sourcegraph/zoekt/blob/main/doc/design.md), delta discussion in 08.

File-level delta shards + tombstones. nudox tantivy batching should mirror **commit-batch** deletes/adds, not per-keystroke.

### 9.7 Summary contrast table

| System | Trigger | Merkle | Unit of work | Persistence | Desktop? |
|---|---|---|---|---|---|
| rust-analyzer | Keystroke / VFS | No (revision counter) | Query memo | Session | Yes |
| Cursor | FS / periodic | FS Merkle | Chunk embed | Server | Yes |
| Zoekt | Clone/fetch | Git | File shard | Disk shards | Server |
| SCIP/SG | CI / auto-index | Git commit | Repo snapshot | DB | Server |
| Blackbird | Push | Git blob | Blob | Global index | Server |
| **nudox** | **CommitGate** | **Git tree** | **Symbol hash stages** | **SQLite+CAS** | **Yes + server** |

---

## 10. Interaction with live `workspace/` gix code

| Existing API (`vcs.rs`) | CommitGate use |
|---|---|
| `open_or_clone_repository` | INDEX/package acquisition — **not** user project open (too wipe-happy on remote mismatch) |
| `gix::open` path | **User project** gate open |
| `fetch_remote_updates` | Optional "fetch + index remote branch" later; not v1 gate |
| `materialize_commit` | When toolchain needs FS; prefer ODB streaming for pure parse |
| `find_commit_with_extractor` | Registry version→commit resolution; orthogonal to desktop HEAD gate |
| `version_search_start_points` | Multi-branch historical index — future "index all tags" |

**Gap:** no tree-diff helper, no status/dirty, no watcher. Those belong in new `gitgate` module reusing gix types.

Note: research 06 claimed gix not in workspace — **stale**. As of 2026-07-16, `workspace/compiler/compile/vcs.rs` is fully on gix.

---

## 11. Recommendations

### R1 — Durable generation key = project + repo + commit (+ optional submodule pins)

Never key solely by branch name. Detached HEAD is first-class.

### R2 — Detect commits with `notify` on `GIT_DIR` + 5s poll reconcile

Optional hooks as accelerator only. Debounce 300ms; rewrite quiet period 1–2s.

### R3 — Hard-forbid auto pipeline on dirty worktree

Ship "Preview uncommitted" as explicit ephemeral feature with separate ids and GC.

### R4 — Parent selection = sealed first-parent / nearest sealed ancestor / full rebuild

Serve search from last sealed while new job runs (Sourcegraph nearest-index model).

### R5 — Composite PackageMapper per ecosystem; over-approx; cache per commit

Lockfiles set `dep_lock_changed` without dirtying first-party packages.

### R6 — Split trusted produce vs INDEX DepSet refresh on every commit

Most commits no-op the sync engine.

### R7 — Schema: `generations` DAG + `project_active` + ephemeral tables

`stage_traces` and CAS survive GC; per-generation heads do not.

### R8 — Implement `CommitGate` + `DirtySet` + `PackageMapper` in a dedicated module

Reuse `vcs.rs` carefully; do not wipe user clones.

### R9 — Multi-root = multiple RepoBindings; nested `.git` = separate binding

Default submodule policy = Ignore (pin change recorded only).

### R10 — Metrics

`commit_to_enqueue_ms`, `debounce_coalesced`, `full_rebuild_total`, `empty_delta_total`, `dep_set_unchanged_total`, `watcher_fault_total`, `ephemeral_started`.

### R11 — Require git for durable project intelligence in v1

Unversioned folders → ephemeral only or guided `git init`.

### R12 — Align Jobs UI (15) with GateEvent

Show `index @ commit` and dirty badge separately.

---

## 12. Risks and open questions

| Item | Risk | Options |
|---|---|---|
| SHA-1 vs SHA-256 git object ids | Mixed repos mid-migration | `ObjectId` 32-byte wide; store algorithm tag |
| Hook resistance in enterprise | Missed installs | Watcher+poll must be sufficient alone |
| FSEvents drops on macOS sleep | Missed commits | Resume poll; compare HEAD |
| Mega-monorepos | Mapper / produce cost | Path include globs; package denylist |
| Submodule recursive indexing | Surprise CPU / secrets | Default Ignore; explicit add |
| Amend lineage UX | Users expect "same commit" | Timeline shows replace; keep old OID until GC |
| Worktree fingerprint cost | Ephemeral slow | Limit to tracked diffs; ignore untracked by default |
| Nested repo false ownership | Wrong package dirty | Always detect child `.git` under changed paths |
| Multi-root DepSet | Search scope confusion | Session explicitly lists roots |
| gix rename parity vs git | Different rename sources | Threshold tests (06) |
| Whether empty commits create generations | Timeline noise | Yes seal empty (cheap) for HEAD fidelity |
| Unborn branch / no git | Product block | Decide onboarding flow |
| Concurrent external `git gc` | Transient object missing | Retry; mark fault |
| Partial clone missing blobs | Incomplete produce | Fetch blob or skip package with error |

**Open design choices:**

1. Exact URL normalization table for `RepoId::Remote`.
2. Should checkout of already-sealed commit re-run dep resolution? (Recommend no if lockfile blob unchanged.)
3. Ephemeral embeds: on by default in "Index now" or search-only default?
4. Store generation per **package** inside monorepo vs per **repo commit**? (Recommend **per repo commit** with dirty package subset — one seal, many package IR hashes.)
5. Integration with planned 16-trust-project-model typestates — wire `Trusted` sources only into produce path.
6. Windows watcher reliability + antivirus lock delays — may need longer debounce.

---

## 13. Worked examples

### 13.1 Simple feature commit

1. User on `main` @ `aaa`; sealed gen G0.
2. Edits `crates/api/src/lib.rs`; dirty badge on.
3. Commits → `bbb`.
4. Watcher sees `refs/heads/main` + HEAD; debounce; peel `bbb`.
5. Parent G0; tree-diff → one file; mapper → package `api`.
6. Produce `api`; SymbolDelta 1 changed; 1 embed; tantivy delete+add.
7. Seal G1; active → G1; dirty badge clear.

### 13.2 Dependabot lockfile commit

1. Only `Cargo.lock` changes `aaa` → `ccc`.
2. DirtySet: `dep_lock_changed=true`, packages empty.
3. No local produce; DepSet diff; SyncEngine fetches new delegated gens.
4. Seal empty/local-noop generation G2 still recorded at `ccc`.

### 13.3 Interactive rebase

1. Rebase rewrites 5 commits in 3 seconds.
2. Debounce quiet period collapses to final tip `ddd`.
3. Parent = nearest sealed ancestor still in new graph (maybe far); or full rebuild if orphaned.
4. Single pipeline job for `ddd`.

### 13.4 Multi-root

1. Project opens `/app` (git A) and `/lib` (git B).
2. Commit in A → only A's packages produce.
3. Search session unions symbols from active gen A + active gen B + delegated DepSet.

### 13.5 Detached HEAD build

1. User `git checkout bbb` (old).
2. If G1 sealed for `bbb`, `CheckoutSealed` — no produce.
3. Active pointer moves; UI shows detached.

### 13.6 Explicit preview

1. Dirty tree; user runs "Index uncommitted preview".
2. Ephemeral E1 from fingerprint; search scope Preview.
3. User commits; durable job runs; E1 expired.

---

## 14. Minimal implementation phases (research planning only)

| Phase | Deliverable | Success metric |
|---|---|---|
| **P0** | Types: `ObjectId`, `Generation`, `DirtySet`, `GateEvent` in heart/client | Compiles |
| **P1** | `GixCommitGate::head` + `changed_paths` + poll loop | Detect commit without hooks |
| **P2** | `notify` watcher + debounce | <2s enqueue p95 |
| **P3** | Cargo PackageMapper + package_index tables | Only dirty crate re-produced |
| **P4** | Wire `on_commit_observed` → existing generate pipeline | Empty commit → empty delta |
| **P5** | Dirty UI badge + forbid auto-on-dirty tests | No stage_traces write on save |
| **P6** | Dep lock detection → SyncEngine | Lock-only commit → 0 embeds |
| **P7** | Ephemeral preview lane | Separate GC |
| **P8** | Multi-root + submodule pin metadata | Nested repo isolation test |

---

## 15. Appendix — key URLs

### Git / gix / hooks / watchers

- https://github.com/GitoxideLabs/gitoxide  
- https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md  
- https://docs.rs/gix/latest/gix/diff/index.html  
- https://docs.rs/gix-diff  
- https://git-scm.com/docs/githooks  
- https://git-scm.com/docs/git-worktree  
- https://github.com/notify-rs/notify  
- https://docs.rs/notify-debouncer-full  

### Sourcegraph / SCIP

- https://sourcegraph.com/blog/announcing-scip  
- https://sourcegraph.com/blog/announcing-auto-indexing  
- https://sourcegraph.com/blog/cross-repository-code-navigation  
- https://sourcegraph.com/blog/optimizing-a-code-intelligence-commit-graph-part-2  
- https://www.eric-fritz.com/articles/optimizing-commit-graph-part-1/  
- https://github.com/sourcegraph/zoekt/blob/main/doc/design.md  

### rust-analyzer

- https://rust-analyzer.github.io/blog/2020/05/18/next-few-years.html  
- https://rust-analyzer.github.io/book/contributing/architecture.html  
- https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html  
- https://github.com/rust-lang/rust-analyzer/issues/4712  

### Cursor / GitHub

- https://cursor.com/blog/secure-codebase-indexing  
- https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/  

### Internal research

- `docs/research/librarification/08-incremental/PLAN.md`  
- `docs/research/librarification/14-client-sync/PLAN.md`  
- `docs/research/librarification/15-gui-references/PLAN.md`  
- `docs/research/librarification/13-storage/PLAN.md`  
- `docs/research/librarification/06-symbol-identity-industrial/PLAN.md`  

### Live code

- `workspace/compiler/compile/vcs.rs`  
- `workspace/heart/content.rs`  
- `workspace/compiler/generate/`  
- `workspace/compiler/compile/typescript/oxc/entry.rs`  

---

## 16. Executive summary

nudox's incremental spine (research 08) correctly treats **git as the Merkle** and **symbol content hashes** as the early-cutoff boundary, but leaves the **desktop commit gate** underspecified. This report defines that gate end-to-end.

**Generation identity** is `BLAKE3(project ‖ repo ‖ commit_oid [‖ submodule pins])`, not a branch name. Detached HEAD is allowed; amend/rebase create new OIDs without mutating sealed generations; parent selection walks **sealed** first-parent ancestors and falls back to full rebuild. Worktrees share CAS by content but keep per-project active HEAD pointers. Ephemeral "Index now" generations use a separate id space and must never enter durable REGISTRY publish paths.

**Commit detection** on desktop should be **`notify` watches on `GIT_DIR` (HEAD, refs, packed-refs) plus a 5s poll safety net**, with 300ms debounce and a longer quiet period during rebases. Optional `post-commit` / `post-rewrite` hooks are accelerators only. gitoxide does not replace the FS watcher; existing `vcs.rs` gix code covers clone/fetch/materialize for registry packages and should be reused carefully (never wipe a user's project clone).

**Dirty working trees never auto-trigger** embeddings or durable index updates. The GUI shows a dirty badge and continues serving the last sealed commit; optional preview is explicit and TTL-GC'd. After tree-diff, a **CompositePackageMapper** (Cargo, npm, Go, Python, Maven/Gradle, …) over-approximates dirty packages; lockfile-only commits refresh the **untrusted DepSet** via SyncEngine without re-producing first-party IR.

**Multi-root** projects are multiple `RepoBinding`s each with a `CommitGate`; nested `.git` directories are separate bindings; submodules default to pin-only/ignore. Schema extends 08 with a generations DAG, `project_active`, submodule pins, and ephemeral tables; **stage_traces + CAS survive GC**, per-generation heads do not.

Prior art supports the split: **rust-analyzer** owns keystrokes; **Cursor** shows content-hash embed caches and FS Merkle for continuous index; **Sourcegraph/SCIP** shows commit-unit publication and nearest-ancestor fallback; **GitHub Blackbird** shows blob-OID dedup. nudox's differentiator is **commit-gated, symbol-level, durable constructive traces** on the desktop and in the fleet.
