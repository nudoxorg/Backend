# Handoff — Backend project state (2026-07-09)

**All work is local only. Nothing has been pushed to `origin`.**

---

## 1. Git topology

```
origin/main @ 880ad7f (atomic-refactor era)
   |
main @ f4e4bd7 (ahead by 3 untracked commits)
   |
   +--- 85b03ba  Phase 0 hygiene (PR1)
   |       |
   |       +--- e45cfb4  Phase 1 One CAS (PR2)
   |       |       |
   |       |       +--- efe80b4  fix: review feedback
   |       |
   |       +--- 0449400  Phase 2 One cage (PR3)
   |       |       |
   |       |       +--- cf5ec7d  fix: review feedback
   |       |               |
   |       |               +--- 052c7fb  Phase 3 Producer trait (PR4)
   |       |                       |
   |       |                       +--- efd3070  fix: review feedback
   |       |
   |       +--- (PR5 merge of PR2 + PR3+PR4) → 0303b13
   |
   f4e4bd7 (working tree with UNCOMMITTED changes below)
```

## 2. Worktree directory state

| Worktree | Branch | Contents | Status |
|---|---|---|---|
| `pr-2` (symlink to `~/.cache/...`) | `execute-plan/7f3d44d4-pr-2` | PR1 + PR2 (CAS trait) | Clean |
| `pr-3` | `execute-plan/7f3d44d4-pr-3` | PR1 + PR3 (Cage + WorkerPool) | Clean |
| `pr-4` | `execute-plan/7f3d44d4-pr-4` | PR1 + PR3 + PR4 (Producer trait) | Clean |
| `pr-5` | `execute-plan/7f3d44d4-pr-5` | Merge of PR2 + PR3+PR4 (`0303b13`) | Clean |

Each worktree has the full phase code:
- **PR2**: `workspace/cas/` (disk, memory, tiered, registry), sandbox additions
- **PR3**: `workspace/util/sandbox/` (cage, worker, supervisor, etc.)
- **PR4**: `workspace/compiler/compile/{go,java,nix,python,rust,typescript}/producer.rs`
- **PR5**: All of the above merged

## 3. Main working tree — uncommitted state

### Staged: nothing

### Modified but unstaged (10 files)

| File | What changed |
|---|---|
| `build/third-party/defs.bzl` | +7 lines — new registry config entries |
| `build/third-party/registry.bzl` | +1062 lines — all `ra_ap_*` crate vendoring |
| `flake.nix` | +4 lines — added `rust-analyzer` component for proc-macro server |
| `workspace/compiler/BUCK` | +31 lines — added `ra_ap_*` deps to compiler target |
| `workspace/util/sandbox/profiles.rs` | +9/-? — sandbox profile updates for RA |
| `workspace/compiler/compile/rust/error.rs` | +20 lines — added Cancelled, ProcMacroDegraded variants |
| `workspace/compiler/compile/rust/mod.rs` | +44/-18 — RA/rustdoc dual-path selection (`rustdoc_selected()`) |
| `workspace/compiler/tests/generate_blob.rs` | Comment update (RA default) |
| `workspace/compiler/tests/parse_rust_to_ir.rs` | Added `scaffold_manifests()` + comment update |
| `workspace/compiler/tests/rust_compiler_e2e.rs` | Comment update (RA default) |

### Untracked (5 items)

| Path | Contents |
|---|---|
| `workspace/compiler/compile/rust/ra/` | **10 files, 2936 lines — the entire RA producer** |
| `workspace/compiler/bin/ra_spike.rs` | Spike binary for the RA producer |
| `workspace/compiler/compile/rust/RA_IMPLEMENTER_BRIEF.md` | Implementer brief (220 lines) |
| `RUST-ANALYZER-PLAN.md` | Full RA plan doc (555 lines) |
| `DAEMON-PLAN.md` | Daemon plan |

## 4. RA producer code — file map

All files under `workspace/compiler/compile/rust/ra/`:

| File | Lines | Purpose |
|---|---|---|
| `mod.rs` | 225 | Entry point (`generate_ir`), package discovery, crate matching |
| `load.rs` | 183 | `ExtractConfig`, `LoadedWorkspace`, `load()` via `ra_ap_load_cargo` |
| `ctx.rs` | 385 | `LowerCtx`: path caches, impl index, stable IDs, visibility gate |
| `walk.rs` | 96 | Module DFS traversal, lenient item catch_unwind |
| `item.rs` | 548 | `ModuleDef` → `Entry` lowering |
| `function.rs` | 249 | Function/TraitMethod lowering, receivers |
| `ty.rs` | 920 | AST-first type lowering + sema path resolution |
| `generics.rs` | 269 | AST generic params + where clauses |
| `docs.rs` | 16 | Documentation extraction via `hir_docs` (stub — parity only) |
| `source.rs` | 45 | HasSource + LineIndex → source map |

Total: **2936 lines** of rust-analyzer-based Rust producer.

## 5. What's been verified

- Code compiles? **Unknown.** Tests have never been run. The `ra_ap_*` crates are vendored in registry.bzl but `cargo check` is not available (Buck-only build).
- Tests pass? **Unknown.** Test files were modified to use RA producer by default with `scaffold_manifests()` but never executed.

## 6. What remains to be done

### A. Commit and push the RA producer
1. `git add` all untracked + modified files
2. `git commit` — this lands the RA producer on `main`
3. `git push` to origin

### B. Land PR2 (CAS) and PR3/PR4 (Cage + Producer) onto main

The CAS, cage, and producer trait code exists in worktrees and local branches but NOT in the main working tree or committed to `main`. Options:

**Option 1: Merge each PR branch in sequence**
```
git merge execute-plan/7f3d44d4-pr-2-phase-1-one-cas   # CAS
git merge execute-plan/7f3d44d4-pr-4-phase-3-producer-trait  # Cage + Producer
```

**Option 2: Cherry-pick from PR5 merge commit**
The PR5 branch (`0303b13`) already has all phases merged. Cherry-pick or merge that onto main after the RA commit.

### C. Phase 5 (ForgeRuntime) — never started

The PR5 implementer failed on launch due to API payment exhaustion. Phase 4/5 ForgeRuntime work was never written. Plan docs reference:
- `workspace/runtime/forge.rs` (doesn't exist)
- Runtime integration with the new cage/producer infrastructure

### D. Run tests

Once everything is committed, run:
```
buck2 test //workspace/compiler:...
```
(This was never done — the agent sessions ran into payment limits before reaching test execution.)

### E. P3 removal (rustdoc path)
After parity is proven, remove `NUDOX_RUST_PRODUCER=rustdoc` fallback and the old `package.rs`/`context.rs`/etc.

### F. P4 exceed phase
Deprecation, doc_links, cfg, ObjectSafe/Sealed, glob aliases — LinkML schema changes and renderer consumption.

## 7. Key design decisions captured

- **RA is the default producer**; rustdoc is `NUDOX_RUST_PRODUCER=rustdoc` fallback
- **AST-first type lowering** — HIR loses lifetimes, so type shape comes from `ast::Type` while identity comes from `Semantics::resolve_path`
- **One workspace load** replaces per-package `cargo rustdoc` invocations
- **Lenient item dropping** — `catch_unwind` per item matches today's behavior
- **`Impl::all_in_crate`** for impl index (NOT `all_for_type` which excludes blankets)
- **`ProbeTier::Std`** for blanket/auto trait probing
- **Proc-macro server** via sysroot component (flake `rust-analyzer`)
- **Offline by default** (`CARGO_NET_OFFLINE`)

## 8. Quick-start for a future agent

```bash
# See current state
git -C /Users/philocalyst/Projects/Backend status
git -C /Users/philocalyst/Projects/Backend log --oneline --all --graph -25

# See worktree contents
for wt in /tmp/grok-worktrees/7f3d44d4-pr-*; do
  echo "=== $(basename $wt) ==="
  git -C "$wt" log --oneline -3
  git -C "$wt" status --short
done

# RA producer plan
cat /Users/philocalyst/Projects/Backend/RUST-ANALYZER-PLAN.md

# Implementer brief
cat /Users/philocalyst/Projects/Backend/workspace/compiler/compile/rust/RA_IMPLEMENTER_BRIEF.md
```
