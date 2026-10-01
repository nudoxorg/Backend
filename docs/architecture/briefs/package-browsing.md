# Package browsing: beating lib.rs, crates.io and npm

Handoff brief. Written by the W-Browse finisher (Sonnet), 2026-09-27, at HEAD
`92f974b370660b8273147886893703d3305e99c4` on branch `canonical`. The next agent on this
component has no access to the lead's scratch folder or to the conversations that produced
this brief — everything it needs to act is either in this file, in the copied source
documents under `docs/architecture/briefs/package-browsing/`, or cited by exact `path:line`
in the repository itself. Where a claim about code could not be verified at HEAD in this
pass, it says "unverified" rather than guessing. Where the lead's ruling and a slice's own
proposal disagree, the ruling wins and this brief says so explicitly.

---

## 1. Purpose

The owner, verbatim (recorded in the W-Browse Opus lane's first message, itself quoting the
owner): "want to build this to be first class, in a way that beats lib.rs and crates.io and
npm and other while being more elegant and useful too." Also, from `DIRECTIONS.md` §6:
"Package browsing should be first class", better than lib.rs, crates.io and npm, "and more
elegant."

**What each of those does, and what "beats" it means concretely.** This is the concept
lane's own comparison (`docs/architecture/briefs/package-browsing/browse-concept.md` §1,
copied verbatim from `CONCEPT.md`; every registry there was read live on 2026-09-26), tightened
to what is actually implemented or planned here:

- **lib.rs.** Right: a quality-blended rank (not raw downloads); shows a crate's *weight*
  ("~12–48 MB ~836K SLoC"); "used in 46,102 crates (30,561 directly)" separates direct from
  transitive use; a category rank; the only registry that names alternatives; flags
  deprecated/unmaintained crates; fast, server-rendered. Wrong: cost is absolute — reqwest's
  12–48 MB ignores that you already have hyper and tokio; a result is a README blurb plus
  version/downloads/hashtags, never what the crate lets you *call*; alternatives come from
  co-occurrence with no basis for comparison; stability is a release count, not an API diff;
  no cross-ecosystem view.
  **We beat it by:** costing a candidate against *your* actual lockfile ("adds 2 crates:
  toml_writer, serde_spanned" or "already in your tree"), by answering with the exact item
  that does the job (`toml::from_str(text) → your type`) instead of a blurb, and by measuring
  churn from real API diffs instead of a release count (§4, §6).
- **crates.io.** Right: exact-match-first search; a copyable `cargo add` line; per-version
  facts (rust_version, edition, crate_size, license, features, yanked); a security/RustSec
  tab. Wrong: result rows lead with download counts, so popularity stands in for fitness;
  search matches only name/description/keywords; dependencies are a flat list with no "what
  for" attached; "Dependents" is a count with no reason; the README *is* the page; nothing
  knows your project, and there is no compare.
  **We beat it by:** ranking by what answers the query, not downloads (§4's verdict rule);
  by attaching a reason to every dependency link ("toml → serde: Serialize/Deserialize on
  Value · 38 uses", `DIRECTIONS.md` §1); and by a real compare view anchored on what your own
  code already calls (§6, ruling 1).
- **npm.** Right: a copyable install line with a TS-types badge in the header; unpacked size
  and file count; provenance; dist-tags; a Code tab. Wrong: keywords game the ranking
  ("toml" returns ESLint parsers in the top 5); the p/q/m score is dead (`quality 1,
  popularity 1, maintenance 1` for every result); cost and compare live on other sites
  (bundlephobia, npmtrends); it never says the platform already does the job.
  **We beat it by:** ranking by what the code actually contains (shared public items, shape
  match), not keywords, and by saying "you need no package" first when the standard library
  answers (§3, §7).
- **PyPI** (also read, mentioned for completeness though not a headline target): classifier
  facets are real structure, but search is weak, dependencies are hidden, and it can never
  tell you `tomllib` is already in the standard library where `toml` would sell you a package
  you don't need. Our "you already have this" verdict (§4) is the generalization of that one
  fact.

The "beats lib.rs/crates.io/npm" promise is not a slogan in this codebase: it is implemented,
for S1, as three real, checkable facts about *this* repository that none of those four
registries could produce — "bincode 1.3.3 is unmaintained" (with the exact why-path through
`syntect`), "60 crates are here twice" (host-only count), and every dependency's role derived
from evidence you can see on hover, never a hand-written blurb. See §2.

---

## 2. Where it stands at HEAD

**The headline fact for whoever picks this up: S1 ("Your tree") is not a half-finished
slice waiting on a plan review — it is built end to end, backend to pixel, and was already
committed at HEAD before this brief was written.** The Opus W-Browse lane's own transcript
(`~/.claude/projects/-Users-mileswirht-Downloads-backend/4821aaf6-28dc-4f75-b07b-d8fc41618656/
subagents/agent-ad4b4a1cbd2495ac2.jsonl`) shows it stopping mid-session at "Now the harness
route word and scene for the tree" — but the owner's merge commit `b6149319c` ("Merge canonical
browse and design work into the landed PR tree") landed a considerably more complete slice than
that transcript alone would suggest, including the harness route word and scene themselves.
This finisher's own job, then, was mostly verification and one real bug fix (§2.3), not
building S1 from scratch. **Do not re-plan or re-build anything below; it exists and its
tests pass.**

### 2.1 File map (`path:line`, checked at HEAD)

**Backend, pure logic (`crates/library/browse/*`), no I/O:**
- `crates/library/browse.rs:1-22` — module root, re-exports `tree`, `roles`, `cargo`.
- `crates/library/browse/tree.rs` (715 lines):
  - `PROJECT_TREE_SCHEMA: u16 = 1` (`:13`), `MAX_TREE_PACKAGES: usize = 20_000` (`:16`).
  - `TreeSource::{Cargo{host}, Lockfile{reason}}` (`:21-33`).
  - `ProjectTree` (`:206-227`): `schema, source, root, name, members, packages, direct, twice,
    health, other_platforms`.
  - `TreePackage.role: PackageRole` (`:76-87, 92-105`) — `Direct | Brought(RoleId) | Shared |
    Unreached`.
  - `DirectDependency` (`:122-137`) — `name, versions, by: [MemberEdge], description, role:
    RoleId, evidence: RoleEvidence`.
  - `Duplicate`/`DuplicateCopy` (`:140-167`) — every copy's why-path, whether it's yours,
    `asked_by`, `only_yours`, `also_asked_by`.
  - `TreeHealth`/`TreeAdvisory` (`:169-201`) — coverage, freshness, checked/of, affecting.
  - `build_tree(input: &TreeInput, advisories: &dyn AdvisoryObserver) -> ProjectTree` (`:326`)
    — the pure builder: BFS why-paths with a bin-first-then-name tie-break (`:356-397`),
    role assignment with cohort inheritance (`:399-462`), duplicate/class detection
    (`:526-564`), advisory rollup (`:566-596`).
  - `shared_prefix` (`:650-669`) — drops the longest shared `word-` prefix from member names
    (`backend-desktop` → `desktop`), derived, not hard-coded.
- `crates/library/browse/roles.rs` (273 lines): `RoleId` (13 variants including `Other`,
  `:18-84`), `RoleEvidence::{DevOnly, Declared{words}, Described{phrase}, Cohort{peer},
  Unknown}` (`:87-110`), the vote table `RULES` (12 rows, category+keyword based, `:120-186`),
  `PHRASES` (description fallback, `:189-194`), `declared_role`/`described_role`/`cohort_role`
  (`:206, 236, 261`).
- `crates/library/browse/cargo.rs` (264 lines): `metadata_input` (`:57`, reads `cargo
  metadata --format-version 1 --filter-platform <host>` JSON), `lockfile_input` (`:209`, the
  Cargo.lock-only fallback — every platform counts, no per-package metadata, roles fall to
  `Other`), `locked_packages` (`:172`, a real TOML parse via the `toml` crate, not a line
  scan).
- `crates/library/browse/tests.rs` (7 tests as of this session — added
  `a_direct_dependency_is_marked_direct_and_a_transitive_one_is_not`, §7) — reads this
  repository's own pinned tree fixture and the real `RUSTSEC-2025-0141`; see §2.2.

**Advisory fixes (`crates/advisory`), the four the design called for:**
- The Markdown `` ```toml ``` `` fence around the RustSec source now parses
  (`crates/advisory/src/parse.rs:132-145`, doc comment at `:138`).
- `informational` is read in both forms — RustSec `[advisory].informational`
  (`parse.rs:189-196`) and OSV `affected[].database_specific.informational`
  (`parse.rs:707-709`) — mapped by `informational_category` (`parse.rs:724-731`) to
  `Unmaintained | Unsound | Notice`, and an informational advisory is never defaulted to
  `Vulnerability`.
- `Advisory.summary: Option<String>` (`model.rs:308-312`) and
  `AdvisorySurfaceDto.summary` (`wire.rs:51`, populated at `:112`) carry the RustSec title /
  OSV summary through to the wire.
- `refresh_authority_source` (`crates/local-service/src/builtin/registry.rs:615`, called at
  `:128`) now has a caller — it no longer sits dead as the plan's §0.2 found it.

**Wire / CLI / MCP (house pattern, `crates/library`):**
- `CommandId::{ProjectTree, AdvisoryRefresh}` (`command.rs:115,117`).
- `COMMANDS: [CommandSpec; 42]` (`command_registry.rs:89`) — grew from the pre-browse 40; the
  two new rows are at `:410-425` (`project-tree`, Read/Home; `advisory-refresh`, Write/System).
- `SurfaceCommand::ProjectTree{root}` / `AdvisoryRefresh` and their replies
  `SurfaceReply::ProjectTree(Box<ProjectTree>)` / `AdvisoryRefreshed(Box<[AdvisorySourceState]>)`
  (`surface.rs:536-541, 1169-1171`, `id()` arms at `:578-579, 1208-1209`).
- CLI grammar row `project-tree` → tool name `backend.project_tree` (`present/grammar.rs:
  887-889`), so it appears in the CLI and MCP tables automatically (the house parity rule,
  no per-surface code needed).
- `DTO_VERSION` is `8` (`crates/library/wire/mod.rs:65`). W-Browse rode W-Facts' 7→8 bump
  exactly as PLAN §2.3/decision D1 recommended — no separate bump was taken. **Every S2+
  command listed in §5 below still needs to be added; none of `find`, `package-api`,
  `package-uses`, `release-history`, `release-diff`, `judge`, `compare`, `dependency-plan`,
  `dependency-add` or `package-progress` exist in `COMMANDS` yet** (checked by grep at HEAD:
  zero hits). S1 really is the entire footprint so far.

**Local-service (`crates/local-service`), the I/O layer:**
- `crates/local-service/src/builtin/browse.rs` (328 lines): `BrowseCache` (`:32-42`),
  `project_tree()` (`:46-71`, folds in the advisory authority), `input()`
  (`:73-95`, the cache: keyed by `(workspace, blake3(file bytes))`, see §7 for an unproven
  seam here), `read_project()` (`:117-135`, tries `cargo metadata` first, falls back to
  `lockfile_input` on any Cargo failure, with the real failure reason carried through),
  `workspace_root()` (`:140-155`, walks up to the nearest `[workspace]` manifest, or the
  nearest `Cargo.toml` at all), `cargo_metadata()` (`:210-224`, `--offline --locked
  --format-version 1 --filter-platform <host>`, 64 MB bound, 90 s deadline, `:24-28`),
  `cargo_program()` (`:228-252`, `NUDOX_CARGO` → beside `NUDOX_RUSTC` → PATH → common
  install locations).

**Product words (`crates/present`), one authority for every surface:**
- `crates/present/browse.rs` (379 lines): `TreeReading`/`AlertReading`/`RoleReading`/
  `RowReading`/`TwiceReading` (`:14-99`), `read_tree()` (`:103-177`, "Your 44 packages lean on
  75 others directly, and 884 in all.", the amber/coral alert line, "N crates are here
  twice"), `role_label()` (`:180-197`, the 13 role sentences, e.g. "speaks formats", "draws
  the window"), `why_line()`/`display_version()` (`:199-215`, strips build metadata for
  display, keeps it under ⌥ — actually: the ⌥ spelling is the design's intent from
  `CHECKPOINT.md`; the code here always strips it, ⌥ is not yet wired at this layer, see §7),
  `twice()` (`:324-365`, the four verdict sentences: "every copy is yours", "moving yours…
  drops a copy", "moving yours… keeps both: … still ask", "neither/none is yours to move"),
  `count()` (`:369-379`, thousands separator).
- `crates/present/browse_tests.rs` (101 lines, 4 tests).

**GUI (`apps/facet` + `apps/desktop`):**
- `apps/facet/src/browse.rs` (6 lines) — `pub mod library;` plus re-exports.
- `apps/facet/src/browse/library.rs` (473 lines) — the `Library` component. `library()`
  (`:115-121`), `Model`/`Role`/`Row`/`Twice`/`Alert` (`:36-108`), `hero()` (`:188-238`, one
  gem + one sentence + the alert/fact line, no box around the alert per ruling 3), `roles()`
  (`:241-277`, one or two balanced columns by row-count height), `role_block`/`row_view`
  (`:279-333`), `card_on_rest()` (`:344-387`, the why-card on hover, no separate float kind —
  reuses `overlay::float`), `twice_block`/`twice_row` (`:389-473`, each duplicate opens in
  place to show both paths and the verdict, closed by default beyond the first
  `TWICE_AT_REST = 8` (`:111`)). It stands in with `paint::gem` and plain mono version text
  for the marks the design calls for (`ecosystem_mark`, `license_mark`, `version`); see §8.
- `apps/desktop/src/navigation/route.rs:71-79` — `OrbitRoute::Browse(BrowseRoute)`, nested
  under Orbit depth (so the shelf's Library head, `orbit_rows`, governs it too — this is
  exactly the seam this finisher's bug was in, §2.3).
- `apps/desktop/src/navigation/browse.rs` (53 lines) — `BrowseRoute::Tree(LocalProjectId)`,
  `address()` → `nudox://<project>/tree`, `here()` → "your tree".
- `apps/desktop/src/model/browse/mod.rs` (58 lines) — `BrowseKey::Tree`, `BrowseValue::Tree`,
  `TreeModel { root, reading: backend_present::TreeReading }`.
- `apps/desktop/src/model/pages/key.rs:56` — `PageKey::Browse(BrowseKey)`.
- `apps/desktop/src/runtime/browse_reads.rs` (45 lines) — `compose()` (`:15-36`, one
  `SurfaceCommand::ProjectTree` round trip), `tree_model()` (`:40-45`). Wired into the read
  dispatcher at `apps/desktop/src/runtime/reads.rs:73,87,646`.
- `apps/desktop/src/shell/bodies/browse.rs` (120 lines) — `body()` (`:14-27`), the
  `LibraryModel` assembly with `unbroken_hops()` (`:29-36`, keeps a `→`-joined path from
  wrapping mid-hop). Registered at `apps/desktop/src/shell/bodies/mod.rs:142,162-163,212`
  (`Route::Orbit(OrbitRoute::Browse(browse)) => browse::body(...)`).
- `apps/desktop/src/harness.rs` — the route word `tree` (`:314`) and the scene
  `desktop-tree` (`:764-768`) both already existed at HEAD before this finisher started; this
  finisher's fix (§2.3) changed what project the route resolves to and added a workspace
  registration, all inside this one file.
- `apps/desktop/src/shell/browse_tests.rs` (145 lines) — the real end-to-end test,
  `the_library_page_shows_a_real_tree_read_by_a_real_owner` (`:70-145`): a real embedded
  `backend-locald`, configured with the real `RUSTSEC-2025-0141` file, reads a real two-member
  Cargo workspace, and the test asserts the rendered sentences on screen, never counts alone.
  It also proves the same owner reads a *second*, different project (this repository itself)
  without serving the first project's cached tree (`:119-144`).
- `apps/desktop/tests/fixtures/browse_tree/{Cargo.toml,Cargo.lock,app/,tool/}` — the pinned,
  tiny, real fixture: `app` depends on `toml = "0.8.23"` and `bincode = "=1.3.3"`
  (unmaintained), `tool` depends on `trybuild` which pulls `toml` 1.x, so `toml` is a real,
  resolvable duplicate on this fixture too.

### 2.2 What renders, and what it says (verified by reading the component and by the real
end-to-end test, §4)

The Library page (`nudox://<project>/tree`) shows, top to bottom: one gem + the project's
folder name; one lede sentence ("Your 44 packages lean on 75 others directly, and 884 in
all.") with a hover tip naming what builds only for other platforms; a line of quiet facts
(advisory coverage, "N crates are here twice") plus, in amber or coral, one line per advisory
that affects the tree (hover opens a small card with the advisory id, its title, and the full
why-path); the roles, each a small headed block ("speaks formats", "for advisory, desktop,
engine and local-service", then its dependency rows, each showing its name and at most one
quiet descriptor — "tests only", or "twice · 0.8.23 · 1.1.5" — with the full evidence
sentence and the dependency's own one-line description on hover); then, if any package is
duplicated, a "Here twice" section where each row opens in place (click, not a new page) to
show every copy's path from your code and the verdict sentence.

Real findings, verified on this repository's own pinned tree fixture
(`crates/library/browse/fixtures/tree-2026-09-27/`, captured 2026-09-27): 44 workspace
members, 75 direct dependencies, 884 packages that build for `aarch64-apple-darwin`, 309 more
that build only for other platforms, 60 duplicated crates on this host (114 across every
platform, per the earlier prototype's all-platform count — see §4 for which count each figure
belongs to). `bincode 1.3.3 is unmaintained` (RUSTSEC-2025-0141, the real advisory, "Bincode
is unmaintained"), reached `desktop → gpui_ce_components 0.2.0 → gpui_ce_components_base
0.2.0 → syntect 5.3.0 → bincode 1.3.3`. `toml` is here twice: `desktop → toml 0.8.23` (yours)
and `frontend-rust → ra_ap_project_model 0.0.341 → toml 1.1.5` — and the real resolved graph
says the prototype's guess ("moving yours would drop a copy") was **wrong**: `cbindgen` and
`rust-i18n-support` still ask for the 0.8 line, so the true sentence is "Moving yours to 1.1.5
keeps both: cbindgen and rust-i18n-support still ask for 0.8." This is exactly the kind of
honesty the design called for (`browse/RULINGS.md`, "the index can answer this… name matching
can't") and it is already what ships.

**How to run it.** In the real desktop app, open any Cargo project — `BrowseCache` resolves
the nearest `[workspace]` manifest above the project root, or the nearest bare `Cargo.toml`
(`crates/local-service/src/builtin/browse.rs:140-155`) — and navigate to its tree route. On
the CLI/MCP, `backend project-tree <path>` / `backend.project_tree`. In the visual harness,
`backend-desktop-gui-harness capture --scene desktop-tree …` (see §9 for the exact command;
this finisher used it successfully once the build below was unblocked).

### 2.3 The bug this finisher found and fixed

The lead walked the real app on a 16:30 harness build and found (evidence: `$S/wave4/walk/
FINDINGS.md` item "Tree (browse S1)" and `$S/wave4/walk/tree-small.png`, `$S` being the lead's
scratch folder, not preserved in the repo): the `desktop-tree` scene's body read the **live
repository** — "Your 45 packages lean on 75 others directly, and 884 in all", a number that
drifts with every commit — while the shelf beside it, built from the harness's own fixture
snapshot, read "Library 0 projects · 5 packages". A harness scene must be deterministic, and
the shelf and body must describe the same workspace; neither held.

Root cause, read at HEAD before the fix: the route word `tree` resolved via
`super::repo().canonicalize()` (the actual checkout of this repository), not any of the
harness's five indexed fixture roots (`apps/desktop/src/harness.rs:158-167`) —
`BrowseCache::project_tree` runs `cargo metadata` directly on whatever path it is given, never
through the fixture owner's search index, so nothing stopped it from pointing outside the
fixture entirely. Separately, `boot()` never registered any project into `WorkspaceState.
projects` (the count the shelf's Library head shows, `apps/desktop/src/shell/shelf.rs:
303-357`), so every scene showed "0 projects" regardless of what the body displayed.

The fix, entirely inside `apps/desktop/src/harness.rs` (no other lane's files touched):
`browse_tree_root()` (`:99-104`) points the `tree` route at the pinned
`apps/desktop/tests/fixtures/browse_tree` fixture instead of the live repository;
`tree_workspace()` (`:105-116`) registers that project's `LocalProjectId` into
`WorkspaceState` (Ready, active, host) so the shelf agrees with the body; `boot()`
(`:497-539`) now resolves the route before building the snapshot and folds the workspace in
only for the tree route, leaving every other scene's construction untouched. A regression
test, `the_tree_route_is_pinned_to_the_fixture_workspace_not_the_live_repository`
(`:810-818`), asserts the route's target path is never the live repository. Full detail,
including why an earlier approach (forcing a `cargo` failure via an env var) had to be
abandoned (the crate denies `unsafe_code`), is in
`docs/architecture/briefs/package-browsing/browse-impl-checkpoint-1.md` §2 and §5.3.

Verified this session, twice, green (§4): `cargo test -p backend-desktop --features
visual-harness --lib harness::tests` (2 passed) and `cargo test -p backend-desktop browse` (2
passed, including the real end-to-end test). All four requested captures of `desktop-tree`
(1440×900 and 760×900, Abyss and Glacier) were taken during this session and confirm the fix
by eye: the shelf reads "Library · 1 project · 5 packages" naming `browse_tree`, matching the
titlebar and the body's hero, in every one; see §3 and §9 for the exact command and what each
capture shows. Files: `$S/wave4/browse-impl/stills-finisher/{1440-abyss,1440-glacier,
760-abyss,760-glacier}/`, scratch evidence, not preserved in the repo — copy them out before
the scratchpad is cleaned if you need the pixels.

---

## 3. The design

### 3.1 The prototype and its URL

Working prototype: `Nudox-Design-System/v4/browse/Browse.html`, `browse.css`, `browse.js`,
built from real data on this machine by `build_data.py` → `data.json` (2.7 MB, offline, ~10s;
`--net` refreshes `cache/net.json`). Serve `Nudox-Design-System/` (`python3 -m http.server
47811 --bind 127.0.0.1`) and open `http://127.0.0.1:47811/v4/browse/Browse.html`.

Every URL parameter, read from `browse.js` (grepped for `P.get`/`P.has` at HEAD, so this list
is exact, not remembered):
- `q=` — the query. Four kinds are read from its shape: words (`parse toml`), a shape
  (`text -> Value`), a package you know (`like serde_json`), or pasted code
  (`serde_json::from_str::<Config>(&s)?`).
- `view=` — `find` (default) | `compare` (implied by `compare=`) | `tree`.
- `compare=a,b,c` — 2–4 candidates, opens the compare view.
- `preview=<candidate>` — shows your code rewritten for that candidate (the adoption preview).
- `sel=<n>` — the selected row index.
- `hover=<pkg>` — hovers a row or column.
- `row=<n>` — unfolds one compare row's signatures (only in `view=compare`).
- `why=<crate>` — opens a package's why-path in the tree view.
- `alt=1` / `cmd=1` — holds ⌥ / ⌘ (the x-ray and key-foot states).
- `hand=a,b` — packages gathered in the hand tray.
- `w=` — window width override; `zoom=` — text-scale multiplier (`200` = 200%);
  `scroll=<id>` — scrolls to an element on load.
- `reduced=1` — forces reduced motion (also honours the real media query).
- `still=1` — suppresses the initial-focus behaviour (for a clean screenshot).
- `t=` — freezes a motion at this millisecond, used with:
  - `add=<pkg>&t=` — the adopt motion frozen at `t`;
  - `to=<query>&t=` — the FLIP re-rank frozen at `t`;
  - `fly=1&t=` — the ask-field-becomes-hero motion frozen at `t`.
- `hovermark=` — hovers a specific mark (used for D-Marks stills inside this board).

### 3.2 Every still (`Nudox-Design-System/v4/shots/browse/`, 56 files), one line each

- **Find, empty** — `find-empty-{1440,1100,760,480}.png`: three teaching rows (a need, a
  shape, a package you know) plus a pasted-code row.
- **Find, words** — `find-words-{1440,1100,760,480,200pct}.png`: "You already have this" —
  `toml::from_str` ranks first because your code already calls it 4 times; basic-toml
  ("adds only itself") and toml_edit ("already in your tree") follow; cousins (tomllib from
  the standard library, plus 4 folded) sit under "the same idea elsewhere".
- **Find, "no package needed"** — `find-std-1440.png`: "read a file into a string" → std
  `fs::read_to_string`, from real std sources.
- **Find, shape** — `find-shape-{1440,1100,760,480}.png`: `text -> Value` → `toml::
  from_str::<Value>`; a generic `T: Deserialize` output can *be* `Value` because toml's
  `Value` implements `Deserialize` (read from scanned impls).
- **Find, like** — `find-like-{1440,1100,760,480}.png`: `like serde_json` ranked by shared
  items, not keywords ("shares from_str and to_string +19").
- **Find, pasted code** — `find-code-{1440,760}.png`: your line rewritten per package, ranked
  by how little changes, changed words underlined. (Defect: doc-sentence Markdown backticks
  leak through as literal characters — must not copy this into the product, PLAN §8 item 5.)
- **Find, HTTP family** — `find-http-1440.png`: ureq answers first (already yours); reqwest
  "adds 10 crates"; attohttpc adds 3.
- **Judge** — `judge-{1440,1100,760,480,200pct}.png`: the card (beside the row at ≥1100, else
  inline): why care, the D-Marks comb *as* the version, one serif sentence, the answering
  signature, and a one-line switch verdict.
- **Judge, ⌥** — `judge-alt-1440.png`: churn (claimed, by version numbers), API size, MSRV,
  local dependents, RustSec status, and downloads (the *only* place downloads appear).
- **Judge, ⌘** — `judge-cmd-1440.png`: key feet only while ⌘ is held.
- **Judge, deprecated** — `judge-yaml-1440.png`: serde_yaml, deprecated, "adds 2 crates".
- **Compare** — `compare-{1440,1100,760,480,200pct}.png`: the verdict sentence is the hero;
  three bands (what your code does today, what else it can do, what it costs you).
- **Compare, a row open** — `compare-row-1440.png`: hovering a row unfolds every cell's
  signature; hovering a column lifts it with a `×` to drop it.
- **Compare, costs and preview** — `compare-costs-1440.png`, `compare-preview-{1440,760}.png`:
  the costs band (adds, license fit, breaking releases/year "measured", API size, MSRV,
  advisories); the adoption preview, before/after per line. **Defect the product must not
  copy**: this still says "77 of your 77 lines change only in name" for toml_edit, which
  ruling 1 (§4) forbids.
- **Compare, other families** — `compare-http-1440.png` (ureq vs reqwest vs attohttpc),
  `compare-errors-1440.png` (thiserror vs anyhow vs miette vs snafu — a derive never matches a
  struct of the same name, equivalence is kind-aware).
- **Tree** — `tree-{1440,1100,760,480,200pct}.png`: this is S1, already shipped (§2); compare
  these stills against a fresh `desktop-tree` capture (§9) for layout parity, not for the
  numbers (the prototype's counts are all-platform; the product's are host-only, ruling 3).
- **Tree, hover** — `tree-hover-1440.png`: the judge card plus "In *talks to the network*
  because web-programming::http-client · used by …".
- **Tree, why and twice** — `tree-why-1440.png`: **defects the product must not copy**
  (PLAN §8 item 4): inline mono spans lose surrounding spaces ("Moving yours to1.1.5drops a
  copy"), and a hovered row's evidence overlaps the neighbouring column. The shipped
  `library.rs` avoids both (words are one `StyledText` run per sentence, and the evidence text
  wraps inside its own card, `card_on_rest()`).
- **Adopt** — `adopt-lift-1440.png` (t=300, the lift), `adopt-{1440,1100,760,480}.png`
  (t=700, mid-flight), `adopt-index-1440.png` (t=2300, the four-stage indexing seam).
- **Hand** — `find-hand-1440.png`: ⌘-click or `C` gathers candidates at the window foot
  ("compare three ›" — this is the hand's Row rung, §8).
- **Motion** — `flip-1440.png` (t=55, the FLIP re-rank), `fly-1440.png` (t=70, the ask field
  becoming the hero).

### 3.3 Serving and viewing

`cd Nudox-Design-System && python3 -m http.server 47811 --bind 127.0.0.1`, then open
`http://127.0.0.1:47811/v4/browse/Browse.html?<params from §3.1>`. `snap.sh` takes one still;
`stills.sh` renders every board in §3.2. To regenerate the underlying data,
`python3 v4/browse/build_data.py` (offline; add `--net` for the RustSec/downloads refresh).

---

## 4. Decisions, and the rulings that bind them

D1–D9 are the implementation plan's own decisions (`browse-impl-plan.md` §7), each with the
lead's leaning from the resume message and the final ruling from the plan-approval message.
**Where §7 below and the rulings disagree, the rulings win — they are what the lead actually
approved, not what the plan proposed.**

- **D1 — one speller for shapes, in a new `crates/words` (backend-words) crate.**
  Lead: "go. Create `crates/words` (backend-words) as a pure move with no behaviour change.
  facet's semantics goldens must pass byte-identically before and after (quote both runs)…
  Do it just before S2b, not in S1." **Status at HEAD: not started.**
  `docs/architecture/package-dag.json` still shows `target_packages: 30` and no
  `backend-words` entry (checked this pass) — correct, since S2b (shape search) hasn't
  started either. When it is done: it moves `TypeExpr`/`parse`/`Scope`/`Vocabulary`/`spell`/
  `Piece` out of `apps/facet/src/semantics/types.rs` (1,152 lines today), converting
  `gpui::SharedString` to `Arc<str>` and `Target::Node(NodeId)` to a plain `u32`; facet keeps
  a thin re-exporting `semantics::types` so its 24 call sites don't change. It touches
  W-Anatomy's file, so it needs a fresh `git diff` guard and the lead's go-ahead at the time.
- **D2 — uses tiering.** Ruling: ship tier B ("by path") in S2, labelled "at least N places ·
  matched by path"; tier A ("resolved") is an upgrade, arriving after D8/D9. Not yet built
  (S2 hasn't started).
- **D3 — Cargo as the tree authority, with a lockfile fallback.** Lead's leaning matches the
  plan exactly. **Built and shipped in S1** (§2): `TreeSource::Cargo{host} | Lockfile{reason}`,
  `metadata_input`/`lockfile_input` (`crates/library/browse/cargo.rs`).
- **D4 — a separate, read-only `CargoIndexRow` decoder for the registry index cache, not a
  merge with the existing acquisition decoder.** Lead's leaning matches. Needed for S3
  (release history/diffs); not yet built.
- **D5 — equivalence coverage is project-wide, not line-local.** Lead's leaning matches.
  Needed for S4 (compare); not yet built. The rule (§1.6 of the plan): a candidate type
  stands for an incumbent type only when *every* method your code calls on the incumbent
  type, anywhere in the project, has an equivalent on the candidate — stricter than
  "on that line", because a field's declared type carries uses made elsewhere in the file.
- **D6 — `toml_edit` is fine to depend on**, since it is already built (transitively, via
  `toml`) with the needed features; adding it as a direct dependency of
  `backend-local-service` adds no crate and no second copy. Needed for S5 (adopt's
  format-preserving writer); not yet built.
- **D7 — edits outside W-Browse's own files are tight, additive, one-line, `git diff`-guarded.**
  The full list of files this touches (route.rs, thread.rs/browse.rs, bodies/mod.rs,
  region.rs, ask.rs, model/pages/key.rs, overlay/peek.rs, motion/shared.rs, facet/lib.rs, plus
  the shared `crates/library` registry/surface files) is in the plan §7. **All of S1's D7
  edits are in and clean** — `OrbitRoute::Browse`, `PageKey::Browse`, `facet::lib.rs`'s
  `pub mod browse;` line, the `bodies/mod.rs` registration. Nothing for S2+ has been touched
  yet.
- **D8 — two Rust-frontend changes for resolved ("tier A") uses**: keep what rust-analyzer
  resolved instead of discarding it to a bare `Universe` spelling
  (`frontends/rust/src/legacy/authority.rs:556-568,643`), and give re-exports their resolved
  target as a `Reexports` link. Ruling: after W-Facts' Phase 1 lands (they touch `lower.rs`/
  `rust.rs`), as its own change with its own checkpoint. **Status: unverified whether W-Facts
  Phase 1 has landed as of this HEAD** — check `crates/engine/src/driver/lower/rust.rs` and
  the W-Facts checkpoint under `docs/architecture/briefs/symbol-page/facts-checkpoint-1.md`
  before starting D8.
- **D9 — workspace members get no compiler publication** (rust-analyzer's loader opens a
  member's own root and walks up to the workspace root, so nothing lower than the workspace
  ever gets its own `Row`; `apps/desktop/tests/content_truth.rs:9-13,297-305`). Ruling: "noted.
  I'm raising it with the owner as an engine-level decision. Don't plan it." **Status:
  unverified whether the owner has ruled on this yet.** This blocks tier-A uses, references
  and relations on *this repository's own code*, not just Browse — check with the lead before
  assuming it is still open.

**The rulings, verbatim where they are binding rules** (`browse-rulings.md`, copied in full
at `docs/architecture/briefs/package-browsing/browse-rulings.md`):

1. **The false-equivalence rule.** "A line changes 'only in name' **only when every
   substituted item has the same shape**: parameters in words, output in words, and the
   receiver type. A same-named item with a different shape is 'reads differently', and the
   line shows the difference. A substituted *type* is a rename only if its capabilities cover
   the uses on that line. The index can answer this through shapes; name matching can't.
   Until shapes are available, show the preview as 'matched by name' and never as 'only in
   name'." This is why the prototype's `compare-costs-1440.png` (claiming toml_edit covers
   "77 of 77" lines "only in name") is a named defect, not a model to follow.
2. **The sticky column header rule.** "The header must either fully cover what scrolls under
   it or not overlap it. Legible fragments must never show." (Compare's costs band, in the
   prototype, cuts rows into dotted fragments under a translucent header — forbidden.)
3. **No box on the care line.** "Drop the box: the mint word carries it (§6.2: space, not
   boxes)." — already honoured in the shipped `Library` component (§2.1: `hero()` paints the
   alert directly, no border).
4. **Where Find lives.** "Its own reader route, `nudox://find?q=`. ⌘K Ask offers 'all answers
   as a page ↵' as its last row. Ask stays the quick list." Not yet built (S2).
5. **What "add" writes.** "The target defaults to the member you're reading, otherwise the
   role's main user, with a menu. If the workspace uses `[workspace.dependencies]`, add the
   dependency there and write `x = { workspace = true }` into the member. Otherwise write the
   member's table. Show the exact diff before writing. Never write silently. The write happens
   through the backend, not by shelling out to `cargo add`." Not yet built (S5); this
   repository's own `Cargo.toml` does use `[workspace.dependencies]` (`Cargo.toml:24` onward,
   per the plan's D6 note), so the workspace-table path is the one that will exercise first.
6. **The tree count.** "Count what builds for this host, and put 'and N for other platforms'
   on hover." **Built and shipped in S1** — `ProjectTree.other_platforms`,
   `TreeReading.elsewhere` ("and 309 more for other platforms"). Verified as a real,
   currently-passing behaviour, and separately verified as *breakable*: see the mutation in
   `browse-impl-checkpoint-1.md` §5.1 (a real quoted panic when this line is disabled).
7. **Measured churn.** "Lazy. Measure the last 12 releases when a package is judged or
   compared, cache the result, and read the full history on the package page's release
   lens." Not yet built (S3).
8. **Role vocabulary.** "Derived, not editable in v1. Rename and merge per project come
   later." **Built and shipped in S1** exactly this way — 13 roles, all derived
   (`crates/library/browse/roles.rs`), no per-project override exists or is planned for v1.

**Cargo metadata is the tree authority**: 884 host crates + 309 other-platform crates
(1,193 total across every platform on this repository, as pinned in the 2026-09-27 fixture),
75 direct. **Equivalence is type-aware and project-wide** (D5, ruling 1). **Every count names
its tier** — "N places" only ever appears with a qualifier once uses tiering lands (D2); S1's
own counts (members, direct, packages, other-platforms, duplicates) are exact, not tiered,
because they come from Cargo's own resolution, not from a text scan.

---

## 5. The plan: remaining slices, in order

From `browse-impl-plan.md` §4, with each slice's data need, tests/acceptance, harness scene,
and matching prototype still. **S1 is done (§2); nothing below has been started.**

- **S2 · Find: words, "you already have this", std.**
  - *Data needed*: the §1.4a syntactic API extractor (`frontends/rust/src/api.rs`, new —
    ports the prototype's `extract.mjs`/`build_data.py` regex scan onto tree-sitter-rust,
    which the frontend already embeds); by-path uses (`frontends/rust/src/uses.rs`, new,
    ports `releases.mjs`'s `scanUses`); std read through the same extractor, from the
    toolchain's own `library/{std,core,alloc}` sources; `library::browse::find` (new, pure
    ranking, ports `browse.js`'s word/verdict rules).
  - *Tests/acceptance*: on the existing `frontends/rust/fixtures/toml_pin` fixture (pins
    `toml = "0.8.23"`, calls `toml::from_str` once), plus two newly-added harness roots
    (`basic-toml-0.1.10`, `toml_edit-0.22.27`, both already unpacked in the local registry
    cache — checked by the plan): `find("parse toml")` → verdict `Yours`, row 0 = `toml` /
    `from_str`, "You already have this. from_str from toml — your code calls it in 1 place.";
    `find("read a file into a string")` → verdict `Std`, row 0 = `fs::read_to_string`. The
    mutation: drop noun exclusion so `str::parse` wrongly enters — the test must name it.
  - *Harness scene*: a new `desktop-find` scene, booted at `route find` (a new `Target::Find`
    word alongside `orbit|world|tree|package|symbol`).
  - *Matches*: `find-words-*.png`, `find-std-1440.png`.
- **S2b · Shape, like-X, pasted code** (cheap once D1's shared speller exists).
  - *Data*: the Deserialize-bound rule (a generic `T: Deserialize` output answers `Value` when
    this package's `Value` implements `Deserialize`, from impl facts) needs W-Facts' R1
    (`ImplementationFacts{self_type, contract, blanket}`) — check whether W-Facts Phase 2 has
    landed this (see D8 note above; same dependency).
  - *Tests*: shape query `text -> Value` on the toml_pin fixture; like-X on a small, real
    corpus of already-unpacked crates.
  - *Matches*: `find-shape-*.png`, `find-like-*.png`, `find-code-*.png` (mind the Markdown-
    backtick defect, §3.2).
- **S3 · Judge.**
  - *Data*: `library::browse::index_row::CargoIndexRow` (new, full typed registry-index row:
    `vers, yanked, pubtime, rust_version, deps[...], features`), a `cargo_requirement_matches`
    function in `crates/advisory/src/version.rs` (Cargo semver semantics sharing
    `VersionKey`/`caret_upper`/`tilde_upper` with the RustSec matcher — one semver authority),
    a `cfg(...)` evaluator against the real `rustc --print cfg` host set,
    `library::browse::closure` (dependency closure cost vs the lockfile), `release-history`
    and (lazily, ruling 7) `release-diff`.
  - *Tests*: `release-history toml` from a pinned `.cache` file copy asserts `0.8.23`'s
    `pubtime` and yanked flag; `release-diff smallvec 1.16.0 1.16.1` on real sources must show
    the real (empty) diff — this is the lead's own verified finding ("smallvec's zero diffs
    are TRUE, not a defect" — 58 identical public fns, only formatting differs, per
    `marks/RULINGS.md` item 4) as a test, not an assumption. Mutation: stop erasing parameter
    names, and a rename-only pair must wrongly read "changed".
  - *Harness scene*: `desktop-judge`, booted at a package + `?answering=`.
  - *Matches*: `judge-*.png`.
- **S4 · Compare with incumbent uses.**
  - *Data*: `library::browse::equivalence` (type-aware, D5/ruling 1), `caps` detectors (family
    capability rows, ported from the prototype's `CAPS` table), `compare`, the adoption
    preview.
  - *Tests*: compare toml vs toml_edit vs basic-toml on the fixture's real uses — the
    `as_table` cell must read "reads differently" with the words `(Item) → maybe Table`, and
    no line may say "only in name". Mutation: equate shapes by name only, and the preview
    must wrongly claim "only in name" — this is ruling 1's defect, captured as a test.
  - *Matches*: `compare-*.png` (mind the "77 of 77 only in name" defect — must not reproduce).
- **S5 · Adopt.**
  - *Data*: `dependency-plan`/`dependency-add` (two-phase write: plan shows the exact diff,
    add refuses a stale digest — never write silently, ruling 5), format-preserving via
    `toml_edit` (D6), `package-progress` (reads real acquisition/index-publish stages, not
    invented ones).
  - *Tests*: `dependency-plan` over a temp copy of a workspace with `[workspace.dependencies]`
    asserts both hunks' exact text; `dependency-add` with a stale digest refuses. Mutation:
    skip the digest check, and a stale write must be caught.
  - *Matches*: `adopt-*.png`.
- **Journeys**: a new J7 "browse" (gui-plan §3 item 10 table), ⌘K `parse toml` → "all answers
  as a page ↵" → Find → hover basic-toml → Judge → compare → the preview. Reports `BLOCKED
  (data)` for any reply not yet served, exactly as J2/J3 already do for the upgrade lens.

**Deferred, and why** (plan §6, unchanged by this finisher): cousins in other ecosystems
(nothing from npm/PyPI/Go/Java/.NET/C++ is indexed here; a hand-written table would be the
"invented phrase beats a real list" defect the marks rulings forbid); downloads under ⌥ (a
crates.io API dependency for one secondary number — usage is secondary per gui-plan's house
rules); network registry search (Find v1 only ranks what the index has read, plus your tree,
plus std); OSV zip ingestion; compiler-lane std and compiler-lane per-release diffs (too slow
per judge — twelve rust-analyzer runs per comparison); feature-aware closure beyond default
features; role rename/merge (ruling 8); "N lit in the graph" door counts (needs the graph
lane's own query).

---

## 6. Data sources and contracts

**Real data used, and by what:**
- `cargo metadata --offline --locked --format-version 1 --filter-platform <host>` — S1's tree
  authority (`crates/library/browse/cargo.rs:57`). Resolves features, targets, renames and
  per-package license/description/categories/keywords exactly as a real build would.
- `Cargo.lock`, parsed with the real `toml` crate (not a line scan) — the all-platform count
  and the fallback reader (`lockfile_input`).
- The Cargo sparse-registry index cache, `$CARGO_HOME/registry/index/*/.cache/<shard>/<name>`
  — needed from S3 on for `pubtime`/`yanked`/`rust_version`/`features2` per release; not read
  by anything yet at HEAD (`CargoIndexRow` doesn't exist). The design's own count from this
  cache: 1,642 crates carry `pubtime` (D-Marks' checkpoint, cited in the plan).
- Unpacked crate sources in `$CARGO_HOME/registry/src/…` — needed from S2 on for the API
  extractor; 2,028 unpacked on this machine per `DIRECTIONS.md` §6, not yet read by product
  code (only by the design prototype).
- RustSec `advisory-db` (a local git checkout, walked as a directory,
  `crates/advisory/src/authority.rs:88-118`) and OSV — **already wired and fixed** (§2.1); this
  is real, current advisory data, not a synthetic fixture, for the one advisory the S1 tests
  pin (`RUSTSEC-2025-0141`).
- `Nudox-Design-System/v4/graph/world.json` (56k nodes) and `releases.json` (real API diffs
  for toml and smallvec only) — **prototype-only**. The product's equivalent facts come from
  the index (once built) or the syntactic extractor (§5), never from this file; it is not a
  data source for anything shipped in S1.
- `~/.cargo/registry/…/releases.json` style data is otherwise unused by the shipped product;
  don't confuse the design prototype's `v4/graph/*` files (real but frozen, from one 2026-09-26
  read) with a live data source the product reads at runtime — it does not read them.

**What the product uses versus what the prototype only used**: the prototype's
`build_data.py` computed everything once, offline, into a 2.7 MB `data.json`, including a
hand-written `TWINS` equivalence table, hand-written `COUSINS` (cross-ecosystem), and a
hard-coded macOS `cfg` set. None of those hand-written tables are in the shipped product or
planned to be — equivalence is derived from real shapes (D5), cousins are deferred (§5), and
the `cfg` evaluator is planned to read `rustc --print cfg` for real (S3).

**Other ecosystems.** The app supports seven: cargo, npm, PyPI, Go, Maven, NuGet, Conan
(`docs/architecture/gui-plan.md` §1). The browse plan is Cargo-first throughout — every data
contract in §1 of `browse-impl-plan.md` is written against Cargo's own tools (`cargo
metadata`, the sparse index, the registry source cache). **What the plan says for non-Cargo
projects: nothing yet, and it does not claim otherwise.** The plan's §1.8 (search) and §1.4a
(the API extractor) are both explicitly one-per-language ports of the prototype's Rust-only
scanner; nothing in the plan schedules a second-language extractor. This is an open gap, not
a documented deferral — **flag it to the lead** rather than assuming Cargo-only is
acceptable long-term, since the owner's own framing ("beats npm") implies JS/TS browsing
should exist eventually. Treat "browse works for the other six ecosystems" as **unverified
and unplanned**, not merely deferred.

---

## 7. Known gaps and risks

- **D9 (§4): workspace members get no compiler publication.** This is the single biggest
  blocker for tier-A ("resolved") uses, and it is not Browse-specific — it blocks references
  and relations on the owner's own code everywhere. **Status (the lead, 2026-09-27):** raised
  with the owner; no ruling yet.
  - The precise state at HEAD is in `docs/architecture/briefs/symbol-page.md` §7. For a
    member, `references` now falls back to a structural answer
    (`semantic_query.rs` `execute_references` → `execute_structural_references`, since
    64c36433b2), while typed relations still gap with `NoSemanticPublication`.
  - So a use count on your own members can reach tier B ("at least N places · matched by
    path") from the structural answer, but not tier A ("resolved") until members get a
    compiler publication.
- **The harness fixture index's registry coverage — checked this pass, and the QUEUE.md note
  may be stale.** `wave4/QUEUE.md` (the lead's own queue file) says: "the harness fixture
  index holds only workspace crates, so there's no registry dep (toml), no comb and no
  upgrade lens in the real app." **At HEAD, this is contradicted by the code**:
  `apps/desktop/src/harness.rs:158-167` already indexes `frontends/rust/fixtures/toml_pin`
  (which pins `toml = "0.8.23"`) *and* `registry_source("toml-0.8.23")` (the real unpacked
  toml source from the local cargo registry cache) as two of its five fixture roots. Whether
  W-Shell's comb and upgrade lens now actually render against this is **unverified** (outside
  this lane's files) — but the specific claim "there's no registry dep in the fixture" no
  longer holds. Re-check with W-Journeys/W-Shell before treating this as still-blocking.
- **Two real test gaps found by this finisher's own mutations — both closed, on the lead's
  ruling that S1 does not count as finished until they are** (`browse-impl-checkpoint-1.md`
  §5.2, §5.4 — full detail and quoted panics there):
  - Nothing asserted that a known direct dependency's `TreePackage.role` is actually
    `PackageRole::Direct`. Disabling that classification entirely (`crates/library/browse/
    tree.rs:497`) used to leave all 10 existing S1 tests green. **Closed** by
    `crates/library/browse/tests.rs::
    a_direct_dependency_is_marked_direct_and_a_transitive_one_is_not`, which asserts `toml`
    0.8.23 (real, direct) *is* `PackageRole::Direct` and `bincode` 1.3.3 (real, four hops away,
    transitive) is *not* — so the test cannot pass on "mark everything Direct" either. Kills
    the mutant (quoted panic: `left: Brought(Window)` / `right: Direct`), green twice.
  - Nothing proved `BrowseCache`'s file-witness cache ever invalidates. Disabling the
    staleness check entirely (`crates/local-service/src/builtin/browse.rs:82`) used to leave
    both existing local-service tests green. **Closed** by `crates/local-service/src/builtin/
    browse.rs::tests::
    an_untouched_workspace_is_served_from_the_cache_and_a_changed_manifest_invalidates_it`: a
    white-box test that plants a `CacheEntry` with a sentinel `TreeInput` whose witness
    matches a real, tiny, on-disk Cargo project, proving an untouched read is served from the
    cache (the sentinel comes back unchanged — so the test also cannot pass on "never cache"),
    then rewrites the project's `Cargo.toml`/`Cargo.lock` and proves the next read recomputes
    (the sentinel is gone). Kills the mutant (quoted panic: both sides read
    `"sentinel-root"`), green twice. (`std::env::set_var` was not available for either fix's
    probe — both crates `#![deny(unsafe_code)]` — hence the type-aware and white-box designs
    instead of an environment-forced Cargo failure.)
- **The ⌥ "exact version with build metadata" spelling is designed but not wired at the
  present-words layer.** `display_version()` (`crates/present/browse.rs:213`) always strips
  build metadata (`1.1.5+spec-1.1.0` → `1.1.5`); the design calls for the exact spelling under
  ⌥ (`CHECKPOINT.md` §"Real vs unknown"), but no ⌥-aware variant exists yet in `TreeReading`
  or `Library`. Minor, but real — flag before assuming ⌥ already does something on the tree
  page.
- **The one unrelated test failure this finisher's due-diligence full-suite run turned up**:
  `view::root::tests::label_lookup_skips_cloned_row_bodies`
  (`crates/library/view/root.rs:1118`), a cold-vs-warm index lookup timing assertion,
  unrelated to browse (different module, file unmodified this session) and plausibly
  sensitive to the heavy concurrent build load this session had (§9). Not investigated or
  fixed — outside this lane's scope — but recorded so it isn't silently lost.
- **Concurrent-lane volatility during this session** (§9): `apps/facet/src/overlay/
  float/model.rs` and `apps/facet/src/controls/comb.rs` were both mid-edit and briefly
  uncompilable for roughly 40 minutes, blocking every GUI-level test and capture until other
  lanes finished their own in-flight changes. Nothing to act on — just context for why some
  verification in `browse-impl-checkpoint-1.md` is timestamped mid-session rather than at
  the very start.

---

## 8. Seams with other lanes

- **W-Marks** (`apps/facet/src/marks/{glyph,semver,spdx}.rs` + `fixture.json`, checked this
  pass: **real, in-progress, uncommitted** — not yet wired into `apps/facet/src/lib.rs`, no
  `mod marks` line exists yet). Do not build against these files yet; `Library` (§2.1)
  correctly still falls back to `paint::gem` and plain mono text and needs no change until
  marks land. When they do, `Library` is the component to update: `ecosystem_mark` (with the
  `quiet` variant Browse requested — no hover card for rows whose ecosystem is implied, from
  `marks/RULINGS.md`), `license_mark`, `version` (Baseline in rows at rest, a compact "band"
  variant below 240 px per Browse's own request, also in `marks/RULINGS.md`), and `dep_link`
  (the "toml → serde: … · 38 uses" hover from `DIRECTIONS.md` §1, needed from S4 on for the
  compare/judge views' capability rows).
- **The hand's Row rung** (`hand/CONCEPT.md` §4, ruling accepted 2026-09-27): "Packages held
  on Find are alternatives, so the Row rung's one action is 'compare three ›' (Browse)." This
  is the "⌘-click or C gathers candidates at the window foot" behaviour shown in
  `find-hand-1440.png` (§3.2) — it is the hand lane's component to build, Browse only needs
  to open its Compare route when the hand's "compare three ›" action fires. Not yet wired
  (S2/S4 territory).
- **The jump bar** (`hand/CONCEPT.md` §3.9, §4): Find is meant to be its own reader route,
  `nudox://find?q=`, reachable from the jump bar / ⌘K Ask's last row ("all answers as a page
  ↵", ruling 4 in §4 above). Not yet built.
- **The graph.** Compare/Find's "N lit in the graph" is a link-only door into
  `Graph.html?find=` (or its product equivalent) — the graph lane owns rendering; Browse only
  owns the link and (later) the count, which needs the graph lane's own query (§5,
  "deferred"). Per the rules given to this finisher, `apps/facet/src/graph/**` is never to be
  touched by this lane.
- **The motion grammar** (`Nudox-Design-System/v4/motion/MOTION.md`, "Neighbouring lanes"
  section, "D-Browse" subsection, verbatim verdicts): the ask-field-flies-into-hero motion is
  kept as *Become*, but must scale text uniformly (`Fit::Height`) rather than stretching it
  (sx≠sy is a named defect in the prototype). FLIP re-rank is kept as *Travel*, but must be
  spring-driven (SNAPPY) so keystrokes retarget, must drop the stagger while typing, must
  narrow leavers before opening arrivals, and must add the *wake* (a periwinkle trailing
  line showing how far a row moved). Adopt is kept as *Throw* (play), but the flyer must
  *Turn* by a fixed quarter rather than rotating freely (which breaks the stone's fixed
  light), must cap mid-flight scale at 1.2 (the prototype overshoots to 1.62), and must aim
  the arc as a throw into the row rather than a lob over the page. None of this motion work
  has started yet (S2+ territory); the primitives it needs (`Roll`/`Odometer`, `clip act`,
  `unfurl`, `wake`, `arc flight`, `Gem::turn`) are being built by W-Flow/W-Float right now —
  confirmed still in flux this session (§9's `overlay/float/model.rs` breakage was exactly
  this work in progress).

---

### 8.x Package lanes are unreachable today (lead, from W-Shell's dead-end survey, 2026-09-27)

`PackageLane` (Dependencies, Dependents, Releases, Security) exists in the route
(`apps/desktop/src/navigation/route.rs:122`). `bodies/package.rs` never reads `route.lane`,
so none of these lanes can be reached. The browse slices that judge a package (why, compare,
releases, advisories) should either become these lanes or delete them. Don't leave both.
W-Marks' `dep_link` navigates to a dependency's own package page. A "dependents" lane is the
natural home for Browse's "who uses it" data.

## 9. Working rules and commands

- **Git**: read-only except `git add <new file you created>`. Never stash, checkout, restore,
  reset, clean, commit, rebase, filter-repo, switch or merge — no exceptions, including inside
  scripts. The owner commits between sessions.
- **Build**: `.local/devenv/cargo <cmd> -p <crate>` only. Never `--workspace`, never `cargo
  clean`. At most two of your own cargo processes at once (the build lock is shared with every
  other running lane).
- **Mutations**: apply, test and revert inside **one** Bash command with a `cp`-based trap
  (never `git checkout`) — see `browse-impl-checkpoint-1.md` §5 for four worked examples,
  including one where the first approach had to be abandoned mid-mutation because the target
  crate denies `unsafe_code`, and the redirect to a better test target that still worked.
- **Tests**: trust a result only when two consecutive runs agree (test order/timing
  differences don't count against this). Assert on rendered content, never counts alone.
- **Capturing the tree scene** (needs `backend-facet --features gallery` to build clean —
  it took six attempts across ~40 minutes to get a clean build in this session, §7, purely
  from other lanes' concurrent in-progress edits, then worked on the first try after):
  ```
  .local/devenv/cargo run -p backend-desktop --features visual-harness \
    --bin backend-desktop-gui-harness -- capture --scene desktop-tree \
    --size 1440x900 --theme abyss --out <DIR>
  ```
  Repeat with `--theme glacier` and with `--size 760x900` (both themes) for the four views
  this brief's own instructions asked for — **use a different `--out` directory per
  combination**: the capture's filename encodes theme/scale/time but not `--size`, so two
  sizes at the same theme overwrite each other otherwise (lost the first 1440×900 Abyss
  capture this way, see `browse-impl-checkpoint-1.md` §3). `backend-desktop-gui-harness list`
  names every scene; `journey NAME` runs an end-to-end journey once one is defined for browse
  (§5).
- **Running the real end-to-end test**: `cargo test -p backend-desktop browse` (no feature
  flag needed — `shell::browse_tests` isn't gated on `visual-harness`, only the capture
  *binaries* are). Takes a few seconds; spins up a real embedded `backend-locald` per run.
- **The four-agent cap**: the owner caps total concurrent agents at ~4; keep your own cargo
  usage to at most two processes so other lanes aren't starved.

---

## 10. Appendix index

Copied source documents, under `docs/architecture/briefs/package-browsing/`:
- `browse-concept.md` — the four-registry comparison and the three design concepts
  (verbatim copy of the lead's `CONCEPT.md`).
- `browse-checkpoint.md` — the design lane's own checkpoint: the prototype's URL contract,
  every still, "real vs unknown" data provenance, and what the implementation lane needs
  (verbatim copy of `CHECKPOINT.md`).
- `browse-rulings.md` — the lead's binding rulings on that checkpoint (verbatim copy of
  `RULINGS.md`; quoted in full in §4 above).
- `browse-impl-plan.md` — the implementation plan: every data contract, the wire additions,
  the GUI component map, all five slices, the test plan, and decisions D1–D9 (verbatim copy
  of `PLAN.md`; note its own filenames like `meta-host.json`, `fixture-meta.json`, `stills/`
  and `cli-data` are scratch evidence from the planning pass, not preserved in this repo).
- `browse-impl-checkpoint-1.md` — this finisher's own checkpoint: the harness bug and fix in
  full, every test run quoted, and all five mutations (two biting, two revealing real gaps)
  quoted in full (verbatim copy of `$S/wave4/browse-impl/CHECKPOINT-1.md`).

Other documents cited but not copied (already in the repository):
- `Nudox-Design-System/v4/DIRECTIONS.md` §6 — the package-browsing direction and real data
  sources, quoted in §6 above.
- `Nudox-Design-System/v4/motion/MOTION.md` — the motion grammar and the D-Browse verdicts,
  quoted in §8 above.
- `docs/architecture/gui-plan.md` §6.2 — the restraint rules every board must obey, quoted in
  full in §4's ruling 3 context and honoured by the shipped `Library` component (§2.1).
- `docs/architecture/gui-plan.md` §3 item 10 — the journeys table and the `BLOCKED (data)`
  convention, referenced in §5's journey note.
- `docs/architecture/gui-plan.md` §8.5 — "Data the index must provide", relevant to S2's API
  extractor and S3's release data; the same limitation (method calls on values need type
  inference, not yet covered) applies to Browse's tier-A uses (D2, D8).
- `wave4/hand/CONCEPT.md` §4 — the hand's Row rung ruling, quoted in §8.
- `wave4/marks/RULINGS.md` — the two requests Browse made of D-Marks (compact comb, quiet
  ecosystem mark), quoted in §8.
