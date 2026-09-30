# Production readiness: what still needs attention

This brief was written by the lead on 2026-09-29, at the end of wave 6, the GUI push that followed the v5 and v6 design work. It lists every issue known to the lead that still stands between the Nudox desktop app and a product a stranger could install and trust. Each issue says what it is, where it comes from, where the code is, and whose area it falls in.

Two finishing agents are working as this is written:
- **F-Data** covers the data plane, first run and lifecycle.
- **F-Shell** covers the shell, journeys and green suites.

Their brief is `.local/lanes/final/FINISH.md`. Issues they are expected to close are marked **[F-Data]** or **[F-Shell]**. The closing section, "Status after the finishers", is filled in when they report. Everything else is open.

**Sources.** Each issue cites its evidence. The lane files live under `.local/lanes/wave6/` and are git-ignored.
- Journey gaps: `journey/GAPS.md`, with ids G1–G15, L1–L2 and D1–D6.
- Reviews: `review/{fit,folio,sym6,open3,feel}/…`.
- Lane checkpoints: `*/<LANE>-<n>.md`.

A "reported at hh:mm" date means the lead has not re-verified the issue since. Check it before starting work on it.

**Severity**

| Level | Meaning |
|---|---|
| P0 | A real person cannot use the product. |
| P1 | A person hits a visible defect or a missing feature. |
| P2 | Quality or technical debt that will cost later. |
| P3 | Polish. |

---

## 0. Where the product stands

**Works on real data** (HEAD `6ef7ebb6a`), with a pre-admitted fixture index:
- The Library (11–13 packages, about 12,200 declarations, real owner compiles).
- A package page: licence, weight, heads-up, timeline, features, territory, README.
- The simple symbol page: variants, verbs, uses, source and neighbours. Toml's `Value` looks right.
- Ask (⌘K) in the titlebar.
- The graph, with a fixture world.
- Settings, and fluid layout from 320 to 2560. The shelf has hysteresis, the phone has a drawer with a scrim, and no reader squeeze on a fast shrink.
- The add-folder dialog, and a failure card with Try again, Reveal and Remove.
- Restart persistence, for most fields.
- Real journeys: runner, parts, cached states, and J0/J1 in progress.

**Not true yet for a stranger.** Most of the P0 issues below have one cause: every test so far ran inside the dev shell, with fixture roots admitted ahead of time. The first real clean-install journey (08:20) exposed that a person who adds their own project sees one package, can read nothing while it indexes, and, when launched from Finder, would have no compiler at all.

---

## 1. P0: a real person's first run

### 1.1 Toolchain discovery for a Finder launch [F-Data]
- **Problem.** The desktop's embedded owner finds compilers only through explicit `NUDOX_*` variables.
  - `apps/desktop/src/host/toolchain.rs` derives Cargo and Cargo's home only when `NUDOX_RUSTC` is set.
  - `production_at` uses `LocalHostDiscovery::ExplicitOnly`.
  - The dev shell sets these variables, but Finder does not. Every Rust root would then be refused as `Unavailable { Rust, LowerIr }`, the same failure the 03:12 index merge caused.
- **What exists.** The engine already has `LocalHostDiscovery::PlatformDefaults` (`crates/engine/src/application/host.rs:433`, `host/paths.rs:337`). This machine has `/opt/homebrew/bin/{rustc,cargo}`, and rustup's `~/.cargo/bin` is the other common location.
- **Needed.**
  - The desktop discovers the person's own toolchains. An explicit variable still wins.
  - The first run and Settings say what was found or what is missing, and how to fix it.
  - Headless `backend-locald` keeps explicit-only.
  - Journeys that stand for a first install run under `env -i HOME=$HOME PATH=/usr/bin:/bin`.
- **Also.** Go needs a module-cache root and gets the same treatment. The other languages (TypeScript, Python, Clang) have not been run through the desktop this wave.

### 1.2 Adding a project indexes none of its dependencies [F-Data] (G15)
- **Problem.** A clean real index of `frontends/rust/fixtures/toml_pin` ends Ready with one package: toml_pin itself, with 25 declarations. The harness only showed 11 packages because `apps/desktop/src/harness.rs` `fixture_with_progress` admits each registry source as its own root.
- **Why it matters.** It breaks the product's core promise, and the first-run copy makes that promise: "compiles it and every package it uses".
- **Needed.**
  - Resolve the project's `Cargo.lock` (or the owner's own resolution) to releases.
  - Resolve each release through one typed registry source (§2.1), offline from `~/.cargo/registry`.
  - Index each release through the owner, and name any dependency that isn't on this machine.

### 1.3 The owner blocks every read while it indexes, and reports no progress [F-Data]
- **Problem.** `session.index` is one blocking `Command::Add` on the owner's single loop (`crates/local-service` listener, "the sole caller of handle_payload"). Page reads, `revision()`, `health()` and `packages()` all queue behind it, up to `owner_reply_timeout` (15 minutes). No stage events exist.
- **Evidence.** W-Install's INSTALL-A.
- **Needed.**
  - Reads are served from the last publication while a job runs.
  - A typed progress channel: stage, package *n* of *m*, and each package done or refused with its reason.
  - Ideally, per-package publication, so a person can read toml while serde compiles.
- **Constraints.** Keep the single-writer and durability guarantees (SQLite, catalog-as-WAL, outbox watermarks), and prove recovery with a real `kill -9` mid-job. The full design is in `.local/lanes/wave6/lead/briefs/W-Owner.md`.

### 1.4 Semantic rows carry no module path or source location [F-Data]
- **Problem.** Since Rust compiles for real again, semantic declarations arrive as `<package>::semantic::<hash>::Name` with `path=None, line=None`.
- **Effects.**
  - The package page reads "155 public names in **1** module" for `present`, which has about 30 modules (`.local/lanes/wave6/index/after-old/desktop-package-abyss-100pct-t1500@1x.png`).
  - The harness scene `desktop-record-rust` panics at boot with "no exact candidate". That stops `capture --scene all` after 16 scenes (`harness.rs` `outline_symbol_candidates`).
- **Needed.** Semantic rows carry their module and source location, or the view joins them to their structural twins. Add a test on a real multi-module crate.

### 1.5 A refused package is invisible until the next boot [F-Data]
- **Problem.** The owner commits a project's source frontier, then returns the compile refusal before republishing the view. The Library therefore shows 11 packages on the first boot and 13 on the second; zod and pflag appear only then.
- **Fix.** About 8 lines in `CommandAdapter::add` (INDEX.md "The 11 vs 13"), so a person sees "zod: could not be read (…)" at once.

### 1.6 Production features still read fixtures
- **Release diffs, change marks and upgrade counts** come from `apps/desktop/src/runtime/fixture_releases.rs`, which serves facet's embedded `apps/facet/src/data/release/fixture.json` (the prototype's toml and smallvec only). The same data feeds the package banner, symbol history, the upgrade lens and the sidebar's row glyphs (G14). **[F-Data, as far as it gets]**
- **The graph** draws the prototype world: `runtime/fixture_world.rs`, labelled "Graph fixture · …" on screen (`shell/bodies/graph.rs:2`). The hand's arrangement is also computed on the fixture world.
- **Needed.** Real graph data from the index: relations, reach and the tour. That is a data-plane project on its own. The label is honest, but a stranger will see a fixture in the product.

### 1.7 Packaging and distribution
- **The bundle is minimal.** `apps/desktop/package-macos.sh` builds `Nudox.app` from one binary plus `Info.plist`, and the fonts are embedded (`facet/src/fonts.rs` `include_bytes!`).
- **Missing entirely:**
  - code signing, notarization and the hardened runtime with its entitlements;
  - an app icon in the bundle (not checked);
  - a version and build number wired from Cargo;
  - an update mechanism (Sparkle or similar);
  - crash reporting, and any logging a user could send (the desktop has almost no `tracing` instrumentation: 2 files);
  - a first-launch Gatekeeper story;
  - uninstall and state location docs (the state dir lives under the host's paths, `host/paths.rs`).
- **The script calls `cargo build`** directly, not the nix wrapper or `cargo-in`. Confirm it builds outside the dev shell, and which toolchain it expects.
- **Homebrew.** There is a `Formula/backend-mcp.rb` for the MCP server, but no cask for the desktop.

---

## 2. Data plane (P1)

### 2.1 One typed registry source [F-Data] (G11)
- Registry name and version resolve to a source tree the owner indexes.
- **What exists:**
  - the harness's `registry_source` (`harness.rs:200`, test-only);
  - `model/source_facts/registry.rs:95` `source_of(name, version)` (string-typed, facts only);
  - a private duplicate in `shell/bodies/symbol/place.rs:19`;
  - the owner's purl path, which FETCHES over the network (`crates/local-service/src/builtin/registry.rs:802` `acquire`).
- **Needed.** One typed interface. The offline adapter reads `~/.cargo/registry/{src,cache}`; the network adapter goes behind an explicit policy (below).
- **Product decision needed: network policy.** When may the product download a crate? Always, after consent, never? How is that shown ("available offline" versus "would download")? Nothing is decided. Agents in this project were never allowed to download.

### 2.2 Add a package from browsing [F-Data data, F-Shell affordance] (G10, G12)
- **No Add affordance anywhere.**
  - Find's rows offer only Held/Compare (`apps/facet/src/browse/find.rs:418`).
  - The package header's `cargo add NAME` is plain text (`marks/eco.rs:116`).
  - Dependency marks only navigate (`shell/bodies/package.rs:291-296`).
- **Find cannot show a crate that is only in the local cargo cache.** `runtime/reads.rs:1126-1168` `compose_find` merges only index and owner catalogs.
- **Partly built.** W-Acquire built facet components for the offer and its seam (`apps/facet/src/browse/acquire*`), plus a desktop worker (`apps/desktop/src/runtime/acquire/work.rs`). Its ACQUIRE-A (06:40) has the design.

### 2.3 Earlier releases on demand [F-Data] (G13)
- **The owner only reads indexed releases** (`product_state.rs:299`, `:436`). `compose_package` asks for the `@0.5.11` purl and never indexes it.
- **A local root's pin falls back** to the pin's dossier, so the page shows 0.8.23's names under "Reading 0.5.11" (`folio.rs:256`).
- **A symbol at another release** is `Unread::ReleaseNotHere`.
- **Needed.** The releases available (cache, and the registry index in production), index-on-demand with progress, rendering from the owner's reply, and a real diff (G14) behind `release_data_for`.

### 2.4 Other owner items
- **`unsupported view DTO version`** (a `view.journal` from an older wire) is still refused whole, as `ProcessError::Profile` prose. It should join `StateFromAnotherBuild` (INDEX.md "what's left" 4). **[F-Data]**
- **`crates/engine/tests/go_corpus.rs:609`** does not compile, because `owner.input()` now takes a `&Path`. `cargo check -p backend-engine --tests` fails at HEAD. **[F-Data]**
- **Named refusals left on the fixture set:**
  - serde_core `Lowering(LoweringCause(RustGenericParameter))`: a real lowering gap in the Rust frontend;
  - zod `Authority { Open, Binding }`;
  - pflag `Authority { Resolve, Authority }`;
  - tokio `Queue(Bytes)`, a queue-capacity refusal, not a compile one.

  Each one is a package a person can't read.
- **`ProcessError::StateFromAnotherBuild(String)`** carries prose where a typed reason belongs (§9).
- **The lane Clang adapter** is dead: removed from `hx` and `cargo-in` at 17:10, with W-Index's product probe fix in HEAD. The file `.local/devenv/clang-sysroot-adapter` remains and can be deleted.
- **The dev shell** (`.config/nix/corpus-env.nix`) still exports only `NUDOX_RUSTC`. `NUDOX_CARGO`, `NUDOX_CARGO_HOME` and a Go module root should be exported there for other embedders (INDEX.md fix 3).

### 2.5 Advisories and MCP
- **The package page's ADVISORIES cell reads "no feed"** ("RustSec, OSV and GHSA can be read; none configured").
  - No feed is configured or shipped.
  - The owner was asked earlier this wave for permission to clone `rustsec/advisory-db`, and has not answered.
  - Offline advisories need a bundled or synced database and a freshness story.
- **MCP setup has no surface (L1).** Settings has no Agents/MCP page (`shell/bodies/settings.rs:25-32` maps Agents to Appearance). `Intent::TestConnection` exists, but nothing on screen dispatches it. The retired onboarding spec had a `McpSetup` journey.

---

## 3. Shell and interaction (P1 unless noted)

### 3.1 The sidebar (`apps/desktop/src/shell/side/*`) [F-Shell]
W-Side split `shelf.rs` into a typed model (scope, lens, row, state, listing, narrow, peek, keys) and views. The work is in HEAD, but the checkpoints are partly unverified on real data (SIDE-A/B/C, 07:44).
- **D1:** the "Type to narrow" hint is 2.59:1 contrast, which fails lint on every Library frame. No journey checkpoint with the shelf on screen can pass until it's fixed.
- **Real data shows:**
  - duplicate module names with no parent context (`map` ×3, `array` ×2, `mod` ×2 on toml's `Value` page);
  - `use` imports listed as a module's contents (BTreeMap, HashMap, fmt…);
  - "Contents 747", whose meaning is unclear;
  - `toml_pin` listed twice in the Library, as the project and as a package. Find out whether the data or the view is wrong.
- **Not done (SIDE-C §6):**
  - ⇧⌘J "reveal on demand" has no key-table row;
  - Settings › Keys doesn't list the sidebar's keys (typing, G C/V/R/U, G G, H);
  - the drawer instance of the sidebar (phone widths) has no keyboard;
  - chips go to ⌘5, because the hand holds five.

### 3.2 Keys and focus
- **Esc does not close Settings (D5).** The route stays `Settings`, so a relaunch reopens Settings. **[F-Shell]**
- **Documented shortcuts that nothing binds (L2).** `navigation/action.rs:204-225` lists cmd-shift-p, cmd-b, cmd-n, cmd-shift-a, cmd-shift-y, cmd-shift-/, cmd-shift-m, cmd-left/right and cmd-0 (Home). cmd-0 collides with ZoomReset. Either bind them or delete the metadata. **[F-Shell]**
- **Arrow keys and Enter were swallowed** by the shell's "walk focus" under any menu other than the jump bar's. W-Sym6 fixed this (`facet::overlay::float::menu_open`, `titlebar.rs::menu_open`). Check every other popover that takes arrows: the Find inspector, the release picker, and the Add dialog's completion list.
- **`hints.rs:43` `codes(count)`** silently leaves targets past the 256th without a hint code (FEEL D15, P3).
- **The package page's crest cells, features bar, ticker and berg blocks are not keyboard targets** (R-Folio D7a). Esc does not fold an open module (D7b), and a stale focus ring remains after Enter on a module (D7c).
- **`cmd-shift-c` (copy the address)** gives no feedback (FEEL D10). It needs a toast.

### 3.3 Overlays, popovers and motion
- **Peek and menu dismissal** (grace plus roll-up, 220 ms) exceeds the 120 ms bar by construction (FEEL D13). Either the bar or the construction changes.
- **Package page popovers open with no dwell** as the pointer crosses the crest: licence, heads-up, then weight, one after another (R-Folio D9). The house rest is `QUICK_REST`, 120 ms.
- **The reader's plate morph jumps** on a shelf-row click: 33 px jumps, a 25 px overshoot, and the carry out of lockstep (FEEL D5; storm seeds 2 and 3).
- **Opening Settings** swaps the shelf in one frame while the reader still shows the Library for about 100 ms, then a near-empty reader (FEEL D17).
- **Blocks under a mode change jump** by the changed block's height in one frame. The package page has 16 such thresholds and the symbol page 5. A reader-level reflow helper would serve both (FLUID-C "Next" 1).
- **In-flight Flow chips cross each other** after a fast step. That needs a change in `facet::motion::flow`, which every flow uses (FLUID-C "Next" 4).
- **The Library shows no hover popover** on package words or shelf rows. That is a design call (FEEL D9).
- **The add-folder dialog** should be on the float layer, so the harness recognises its scrim: 13 false contrast lints per checkpoint (D4). Its focus ring jumps on close (D6). **[F-Data]**
- **Harness lint gaps:**
  - It skips text that is wholly clipped or outside the window, so a strip laid out past the right edge lints clean (R-Fit "The lead"). It needs a fully-hidden rule.
  - Button labels are not probe texts, so contrast, clip and overlap lints never check a button's words (D3).

### 3.4 Library and Find
- **Find:**
  - at narrow widths, the name column squeezes to 19–32 px with no ellipsis (FEEL D6);
  - at 200 % text, the settled frame differs from a reduced-motion boot (FEEL D7);
  - the inspector glides through the results, with text over text for 3+ frames (R-Fit 7).
- **Hit targets under 24 px:** the Library chips at 85 % text (`orbit.rs:257`) and Find's inspect controls (FEEL D12); the package page's badges are 21 px (R-Folio D14).
- **At 2560, the Library, package and symbol pages** are an 800 px column in an empty window (R-Fit 8). FLUID-B added wide leaves; re-check.
- **The Library at 200 % text and 360×900:** the viewport ends around y=550, and the caption is cut on both sides (R-Fit 9, reported at 02:30).

### 3.5 Lifecycle
- **An owner failure** shows "Try again" only inside page bodies. The Library and status bar show it as text only (G7, R7). **[F-Data]**
- **Window size persistence** (G5) was done by W-Install but only unit-tested. It needs a real resize and relaunch. **[F-Data]**
- **A shell launch inside a project folder** auto-admits it and skips the first run (G9). That is a product decision.
- **`runtime/owner.rs::watch`** holds root and store strongly, so dropping an app leaks handles, which panics in debug at quit (D2). **[F-Data]**

---

## 4. The package page (W-Folio's; P1 unless noted)
Mostly from R-Folio's review (02:54). FOLIO-B (03:11) addressed some of it, so re-verify each item on real data.
- **Structure and data:**
  - When names carry no module, the page draws one `lib` block as though that were true. It should say so (§1.4).
  - The HEADS-UP tile is empty apart from its icon on some packages (for example `present`). **[F-Shell]**
  - **(D3)** On a package indexed from an unpacked registry directory (`is_local()`), the release ticker is absent and time travel loses its banner (`page_mapping.rs:1754` sets `versions = Unknown(LocalProject)`).
  - The README (D12):
    - link reference paragraphs print as raw URLs;
    - links leave dangling text ("(LICENSE-APACHE or )");
    - nothing is clickable;
    - the left edge is off the folio's;
    - it clips at 480 px.
- **Layout and motion:**
  - **(D1)** Live resize flickers the whole lower page: the crest flips between one and two rows on alternate frames (a 150 px jump).
  - **(D2)** A fresh window at 1440–1600 wraps ADVISORIES onto its own row.
  - **(D4)** Hovering a badge reflows its card and the grid below it.
  - **(D10)** The crest re-lays out on arrival, because the first frame doesn't know the reader's bounds.
  - **(D11)** Module open and berg open are cuts, not motion.
- **Phone widths.** 94 of 285 sweep widths have text past the window's edge: feature chips and the repository URL, from 416 px down. The chips need `flex_wrap` and `min_w(0)` with an ellipsis (FLUID-C "Next" 2). **[F-Shell]**
- **Visual (D5, D6, D13, D8):**
  - The heads-up stack is unreadable at rest: each chip covers half the previous glyph.
  - A Cargo description with a newline renders as a hard break.
  - The hero lede and byline run past the edge at 430 px and below.
  - Region labels clip by 0.7–1.2 px below 900 px and at 200 % text.
  - There is no pointer cursor over hand-written elements (shingles, ticker, berg).
- **Planned but not built (P3):** Phase C "shingles fly to their cards".

---

## 5. The symbol page (W-Sym6's; P1 unless noted)
From R-Sym6's review (04:27) and FLUID-C. SYM6-B (08:36) addressed some of it.
- **(D1)** Phone widths: the written type runs past the plate edge, and the mint case count overlaps it.
- **Motion:**
  - **(D2)** Layout changes are cuts: the rail disappears within one frame.
  - FLUID-C finds 5 hard thresholds on real content. The worst is the `SYMBOL_RAIL` edge at 1344, where `s6-block-source-sub` moves 1084 px in one frame.
- **Words:**
  - **(D4)** The verbs don't read like the board. Value reads 305 places (`names` 170 …) where the board has reads 368, makes 47, changes 1. The line-local classifier misreads a pattern whose `=>` is on the next line (SYM6-B).
  - **(D5)** Enum case rows carry bare numbers with no words.
- **(D6)** Hover and cursor feedback is missing on the "Example" fold line, "N more in workspace" and the rail source link.
- **(D8)** Glacier contrast: the quiet "imports" chip is 4.23:1, because `kit.rs::chip` dims the whole chip with opacity. It should dim through the colour token.
- **(D9)** Icons: the function kind mark reads as a disclosure caret, and the enum mark doesn't match the board's diamond (`facet::icons`).
- **(D10)** Fidelity:
  - "Next to it" shows name tails and repeats, with no outcome glyphs (`facts.rs` builds `Beside { signature: None }`);
  - code inside doc links prints its backticks;
  - the Gives row drops an Option/Result output's written type;
  - the generic card's "Elsewhere in the registry" is never drawn;
  - "83 % derive" needs a derived flag the index doesn't provide.
- **Hand-rolled widths still in facet** (`fluid::tests` `KNOWN`):
  - `anatomy/page.rs:86`, `anatomy/page/gallery.rs:433`, `anatomy/gallery.rs:196`, `anatomy/prism.rs:23,267,272`;
  - plus the dead legacy `Room` enum, `Measure::room` and `Measure::columns` in `measure.rs:111-117`, which can be deleted.

---

## 6. Runtime and performance (P2 unless noted)
- **Release-mode budgets have never been judged this wave:** page open ≤ 120 ms, search ≤ 50 ms, flight ≤ 1500 ms, p95 frame ≤ 8 ms at 1440×900 (gui-plan §3.10). The journeys measure in debug but only judge in release. **(P1 for "production-ready")**
- **(D1)** `warm_anatomy` (`store.rs:292`) starts a world computation on every visit whose result nothing reads, and each result redraws every window (R-Open3).
- **(D4)** Every hand touch re-runs the producer walk on the UI thread: 291–724 ms in dev and about 10 ms in release, a dropped frame per ⌘1–⌘5. The cause is that `Tables::hand` compares `touched_at`.
- **(D5)** A panic on the world thread is silent and permanent: `fixture_world.rs:491`, with no `catch_unwind`, so `is_loading()` stays true forever.
- **(D6)** A panic on the owner thread used to leave every read blocked on a `Condvar` with no timeout. `host/owner.rs:92` now catches the starter's panic. Verify that the gate fails the waiting reads.
- **Legacy names awaiting migration** (R-Open3 `MIGRATE.md`): `hand_view` becomes `hand_view_for` (5 call sites in `status.rs`, `root.rs` and `orbit.rs`), then delete the `Everyone` arm, among others.
- **The owner reply timeout is 15 minutes.** After §1.3, reads should time out quickly and say so.

---

## 7. Tests and harness (P1 for green; P2 otherwise)
- **The suites are not green** as of 08:00. Nine desktop lib failures, eight of them deterministic:
  - reader transit tests comparing words the package page no longer says;
  - `browse::tests::the_library_page_shows_a_real_tree…`, which fails with "private state parent is not owned by this user";
  - `motion_tests` on the symbol page;
  - the lease owner test.

  Three facet failures:
  - `anatomy::symbol::layout` (…`a_width_at_the_edge_does_not_flicker`);
  - `graph::prism::rail` (…`family_head_text…`);
  - `overlay::float::unfurl` (…`a_reversal…`).

  Also failing: `probe::rules` (…`a_text_wholly_past_a_side…`, FEEL D16) and the `backend-engine` tests' compile (§2.4). **[F-Shell]**
- **Journeys** (`apps/desktop/journeys/`, runner in `apps/desktop/src/harness/journey*`).
  - **Required:** J0 install (Finder environment), J1 first look, J9 search, J10 settings persist, J11 every key (generated from the table), and J12 the crawler. **[F-Shell]**
  - **Also to run:** J2 find by shape, J3 upgrade (blocked on real diffs), J4 the map, J5 failure pages, J6/J14 weather, J7 add from browsing, J8 earlier release, and J13 failure and retry. The owner refuses almost nothing small, so J13 needs a real failing fixture.
  - Each key journey needs a product mutation that makes it fail.
- **Harness correctness** (R-Fit C1–C4, reported at 02:30):
  - the refusal record is keyed by source mtimes, not the compiled owner and toolchain;
  - every `ClientError` is recorded as an owner refusal;
  - a refused root with no Ready row is called Failed too early;
  - "13 of 13 refused, `failed()` empty" hides behind a Ready row.
- **Tests that prove nothing** (R-Fit C8):
  - `survey_every_scene_at_every_size` asserts nothing, and its gate is an `eprintln!`;
  - `a_state_dir_the_harness_creates_is_one_the_owner_accepts` passes under any umask;
  - three `fixture_world_tests` assert words only the retired body drew.
- **The graph under load.** `matrix --scene desktop-graph` and `lint --scene all` end with "the product never went quiet within 120s" under load, because the wait is wall-clock. It should be virtual, or bounded by work done.
- **The full `verify` gate never gave a verdict this wave.** It ran only scoped (one scene, `--quick`), because of machine load.
- **`fit_tests` and `fluid_tests`** carry lane names in product paths (`.local/harness/w-fit-*`, from `fallback_state`), and panicking tests leave directories behind (R-Fit C7).

---

## 8. Platform, accessibility, internationalization (P2)
- **Cross-platform.** The `.#cross` shell compile-checks Windows and Linux, but it was not run this wave. GPUI on Windows and Linux has never been exercised for this app, including fonts, the titlebar, the traffic-light inset and the folder picker.
- **Accessibility.**
  - Contrast lints and a high-contrast mode exist, and keyboard reach is broad.
  - Screen readers were not exercised at all: VoiceOver would need the AX tree GPUI exposes, if any.
  - Reduced motion exists (`MotionPreference`).
  - Dynamic Type follows the system text size via `ZoomTo`.
- **Localization.** None: every string is inline English.
- **Right-to-left and long words** have not been tested.

---

## 9. Typing and abstractions (P2)
The owner flagged that typing "is getting worse". The known issues:
- **Prose matching** where a type exists: the held-lock test matches `"AlreadyOwned"` in a `Debug` string (R-Fit C5), and `StateFromAnotherBuild(String)` should carry a typed reason.
- **Tuples and parallel indices in the harness:** `Vec<(PathBuf, String)>` in a public signature, and `refused: BTreeMap<usize, String>` read against a parallel `Vec<Standing>` (R-Fit C6).
- **`fixture_with_progress`** is a 174-line function doing three jobs (R-Fit C7).
- **`Targets` is `Clone`,** and `ctx.targets.clone()` compiles inside any `Act`: the D1 class of bug (an action holding the list that holds it) is still writable (R-Fit C9). `Recall` re-implements `Targets::focus`.
- **Render mutates the model:** `root.rs` sets `shelf_over_open` inside `Render::render` (R-Fit C10).
- **Callback bags of `Rc<dyn Fn>`** in facet components (for example the acquire offer's `state`, `add` and `open`) where a typed action enum dispatched through `Intent` would be checkable.
- **Magic numbers and parallel constants** remain in page code. They are caught only where the `fluid` scan looks (widths), not for heights, durations or opacities.

---

## 10. Process and infrastructure (for whoever runs the next wave)

### What bit this wave
- **API rate limits killed every agent twice,** at about 05:25 and 08:55 UTC. Work survived because the lead snapshots the tree often, and agents write checkpoints to disk. Keep both habits, and keep ≤ 3–4 concurrent agents or expect it again.
- **Machine load reached 152** on 11 cores: eight compile groups at full `-j` plus six harness renders. The fixes now in place:
  - `cargo-in` sets `CARGO_BUILD_JOBS=3` under `nice -n 10`;
  - harness renders go through `.local/devenv/slot harness 4` (inside `hx`);
  - one cargo process and one harness process per agent.
- **A test mutant reached HEAD once.** A snapshot captured a mid-mutation tree (`root.rs` `.w(px(0.0))`, in 6e2b2bf3d, fixed in 86cd27d8e). The protocol since then:
  - mutation runs hold `.local/mutating/<lane>.<pid>`;
  - the lead commits only isolated snapshots (`.local/lanes/wave6/lead/commit/snapiso.sh`), captured at a mutation-free instant and compile-checked as a frozen copy.
- **A quiet-tree snapshot never finds a window** with about 14 agents (25 tries failed). Use `snapiso.sh`.
- **The owner's index workstream** commits to this branch from its own worktrees (`/private/tmp/backend-*`, `~/Documents/ChatGPT/backend-*`). A merge from it silently broke the desktop's index at 03:12. Nothing but a real journey would have caught it: gate merges on J0 and J1.
- **Agent slips seen:**
  - one agent ran `git checkout -- <file>` on its own edit;
  - one lane's new facet module shadowed a helper and broke every lane's facet tests for about 20 minutes;
  - one lane deleted a public type (`Targets`) that 10 files used.

  The rules that followed: git is read-only, shared APIs are never removed, and the tree compiles whenever a lane isn't mid-edit.

### Cleanup available (the owner's call; nothing is deleted without asking)
- About 30 harness state dirs under `.local/harness/` (`w-*`, `r-*`, `fallback-*`, `w-fit-unit-*` left by panicking tests).
- 13 build groups under `.local/build/`, each 40–50 GB apparent. They are copy-on-write clones, so the real usage is lower, but they diverge as they build.
- `.local/lanes/wave6/index/tree/` (223 MB).
- The dead `.local/devenv/clang-sysroot-adapter`.

---

## 11. Decisions only the owner can make
1. **Network policy** for Add and earlier releases: when may the product download a crate, and how is consent shown (§2.1)?
2. **Advisories:** may the project clone `rustsec/advisory-db`, and how does the app keep it fresh (§2.5)? The earlier request is unanswered.
3. **A shell launch inside a project folder** auto-admits it and skips the first run (G9). Keep or change?
4. **The Library's hover popovers** (FEEL D9), the **2560 layout** (§3.4), and **peek/menu dismissal against the 120 ms bar** (§3.3).
5. **The graph's fixture world** (§1.6): ship it labelled, hide the graph until real relations exist, or schedule the data work?
6. **Distribution** (§1.7): signing identity, notarization, update channel, crash reporting.
7. **What `/ua` meant** (an interrupted message).

---

## Status after the finishers

The 2026-09-29 inventory above is historical. The checkpoint below records what was integrated and tested, and the resumed-readiness ledger records subsequent verification. A feature's existence in source, an isolated unit test, and a passing installed-product journey are separate evidence; none substitutes for the others.

## Checkpoint verification — 2026-09-30

The current GUI, compiler, index, semantic-history, and storage work was collected into subsystem commits and integrated. The 45 pending GUI paths were split into eight commits. Seventeen superseded worktrees were preserved in 61 commits on local `codex/checkpoint-archive-20260930-*` branches; their obsolete implementations were not substituted for the current architecture. The local collection inventory and source backups are under `.local/checkpoints/2026-09-30-collect/`.

These checks actually ran in the saved Nix shell, with one Cargo job per lane:

| Check | Observed result |
|---|---|
| `cargo check --locked -j1 --workspace --all-targets --keep-going` | Passed for the integrated tree |
| `backend-version --lib` | 102 passed |
| `backend-semantic --lib` | 254 passed |
| `backend-store --lib`, after the CAS oracle correction | 255 passed |
| `backend-store --doc VerifiedObjectView` | The borrowed-view escape compile-fail test passed |
| Materialization-policy Python model | 16 passed |
| Common search-lane Python checks | 2 passed |
| Frozen search provenance | 25 primary documents and 46 source records verified |
| Full `backend-desktop --lib` run | 541 passed, 9 failed, 10 ignored |
| Focused desktop fixture-world rerun, after shortening its Unix socket path | 1 passed |
| Full `backend-replication --lib` run | 169 passed, 8 failed, 1 ignored |
| Focused replication reruns after fixture repairs | Both map-count crash-window tests and the map-unlink/reclaimed-byte test passed |

The full desktop and replication suites were not rerun after those focused repairs. Their original failure counts must not be presented as an all-green verdict.

Eight desktop owner tests still need investigation. Focused runs refused cached `toml 0.8.23` at `examples/decode.rs` and `serde_core` at `build.rs`. The error is flattened by `PackageAuthorityError::RustProject(_)` into `Authority { phase: Resolve, class: Authority, diagnostic: None }`, so the concrete workspace/setup cause is lost. Debug read-frontier observation was not enabled in those runs. Offline metadata resolution is a hypothesis, not a confirmed cause; no speculative compiler performance change was made.

Five replication failures remain unresolved or unverified: corrupt unreferenced bridge-map admission, deleting the last branch reference, torn references/missing parents or generations, competing child-process reference CAS, and nonempty typed-V2 cold publication/replay/GC. The three focused passing repairs above concern fixture privacy/layout and physical map-byte accounting. They do not prove these five cases safe.

The GUI process wrapper now uses the current live journey runner rather than the retired capture adapter. Its project-choice plan includes a restart and checks that the project remains on the shelf. This native journey has not been executed in this checkpoint. The full Facet suite and comprehensive visual/motion captures have also not been rerun. Existing GUI gaps remain in `gui-remaining.md` and the tracked inventories in `docs/reviews/gui-checkpoint-2026-09-30/`.

## Resumed readiness — 2026-09-30

The collected checkpoint was pushed to canonical at `f9c158af0dfc229c4567bf5cd725c5c0828043fa`. The following work is being integrated on the continuation worktree; this is not a release verdict.

### Verified since the checkpoint

- The complete replication library suite passed: **178 passed, 0 failed, 1 ignored**, in 287.20 seconds. The five outstanding cases received test repairs in `655701202`: valid-map checksum corruption, process-death GC recovery, strict missing-parent/generation validation without sweeping, competing child-process reference CAS, and proof-bearing typed-V2 cold publication/replay. The tests independently inspect durable file inventories and selected roots. A reclaimed-commit statistics discrepancy is still being investigated; physical reclamation does not prove reporting correctness.
- Explicit `CARGO_BUILD_BUILD_DIR` no longer bypasses the shared build-capacity lease. The process-protocol regression suite passed for explicit-directory contention, preservation of caller placement, independent lanes, serialization, stale-process recovery, and cancellation. The saved Nix environment still requires explicit lane coordination until it incorporates this wrapper revision.
- Current-source CLI, locald, and MCP binaries built successfully in the saved Nix shell. This proves buildability, not live ingest, restart, remote access, or GUI readiness.
- Direct rust-analyzer workspace opening and source analysis of cached `toml 0.8.23/examples/decode.rs` succeed. Offline full Cargo metadata alone is therefore not the sufficient explanation for the owner refusal: rust-analyzer can use its no-dependencies fallback. The package boundary currently drops the concrete Rust authority diagnostic; the owner lane is fixing that before selecting a remedy.

### Product acceptance work

| Boundary | Verified gap | Required acceptance evidence |
|---|---|---|
| Owner/compiler | Some package errors lose their typed diagnostics; desktop cancellation does not name an owner-admitted job. | Real Add and cancel preserve synchronous CLI/MCP terminal behavior, expose stage/package progress, keep reads responsive, preserve prior selection on refusal/cancellation, and recover from process death. |
| Persistent search | Semantic and discovery queries construct in-memory Tantivy indexes; durable Turso projection and durable Tantivy primitives exist separately. | Production serving binds persisted postings to the exact selected root, updates and deletes correctly, rejects stale/incomplete projections, and reproduces real queries after a cold process restart and backup restore. |
| Typed IR history | V3 producer and bounded verifier exist, but the production writer still emits ordinal 1 MiB core segments. | Owner-selected native images produce typed stable-key segments, durable history/ref publication, cold replay, and client hydration. Admission binds exact source/capture/selection identity; unproven read-frontier coverage must remain unproven. Whole-family scans must not be described as changed-key compilation. |
| Remote client | Authenticated Iroh compiler dispatch/results work; client queries and hydration are local Unix-socket operations. | An independently running authenticated Iroh client performs product queries and bounded verified range hydration, with exact selection binding, backpressure, interrupted transfer, restart/resume, and capability rejection. |
| Embeddings | Verified external inference and separate embedding planes exist; native GPU inference and durable changed-content reuse are not established. | Real model/provider correctness, typed content/model/tokenizer keys, bounded batch buffers, changed/deleted vector publication, withdrawal and restart recovery, and measured cold/warm RAM/VRAM and work counts. |
| Registries/advisories | Seven acquisition providers and typed facts exist, but browse-field/source completeness varies. | Real pinned packages from every supported registry, source-stamped missing/known-empty distinctions, yank and advisory changes, bounded refresh/backoff, and dependency/dependent consistency. |
| GUI data | Releases and graph still have reachable fixture-backed production paths; Settings and several navigation paths remain incomplete. | Real owner-backed release comparisons, declarations and relations, Add/settings/MCP setup, all supported language labels/source views, keyboard/focus behavior, and persistence across ordinary first and second launches. |
| GUI presentation | Comprehensive current captures, motion/resize evidence and native packaging checks have not passed. | Inspect actual pixels and crops across components/pages, themes, text scales and window widths; test transition reversal, settlement, focus and input; verify native menus, icon/version provenance, cold launch and diagnosable refusal. No new canvas drawing. |
| Deployment/quality | Existing live backup and worker/hydration journeys exercise different pieces; search-quality corpora do not establish superiority. | One real compiler/index/client journey, running-ingest Turso backups with matching owner CAS/journals, cold restore without sidecars, seven-language CLI/MCP journeys, and independently judged same-scope search accuracy plus same-host timings. |

Implementation lanes use separate worktrees. At most four Cargo/build lanes may run machine-wide, with one Cargo process and one compiler job per lane; source-only lanes wait for a handoff. Each slice is reviewed before integration, and integration requires current-source product journeys and visual inspection rather than accepting an agent's completion summary.

### Integration and adversarial evidence — 2026-09-30, 12:15 UTC

Canonical remains the checkpoint above. These results belong to the continuation and to the explicitly named isolated sources; no subsequent release or deployment is implied.

| Executed check | Source and observed result | Limit of the evidence |
|---|---|---|
| Complete replication library | `3a8` frozen source: 188 passed, 0 failed, 1 ignored; transcript `/tmp/readiness-root-replication-3a8-20260930.log`. | Subsequent observation-carrier and multi-image history changes have not passed this suite. |
| Advisory and local service | `fe544f4b4`: advisory 48 passed; local service 689 passed, 0 failed, 11 ignored, 1 explicitly filtered; transcript `/tmp/readiness-root-owner-registry-20260930-pass3.log`. | The filtered real multi-image history/CAS case is a known failure, not a passing test. |
| Search, platform, presentation | `e73761e78`: Tantivy 51, platform 21, presentation 70 passed; transcript `/tmp/readiness-root-integrated-search-platform-e737-20260930.log`. | The later borrowed revision planner and exact cold posting-cover guard require their own composed-source validation. |
| Borrowed lexical revisions | Isolated `dd38cde98`: Tantivy 53 passed; local service 689 passed with the same known multi-image CAS failure and 11 ignored. | The later surplus-posting and merge fixtures are source changes, not executed evidence yet. |
| Client and remote surfaces | Isolated `bdda355ad`: client 16, CLI 34, transport 32, MCP 88 passed. Lease-completion regression at `551313038`: 4 passed. | Composed remote-service tests still expose two fixture failures; the Turso barrier race also fails. Neither is excluded from acceptance. |
| Native components | Isolated QA `47306583c`: Facet 834 passed, 0 failed, 5 ignored; focused Folio 26 passed. | New protected wrapping, modal scrolling, settings, resize and keyboard changes require a fresh build and captures. Desktop's complete suite and installed-product journeys remain open. |
| Persistent embedding supervisor | Isolated `e267d0556`: 13 persistent tests passed, 1 ignored; full compile library 89 passed, 1 ignored in a serial run. | Adversarial review subsequently found repeated process-group retirement and an escaped descendant holding pipes that can hang teardown. These results do not establish bounded teardown. A parallel-suite timing failure also needs deterministic synchronization. |

The immutable runtime bundle `fe544f4b46b9-20260930T113550Z` has manifest SHA-256 `5bcc3f7f96d2da9238731686d187e21b698b2afbc646d197a73dc5605f2e31dc`. Its CLI/locald/MCP hashes and build environment are recorded in `.local/readiness/bundles/fe544f4b46b9-20260930T113550Z/manifest.json`. Fresh startup now passes the private registry-directory and authority-file checks. Its first real, 338-byte Rust ingest then terminates as Cancelled without a cancellation request, before semantic publication. A separate typed-ticket diagnostic observes the same result through one persistent MCP process; its report is `.local/live-index-stress-fe544-typed-diagnostic/20260930T114540Z-62238/artifacts/report.json`. An active attempt also remains pending after cold reopen. No backup/restore or all-language acceptance gate advanced past this failure.

The confirmed cancellation cause is the compiler request lease's unconditional cancellation on Drop after receiving a terminal response. The continuation integrates completion-aware affine request retirement at `38aec0740`, exact durable attempt retirement at `21affd0db`, and terminal-only cleanup inventory at `02503b617`. The latter also strengthens the test to reopen the database with a real prior selected generation. A new immutable bundle must prove ordinary compilation publishes, an exact requested cancellation preserves the prior selection, immutable cancellation receipts survive cold reopen, and no terminated attempt remains pending. The old cancelled run does not establish whether the hidden compiler response itself was successful.

Terminal attempt retirement also exposes a separate remote cleanup interaction: the no-result barrier must reopen the exact terminal receipt after its active row is removed. Barrier retries must obtain a control epoch newer than worker-observed work while retaining `retired_through_epoch` equal to the old terminal attempt. The current concurrent barrier/begin test fails and is under repair; it is not a test to weaken or skip.

The real Maven journal experiment at isolated `a6ab8263` uses 100 discovery events, 96 identities, independent labels, nine queries and 21 exact pagination rounds across initial and cold serving. The durable search projection occupies 234,945 bytes; initial CLI round-trip p50/p95 is 294/816 ms, cold 296/522 ms, and peak sampled aggregate RSS is 169,181,184 bytes. The report is `.local/live-maven-replay/20260930T054321Z/case/runtime/durable-index-benchmark-a6ab8263/report.json`. These are debug process-and-CLI timings on a small real discovery corpus, not isolated index latency, acquired package IR, a release budget verdict, or evidence of superiority to lib.rs. A direct production-API benchmark now separates durable build, cold admission, query chains, candidate work, RSS and projection bytes; it has not executed yet.

The current native pixels show real typed `j` input and a single settled focus outline. They also show a Heads dialog clipping its final finding. The preserved failure is `.local/native-evidence/readiness-harness-20260930/native-current-3c3debb73-b521cc4a-tokio-200/tokio-heads-sheet-text200/f03-1600ms.png`. A shared native dialog scroll handle, title probe and last-finding keyboard/wheel journey are being composed. The focus gate now derives settlement from the actual spring's measured budget plus a quiet interval, rather than treating a still-moving 600 ms frame as settled. Fixture screenshots validate rendering and interaction only; the fixture's explicit search failure is not a live search pass.

Build storage is also an acceptance boundary. Active role directories are unstamped and contain mixed worktree provenance; they cannot be relabelled as clean graph reuse. Four exclusive build lanes remain enforced by orchestration while the new wrapper isolates mutable Cargo graphs, records source/binary provenance and reuses only content-addressed compiler-cache outputs. Adoption and storage reclamation must preserve frozen evidence, avoid active-directory mutation and avoid rebuilding all dependencies into an already constrained volume.

### Continuation evidence — 2026-09-30, 23:37 UTC

Canonical is still `f9c158af0dfc229c4567bf5cd725c5c0828043fa`. Primary implementation source is `4e0b8d5344288cd735fbf1849d8ba833f3298d38`; later isolated source and captures are named below. The full readiness gate remains open.

- The integrated Tantivy library suite passes **57 tests**, including deterministic tombstone/merge work, exact cold posting admission and durable selected-root behavior. Transcript `/Users/mileswirht/Downloads/backend/.local/readiness/evidence/primary-4e0b8d53-tantivy57/readiness-root-tantivy-components-final.log`, SHA-256 `15548afe5b58cb020015ea767f8c638024c50d38490d0d43b54e9193d369e0ad`. This does not validate the new admitted-delta product caller on the isolated durable branch.
- The direct production Maven API experiment now passes independent membership and count oracles before and after same-process reopen: nine query chains and 900 warm samples. Its exact debug timings and limitations are in `docs/benchmarks/live-index-readiness-2026-09-30.md`. It still does not establish ranking superiority or large-corpus production performance.
- Bundle `1a6b54a8` passes authored Rust publication, explicit V2 cancellation, immutable terminal retirement, and same-MCP-process reconnect after an owner restart. The earlier cancellation failure is preserved as historical evidence. This bundle still refuses public `serde` at `build.rs`; Go configuration is absent in its seven-language run and semantic health is 0/18. Public-package backup/restore remains unproved.
- Composed GUI source corresponding to checkpoint `92b01f4bd` passes **842 Facet tests**, with five ignored. Transcript SHA-256 `e3712b2bb578da662b9b8bb61da6800894c6fbcd73fda48291717f3dc5d9c370`. Subsequent desktop capture compilation exposed real integration errors. Actual native pixels at `f0fc2bdad` then exposed 320px/200% text overlap that those tests missed. The new `06d31958a` capture fixes the observed description/byline overlap, but all pages, responsive cards, motion, focus and live-owner flows still require visual and runtime scrutiny.
- The real multi-image history fixture compiles on the isolated history branch, but publication still rejects a source-specific image recipe compared against its package invocation recipe. The producer has two different identities; dropping the comparison without an admitted link is not an acceptable fix. Complete multi-image publication and cold lineage replay are still unverified.
- The persistent compiler filter passes 17 tests. A later full suite passes 96 tests only with a provisional Darwin sentinel workaround. Lead and MAX review reject that workaround as containment evidence: adding an eligible sentinel can make a group signal succeed while an ineligible member survives. The source is not integrated; bounded, correctly classified retirement remains required. Apple's [kernel implementation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c) counts eligible signal targets, rather than proving every member exited.
- Registry policy CAS/queue source review finds no concrete defect, but the held-root archive work still has two confirmed gaps: path-based scan reopening can race a swap-and-restore, and cached extracted files/generated Cargo manifests are not verified against the archive's content. `8ab38384f` improves extraction and identity checks; it does not close either admission gap by itself.
- Stable lexical keys and a sealed production delta path are being composed, but semantic corpus rebuilding and global candidate rekeying remain O(N). Shared V3 observation receipts and the 50,000-lineage refs-map/tree admission work are source changes under review, not performance or production readiness claims.

The four assigned build lanes remain lead, shell, sym and QA, with one job each and incremental compilation disabled. Two path-placement mistakes and an unauthorized V3 build were identified; their outputs are not treated as validated role reuse. A confirmed unused incremental directory and the V3 agent’s abandoned host-default `target` were removed after checking active processes; compiler metadata from the latter was preserved as diagnostic evidence only. Further builds and optimized benchmarks are paused when storage headroom is insufficient; sources, failures and frozen runtime evidence are retained.
