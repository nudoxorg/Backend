# Brief: instant open, and instant everywhere after

**For:** the agent who takes this over (one Opus lane).
**From:** the GUI lead, 2026-09-28.
**Status of the tree:** HEAD f9359e942, clean.

## 0. What the owner asked for

- 2026-09-28 03:15: "not really fully async, the package graph loading thing is a pain, should feel instant across the board, with deep work on datastructures to enforce this, really getting adventurous."
- 2026-09-28 15:30: "dispatch an agent at this point to work on instant app opening."

**Done means:** you launch Nudox and the page you were on is simply there, before you can register a wait. After that, nothing you do ever shows a wait.

## 1. The binding budgets

These come from `Nudox-Design-System/v5/DIRECTION.md` §2. Measure them in a quiet **release** build, and enforce each one as a harness perf test.

| What | Budget |
|---|---|
| Cold launch → a meaningful frame | **< 150 ms**, painted from a snapshot |
| Route change → the new page's first real frame | ≤ 16 ms, from cache |
| Graph open (world or focus) → a laid-out frame | ≤ 100 ms (refinement may follow) |
| Keystroke → repaint (Find, Ask, Filter) | ≤ 8 ms p99 |
| Hover → peek, after the hover delay | ≤ 16 ms |
| Any main-thread task | ≤ 4 ms. A debug watchdog logs the backtrace, and the harness fails the run. |

**A meaningful frame** is:
- the restored route's page, with its title and its first section drawn from real data;
- the shelf with its rows;
- the jump bar with its path.

A placeholder, a skeleton, a spinner or an empty shell is **not** a meaningful frame.

## 2. What is true today (measured; cite these, then re-measure)

### 2.1 The launch path is serial, and the window waits for the index owner

`apps/desktop/src/host/launch.rs` does everything below **before** the gpui app exists:

| Step | Where | Notes |
|---|---|---|
| `open()` | `:131-150` | Retries 12× at 120 ms, with a 20 s deadline |
| `attempt_once()` | `:152` | |
| `DesktopHost::start()` | `host/lease.rs:51` | Embeds `EmbeddedLocalService`, or attaches to a live owner |
| `LocalSubscriptionTransport::connect` + `bootstrap_root()` | | Blocks on the owner's first snapshot |
| `PersistentState::load_recovering` | | Reads `desktop-state.json` |
| `cold_shelf` / `cold_settings` / `cold_reload` | | |

Only after all of that does `run()` (`:48`) start the `EngineActor`, the `ReadPool` (3 sessions) and `gpui::Application`. It then installs fonts, runs `UiEntityGraph::install_with_reads`, calls `open_window` and builds the shell.

**Consequence:** the window cannot appear until the index owner has started and answered. That is structural. No amount of tuning makes it 150 ms.

### 2.2 The harness boot trace

Source: debug build, fixture index, 2026-09-28 07:17. The file is `$S/wave5/instant/runs/after-all-pkg-2/trace.jsonl`, where `$S=/private/tmp/claude-501/-Users-mileswirht-Downloads-backend/4821aaf6-28dc-4f75-b07b-d8fc41618656/scratchpad`.

| Span | ms | What it is |
|---|---|---|
| `boot.owner_start` | 9 315 | Embedded owner start. This is the harness, which indexes 13 roots first. |
| `boot.bootstrap_root` | 818 | First snapshot over the subscription |
| `boot.mount` | 0.6–11 | UiEntityGraph + shell. This part is cheap. |
| `surface Package` | 390–461 | The package page's read, one owner round trip |
| `probe.packages` | 384–450 | |
| `probe.outline` | 40–74 **each, serially, ×10+** | One round trip per outline, in sequence |
| `world.parse` | 773 | `serde` of the **16 MB** `v4/graph/world.json`, on every process start |
| `world.identities` | 220 | `IdentityAdapter::load` |
| `world.tables` | 472–556 | Names + Recipes |
| `read.land Orbit` | 4.3 s after its submit, on the first run | |

- **Tracing** is `NUDOX_TRACE=<file.jsonl>`, from `apps/desktop/src/runtime/trace.rs`. It covers boot spans, `read.submit`/`read.land`, surfaces, probes, world spans and gpui frame timings. It works in the product binary as well as the harness.
- **First job:** run it on the **product** in release, cold (after a reboot or `purge`) and warm, twice each. That is the ledger every later claim is measured against.

### 2.3 The world is a design-system JSON snapshot, parsed per process

`apps/desktop/src/runtime/fixture_world.rs:1-15,48-67` reads `Nudox-Design-System/v4/graph/world.json` through `World::from_json`, and blocks its thread while it does. The graph body and the symbol page's anatomy both read it:
- `shell/bodies/symbol.rs:66,94`;
- `bodies/orbit.rs:151`;
- `status.rs:177`.

The graph's "loading" state is `fixture_world::is_loading` (`:316`). That is the owner's "package graph loading thing": the graph waits on a JSON parse.

### 2.4 Already landed by W-Instant (in HEAD; don't redo)

- `4d3dfbb2c` resolves the project root residence. The owner start no longer rebuilds derived state.
- `1de49b6dd` and `446f8b86d` add the wire claim index: admission is linear instead of quadratic. The live frame's admit dropped from 4.1 s to 66 ms in release, with goldens and mutations (`crates/library/wire/claims.rs`, `claim_index_tests.rs`).
- `93e6466d4` makes the view journal decode only its suffix (`view_journal.rs`).
- The root `Cargo.toml` has dev opt-levels: `blake3` at 3; `backend-{version,flow,library,store,engine}` at 2.
- **Result:** a harness capture fell from 123 s to 22–27 s, pixel-identical (`$S/wave5/instant/runs.tsv`).

### 2.5 Contracts already in the tree that you build on

- **`facet::anatomy::plan`** (`apps/facet/src/anatomy/plan.rs`):
  - `PagePlan` (`:414`) and `pub fn compile(source: &Source) -> PagePlan` (`:456`) are pure and independent of width;
  - `AnchorId` (`:426`);
  - `facet::anatomy::page::Anchors` (`page.rs:81`) carries the measured rects.

  The page lane (`docs/architecture/briefs/symbol-page.md`) renders from this. **You own the cache key and the cache:** `PlanKey { page, inputs: blake3 of compile()'s inputs, schema }`.
- **`Now<T>`** was agreed with the motion and page lanes but **is not in the tree**. Define it:
  ```rust
  pub enum Now<T> { Ready(T, Rev), Stale(T, Rev), Pending { reserve: Size } }
  ```
  - `Pending` carries the size to reserve, so nothing jumps when the value lands.
  - There are no spinners and no skeletons. A section whose data isn't here yet is simply not printed. It prints (by clip, never by fade) when the data lands.

## 3. The design: be adventurous

1. **Window first.**
   - `main` opens the gpui window at once and paints the last frame from a snapshot.
   - The owner start (§2.1) moves to a background thread and arrives as a data event: the snapshot's rows turn from `Stale` to `Ready` as it revalidates.
   - Attaching to an already-running owner stays the fast path.
   - An owner that fails to start is a **notice in the window**, not `exit(70)` with a stderr line.
2. **Snapshot-first state.**
   - On quit and at idle (debounced), persist what is visible and the read models behind it:
     - the restored route's `PagePlan`s and their neighbours (back/forward, hand cards);
     - shelf outlines;
     - the hand;
     - the graph camera and layout.
   - Use a zero-copy archive, memory-mapped. rkyv, or a hand-rolled format with a header, a schema number and blake3 section hashes.
   - At launch: map it, validate the header, paint.
   - Revalidate against the owner in the background. Only what changed moves (hand the difference to the motion lane as a data epoch). **Nothing is re-drawn that didn't change.**
   - A corrupt or mismatched snapshot is ignored and replaced; it is never trusted.
3. **The world as columns, not JSON.** Replace `World::from_json` of 16 MB per process with a content-hashed binary built beside the index (or once per `world.json` content hash while it is still the fixture). It contains:
   - node columns (SoA: x, y, kind, package, name offset);
   - CSR adjacency sorted by edge kind;
   - an interned string arena;
   - a spatial grid for picking and culling;
   - precomputed label placement per zoom band (the LOD). Law 2 says a label is legible or absent, so there are no partial-alpha labels.

   Load it with mmap and no parse. Layout is computed once per content hash and cached on disk. **Graph open ≤ 100 ms** falls out of this.
4. **Pages as plans, cached.**
   - A route change looks up `PlanKey` and paints the plan: ≤ 16 ms from cache.
   - Drawing never walks DTOs.
   - Measured anchors (`Anchors`) are cached with the plan at the current width class, so the motion lane's page ↔ graph fold has its `EdgeAnchors` on the keypress frame.
5. **Batch the owner round trips.**
   - `probe.outline` runs serially, 40–74 ms each (§2.2). One request should return N outlines, or they should run in parallel over the 3 read sessions.
   - `surface Package` at ~400 ms needs a profile. Find out where it goes in the owner, then fix it or precompute it.
6. **Prefetch by intent.**
   - A pointer resting on a link for 80 ms, or moving fast toward it, warms its plan.
   - So do keyboard focus, back/forward neighbours, hand cards and the Start-here stops.
7. **Typed guarantees.**
   - The UI can consume only `Now<T>`. The store guarantees one always exists: the snapshot, the last known value, or an honest empty.
   - Anything async lives behind it and cannot block a frame.
   - Grep-guard that no body calls a blocking read (`fixture_world::blocking`, `std::fs`, `recv()`) on the main thread. Make it a test that scans the source, as the checkout-completeness guard does.
8. **The watchdog.**
   - In debug builds, any main-thread task over 4 ms logs its backtrace.
   - In the harness, it fails the run. Start it in *report* mode; the first ledger will be long.

## 4. Slices (in order). Each one needs:

- a failing test first;
- the release ledger before and after (two identical runs);
- a trap-restored mutation with its real panic line quoted.

1. **I0: the ledger.**
   - Product boot in release: cold and warm, `NUDOX_TRACE`. Also route change, graph open, Find keystrokes and hover→peek, all on the fixture index in the harness.
   - Write `LEDGER.md` with a row per budget: measured, budget, gap, where the time goes (the span tree), and the cause in `path:line`.
   - Then write `PLAN.md` and **stop for the lead's review.**
2. **I1: window first.** The window appears with the last route from `desktop-state.json` plus a minimal snapshot. The owner start moves off the main thread. Tests:
   - `boot_to_first_frame` (harness, release): < 150 ms;
   - an owner that never answers still gives a window with the notice.
3. **I2: the snapshot store.** Mmap'd, schema'd and hashed, holding page plans, shelf, hand and camera. Revalidation morphs only the difference. Tests:
   - a corrupt snapshot is ignored;
   - a stale row becomes `Ready` without re-drawing its unchanged neighbours (assert on the trace);
   - a meaningful frame < 150 ms.
4. **I3: plans cached + round trips batched + prefetch.** Tests:
   - route ≤ 16 ms from cache (harness perf test);
   - one owner round trip for N outlines.
5. **I4: the world as columns.** Tests:
   - graph open ≤ 100 ms;
   - a pixel-identical graph capture against the JSON path;
   - an mmap'd world with a mismatched hash is rebuilt, not trusted.
6. **I5: the watchdog + keystroke p99** in Find, Ask and Filter.

## 5. Interfaces with the other lanes

- **The transitions lane** (`docs/architecture/briefs/transitions.md`) needs two things from you:
  - the destination page renders **on the click frame**, from `Now<T>`;
  - `EdgeAnchors` come from the cached measured anchors.

  If a section's data lands later, it prints when it lands.
- **The page lane** (`docs/architecture/briefs/symbol-page.md`) owns `compile()`'s inputs and the plan's shape. You own the key, the cache and its invalidation.
- **The lead** owns the shell (`apps/desktop/src/shell/**`). Keep your changes to the shell to mounting `Now<T>` and the notice. For anything more, write a request into your checkpoint.

## 6. Rules (binding)

- **Git.** Only `git status`, `git diff`, `git log`, `git show`, and `git add <a NEW file you created>`.
  - Never stash, checkout, restore, reset, clean, commit, rebase, switch, merge or filter-repo, including inside scripts and sub-agents.
  - The owner commits.
- **Build.** Only through `.local/devenv/cargo <cmd> -p <crate>`. Never use `--workspace`, and never run `cargo clean`. Other lanes share the build lock, so run one cargo process at a time.
- **Harness.**
  - Build: `.local/devenv/cargo build -p backend-desktop --features visual-harness --bin backend-desktop-gui-harness`.
  - Capture: `.local/target/debug/backend-desktop-gui-harness capture --scene <id> --out <dir> --size 1440x900 --time 1500`.
  - Journeys: `… journey <file> --out <dir>`.
  - Scenes: `desktop-{orbit,package,symbol,code,graph,world,tree,find,compare,value,serialize,smallvec,error}`.
- **Tests.**
  - Trust a result only when two consecutive runs agree.
  - Tests pin fixtures, never the live `world.json` or the owner's registry.
  - Assert on rendered content, not counts.
  - Mutations are applied, run and reverted inside one Bash command with a trap, then `touch`ed. Quote the real panic line.
  - Remember that `index::server` tests need `--features server`.
- **Scope.** Lints go to a later Sonnet pass. Every claim cites `path:line` at the current tree.
- **Checkpoints** go in `$S/wave6/instant/`.
