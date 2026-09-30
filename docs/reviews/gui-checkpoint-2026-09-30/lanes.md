# inv-lane-leftovers: GUI open items harvested from wave2-6 lane files (non-final)

## READ ME
- 329 records (after merging 18 duplicates; each merged record says MERGED and lists the extra sources). Sections 1-20 follow the order of the reading list. The final lanes (F-Data, F-Shell) are in scratchpad/raw/M-final.md and are only cross-referenced here.
- Id schemes: G1-G15, L1-L2, D1-D6 (journey/GAPS.md); FEEL-D1..D17 (review/feel); RFit-1..19, RFit-C1..C11 (review/fit); RFolio-D1..D15, -N, -C, -A (review/folio + folio/FOLIO-A/B); RSym-D1..D10, -N, -C (review/sym6 + sym6/SYM6-A/B); ROpen-D1..D8, -N, -C, -T (review/open3); SIDE-n; FLUID-n; FIT-n; GLYPH-n; ACQ-n; INS-n; IDX-n; INST-n (instant lane); TIS-n (tissue); TRN-n (transitions); PG/WLD/PLN (page, world, wave PLAN); RES-n (research); OLD-n (waves 2-5); PH-n (phases that never ran).
- Status was checked against the working tree on 2026-09-30 by read-only grep where the text says "grep" or names a line; every other status is what the newest lane file says. "unverified" means no file shows it working or failing after the last change.
- Whole phases that never ran (no checkpoint file): SYM6-C, FOLIO-C, INSTALL-B/C, JOURNEY-B/C, OWNER-A/B/C, ACQUIRE-B/C, FEEL-B, transitions T3 FLIP / T4 purge / T5 Push-Peel-Reel-Odometer, W-Surfaces, W-World, W-Green ROUTED.md, page CP3 (S8-S9), R-Side / R-Journey / R-Install / R-Acquire / R-Feel reviewers, and phase 2 of R-Sym6 / R-Folio (R-Fit and R-Open3 phase 2 landed code but wrote no report). See PH-1..PH-14.
- Items with real-data "never looked at" or fixture/gallery/rig-only evidence dominate: 205 of 329 records; only 38 were seen on the real owner or real packages.

Base: /Users/mileswirht/Downloads/backend/.local/lanes (W6 = wave6). Records are appended file group by file group; a consolidation pass at the end merges duplicates and updates statuses. "M-final Mn" refers to records in scratchpad/raw/M-final.md; "PR" = docs/architecture/briefs/production-readiness.md.

## SECTION 1: journey/GAPS.md and JOURNEY-A.md

### G1 · add-project dialog has no keyboard path or render
surface: onboarding/install
problem: At 06:30 nothing bound a key for Add project and Overlay::AddProject was drawn by nobody; W-Install added Cmd-O and shell/onboard/add.rs by 07:15 and the status only becomes closed when a J0 run passes the step.
evidence: W6/journey/GAPS.md:13-21 (shell/keys.rs:195-237, navigation/route.rs:217-219, shell/bodies/mod.rs:249, model/workspace.rs:137)
status: FIXED in code by W-Install (GAPS.md:14); J0 walked add-folder and D4 (see below) fixed on the real journey (M-final M42); no post-fix J0 verdict recorded (M-final M5)
real data: verified on the real owner in the 19:53 partial J0; post-fix verdict never recorded
sources: W6/journey/GAPS.md:13-21; W6/install/INSTALL-A.md; final/COORD.md:64-69

### G2 · "Add a folder" empty-state button outside the keyboard walk
surface: keys/focus
problem: The empty Library's Add-a-folder button was not pushed into ctx.targets so J/K, F hints and Enter never reached it.
evidence: W6/journey/GAPS.md:23-28 (shell/bodies/orbit.rs:55-57, orbit.rs:97-103)
status: FIXED in W-Install's tree (GAPS.md:24, onboard/library.rs:142); J11 (which would prove it) written but never reported run (M-final M2)
real data: never looked at in a keyboard run on the real empty state
sources: W6/journey/GAPS.md:23-28

### G3 · indexing progress has no owner stages
surface: onboarding/install
problem: The owner emits no discovery/resolution/per-package events; the window can only say admitted, indexing, ready, so a person waiting 10+ minutes sees no movement inside a package compile. MERGED: also INSTALL-A: the owner reports nothing while a project indexes; window shows only admitted/indexing/ready with a hatch that does not advance in virtual time; a real 10-17 minute install shows a static hatch and 'N min ago' (install/INSTALL-A.md:17-28,52,91)
evidence: W6/journey/GAPS.md:30-38 (orbit.rs:104-109, client.rs:184-197, coordinator.rs:286-332)
status: deferred (owner internals); per-package progress is window-side only, nothing inside one compile (M-final M34; final/F-Data-2.md:14,103)
real data: REAL install measured (39 s compile, no movement)
sources: W6/journey/GAPS.md:30-38; final/F-Data-2.md:14,103

### G4 · a finished index is not persisted, restart re-indexes
surface: foot/status/lifecycle
problem: Engine results never emitted Effect::Persist so Ready/Failed was written only by a later intent, and a restored Indexing row was re-indexed automatically.
evidence: W6/journey/GAPS.md:40-48 (coordinator.rs:286-332, ui_graph.rs:392-394, persistence.rs:966-975)
status: FIXED by W-Install (INSTALL-A.md:43 "persistence of a finished/failed index the moment it arrives"); relaunch of a real install reads back the finished Library (final/F-Data-2.md:16-22, m2c capture). Restart persistence is "for most fields" (PR:39)
real data: REAL for the toml_pin install relaunch; other states (Failed, Cancelling) relaunch not seen
sources: W6/journey/GAPS.md:40-48; W6/install/INSTALL-A.md:43; final/F-Data-2.md:16-22

### G5 · window size not persisted
surface: foot/status/lifecycle
problem: The window always opened at 1380x880; a size field was needed; done by W-Install but only unit-tested until F-Data-2 wrote the state file after a real resize.
evidence: W6/journey/GAPS.md:50-55 (host/launch.rs:37)
status: partially verified: desktop-state.json holds window{1100,760} and a unit test reads it back, but the reopened size was never seen in a picture (final/F-Data-2.md:22; M-final M36)
real data: state file REAL; reopened window UNVERIFIED visually (a harness relaunch captures at the scene size)
sources: W6/journey/GAPS.md:50-55; final/F-Data-2.md:22

### G6 · a failed index shows no reason and no Try again / Reveal / Remove
surface: onboarding/install
problem: A failed project read only "stopped" and nothing dispatched RetryIndex/CancelIndex/RemoveProject/RevealProject; W-Install added a failure card with named cause and three buttons.
evidence: W6/journey/GAPS.md:57-65 (orbit.rs:106, workspace_reducer.rs:222-292)
status: FIXED in code (INSTALL-A.md:43; PR:38 "a failure card with Try again, Reveal and Remove"); J13 (failure and retry on the real owner, needs a real failing fixture) never written or run (M-final M4; PR:291)
real data: fixtures only; no real failing project run through the card
sources: W6/journey/GAPS.md:57-65; W6/install/INSTALL-A.md:43; PR:38,291

### G7 · owner failure has no Try again outside page bodies (R7)
surface: foot/status/lifecycle
problem: The Try again button existed only inside page bodies; Orbit and the status foot showed the owner fault as quiet text.
evidence: W6/journey/GAPS.md:67-72 (shell/bodies/state.rs:100-104, orbit.rs:147-150, status.rs:202-283)
status: FIXED by F-Data-2 (status foot shows "The index could not start... Try again" on every route, runtime/store.rs::owner_failed) but proven by lifecycle_tests with an injected failure only (M-final M38)
real data: fixtures only; never seen with a real owner start failure
sources: W6/journey/GAPS.md:67-72; final/COORD.md:76; final/F-Data-2.md:21

### G8 · switching or removing a project re-indexes it
surface: onboarding/install
problem: ActivateProject set the phase back to Indexing and scheduled a full recompile blocking the owner's single loop.
evidence: W6/journey/GAPS.md:74-79 (workspace_reducer.rs:212, ui_graph.rs:268-271)
status: FIXED by W-Install (INSTALL-A.md:15,43 "switching and removing no longer re-index"); J0b (ReadyMultiProject: click second tile, check switched within the page-open budget) never reported run; the Library has only ever been driven with ONE project (toml_pin)
real data: never looked at with two real ready projects
sources: W6/journey/GAPS.md:74-79; W6/journey/JOURNEY-A.md:93; W6/install/INSTALL-A.md:15,43

### G9 · a shell launch inside a project folder auto-admits it and skips the first run
surface: onboarding/install
problem: A launch whose working directory looks like a project skips the empty state; whether a shell launch should also show the first run is undecided.
evidence: W6/journey/GAPS.md:81-85 (host/launch.rs:227-229, persistence.rs:1045-1097, paths.rs:74-86)
status: owner decision (PR:207,363)
real data: N/A
sources: W6/journey/GAPS.md:81-85; PR:207,363

### G15 · installing a project indexes none of its dependencies
surface: onboarding/install
problem: A clean real index of toml_pin ended Ready with one package (25 declarations); the harness fixture index hid this by admitting each registry source as its own root.
evidence: W6/journey/GAPS.md:89-94 (runs/J0-v2/06-library.png: "1 project · 0 packages")
status: FIXED by F-Data-1 (17:28 COORD): toml_pin now yields 12 packages from the offline cargo cache; two remain thin (serde_core, hashbrown) and five cfg(any()) pins show only in the "17 packages beneath" count (M-final M16, M25); the check "library lists toml, serde, toml_edit, winnow, non-zero N declarations line" was never quoted as PASS in a final file
real data: REAL (clean install, ~10-17 minutes wall)
sources: W6/journey/GAPS.md:89-94; final/COORD.md:10-11,109; final/F-Data-1.md

### G10 · no "Add to library" anywhere a person browses
surface: Find/browse
problem: Find rows, the package header (cargo add NAME is plain text) and dependency marks have no Add action; only the empty shelf's Add a folder existed.
evidence: W6/journey/GAPS.md:96-105 (browse/find.rs:418, find.rs:473-498, marks/eco.rs:116, package.rs:262-296)
status: code landed (shell::acquire::add_actions, Intent::AddRelease, bodies/browse.rs:76, bodies/package.rs:135; final/COORD.md:13) but never seen drawn or working on the real library in any final PNG (M-final M10); J7 add from browsing not written
real data: never looked at on the real library
sources: W6/journey/GAPS.md:96-105; final/COORD.md:13

### G11 · no single typed source (registry name+version to a source tree)
surface: onboarding/install
problem: Three string-typed or test-only source resolvers exist (harness registry_source, source_facts registry.rs:95, symbol/place.rs:19) and the owner's purl path fetches over the network, which agents may not run.
evidence: W6/journey/GAPS.md:107-111 (harness.rs:200, registry.rs:95, place.rs:19, builtin/registry.rs:802)
status: partly FIXED: one host::registry::CargoCache offline adapter added by F-Data-1 (COORD 17:28); network policy is an owner decision (PR:119,361; M-final M64); the duplicate resolvers' removal not confirmed
real data: REAL offline for 12 packages; network path never run
sources: W6/journey/GAPS.md:107-111; final/COORD.md:10-12; PR:111-119

### G12 · Find cannot show a package that is only in the local cargo cache
surface: Find/browse
problem: compose_find merges only index and owner catalogs so a crate present only in ~/.cargo/registry cannot be found or added from Find (zero results read "No recorded match yet").
evidence: W6/journey/GAPS.md:113-117 (runtime/reads.rs:1126-1168, browse_reads.rs:53-78, find.rs:362-363)
status: still open per PR:126; no final file changes it (M-final M11)
real data: never looked at
sources: W6/journey/GAPS.md:113-117; PR:126

### G13 · an earlier release has no API unless it is itself indexed
surface: releases/time travel/upgrade
problem: The owner reads only indexed releases; a local-root pin falls back to the pin's dossier so the page shows 0.8.23's names under "Reading 0.5.11" with the line "Its names are not read yet", and a symbol at that release is Unread::ReleaseNotHere.
evidence: W6/journey/GAPS.md:119-127 (product_state.rs:299,436; reads.rs:1036-1095; store.rs:247-249,282; folio.rs:256; bodies/mod.rs:129-131)
status: still open: runtime/releases.rs is untracked in the working tree and unverified (M-final M7, M8); journey verdict BLOCKED(data)
real data: never looked at; fixtures only (toml 0.5.11 not indexed)
sources: W6/journey/GAPS.md:119-127; final/COORD.md:14; PR:129-133

### G14 · change states and upgrade counts come from a fixture
surface: releases/time travel/upgrade
problem: Release diffs, banner counts, symbol history, the upgrade lens and the sidebar row glyphs all read runtime/fixture_releases.rs (facet's embedded fixture.json: toml and smallvec only), so no journey can pass on those numbers.
evidence: W6/journey/GAPS.md:129-133 (fixture_releases.rs:42; package/data.rs:430-474; symbol.rs:112,179-181; symbol/history.rs:18; side/state.rs:169)
status: still open at HEAD (M-final M9); untracked releases.rs carries empty before/after, no semver-slip, empty impact (M-final M8)
real data: FIXTURE (prototype toml and smallvec only)
sources: W6/journey/GAPS.md:129-133; PR:90

### L1 · MCP setup has no surface
surface: settings
problem: Settings has no Agents/MCP controls (Agents maps to Appearance) and Intent::TestConnection is dispatched by nothing on screen; the retired McpSetup journey (open guidance, copy the command, test connection) has no runnable form.
evidence: W6/journey/GAPS.md:140-143 (shell/bodies/settings.rs:25-32, ui_graph.rs:228-241)
status: still open (PR:154)
real data: never looked at
sources: W6/journey/GAPS.md:140-143; W6/journey/JOURNEY-A.md:96; PR:154

### L2 · documented shortcuts that nothing binds
surface: keys/focus
problem: navigation/action.rs lists cmd-shift-p, cmd-b, cmd-n, cmd-shift-a, cmd-shift-y, cmd-shift-/, cmd-shift-m, cmd-left/right, cmd-0 (Home) as metadata only, and cmd-0 collides with ZoomReset.
evidence: W6/journey/GAPS.md:145-148 (navigation/action.rs:204-225)
status: still open (FINISH F-Shell item 8 asked for it; no final file marks it done, M-final M40, M41)
real data: N/A
sources: W6/journey/GAPS.md:145-148; PR:176

### D1(GAPS) · "Type to narrow" fails contrast on every Library frame
surface: contrast/a11y
problem: The sidebar hint was ink4 at 2.59:1, so no journey checkpoint with the shelf on screen could pass its zero-lint rule.
evidence: W6/journey/GAPS.md:152-156 (runs/probe-dialog/REPORT; side/view.rs:205)
status: FIXED by F-Shell-1 (all sidebar words to ink3, guard test fit_tests::no_words_in_the_shell_are_set_in_ink4; J10 Library frames pass zero-lint both themes) (final/F-Shell-1.md:30; M-final M43)
real data: REAL (J10 on the real owner)
sources: W6/journey/GAPS.md:152-156; final/F-Shell-1.md:30

### D2(GAPS) · owner watch task leaks the window's root and store
surface: foot/status/lifecycle
problem: runtime/owner.rs::watch held strong Entity handles forever so gpui's leak detector panicked when an App was dropped; the harness works around it by leaving a shut-down app to the process's end.
evidence: W6/journey/GAPS.md:158-162 (runs 07:13/07:19; harness/journey/machine.rs quit)
status: FIXED by F-Data-2 (weak handles, test host/lifecycle_tests.rs:352 mutation M14) (final/F-Data-2.md:23); the harness workaround in machine.rs quit not confirmed removed
real data: fixtures only (test)
sources: W6/journey/GAPS.md:158-162; final/F-Data-2.md:23; PR:208

### D3(GAPS) · button labels are not probe texts, so lints never see them
surface: harness/journeys/lints
problem: Contrast, clip and overlap lints never check the words of Add a folder, Choose folder, Cancel, Add, or any button because facet/controls/button.rs paints the label without a probe text record; journeys read painted text from gpui's trace instead.
evidence: W6/journey/GAPS.md:164-168 (apps/facet/src/controls/button.rs; harness/journey/look.rs painted_extras)
status: still open (no final file marks it done; M-final M40; git status shows look.rs modified uncommitted)
real data: N/A
sources: W6/journey/GAPS.md:164-168; PR:193

### D4(GAPS) · add-folder dialog not on the float layer, scrim lints as failed contrast
surface: overlays/popovers
problem: 13 false contrast lints per checkpoint because the dialog published no probe::StackEntry{kind:"dialog"}. MERGED: also INSTALL-A: lint measured page text veiled behind the modal scrim; disclosure hit target 124x16 raised to 24 (install/INSTALL-A.md:58,93)
evidence: W6/journey/GAPS.md:170-174 (runs/J0-v2/REPORT.txt add-dialog, path-typed-toml_pin)
status: FIXED by F-Data-2 (dialog through facet::overlay::dialog, publishes probe::veil; J0-finder-190840 "lints: none (5 texts, 4 targets linted)") (final/F-Data-2.md:24; M-final M42)
real data: REAL
sources: W6/journey/GAPS.md:170-174; final/F-Data-2.md:24,52

### D5(GAPS) · Esc does not close Settings
surface: keys/focus
problem: The route stayed Settings after Esc, so relaunch reopened Settings.
evidence: W6/journey/GAPS.md:176-180 (runs/J0-v2/08-library-again.png, desktop-state.json route Settings)
status: FIXED by F-Shell-1 (rig test + J10 PASS on the real owner, run J10-174347) (final/F-Shell-1.md:39-43; M-final M44)
real data: REAL
sources: W6/journey/GAPS.md:176-180; final/F-Shell-1.md:39-43

### D6(GAPS) · add-folder field's focus ring jumps 1.000 to 0.000 in 64 ms
surface: keys/focus
problem: The ring jumped when the dialog closed on Esc (FAIL continuity 848 ms add-folder-path-focus).
evidence: W6/journey/GAPS.md:182-184 (runs/probe-dialog2/REPORT.txt)
status: dialog case FIXED by F-Data-2 (dialog closes onto no focused control; shell/onboard/tests.rs:167); the general continuity-check decision (does a modality switch retarget) is open and F-Shell's (M-final M20)
real data: REAL for the dialog
sources: W6/journey/GAPS.md:182-184; final/COORD.md:68,74; final/F-Data-2.md:25

### JRN-1 · native folder picker never exercised by a journey
surface: onboarding/install
problem: Journeys answer the native folder panel with answer-picker PATH (through UiRootEntity::answer_folder_picker) and the picker's real NSOpenPanel behaviour, path return and cancel were never run end to end.
evidence: W6/journey/JOURNEY-A.md:19
status: still open (J0c PickerCancelled is described but no run is quoted)
real data: never looked at
sources: W6/journey/JOURNEY-A.md:19,95

### JRN-2 · journeys inject route and settings as intents, not as a person's input
surface: harness/journeys/lints
problem: route, theme, density, contrast, motion, text-scale acts inject product intents and are refused outside a detour, so settings journeys through the real Settings controls only exist in J10; no other journey drives the controls.
evidence: W6/journey/JOURNEY-A.md:17-19
status: by design; J10 (settings persist) PASS on the real owner (final/F-Shell-1.md:41); other settings (density, contrast, text scale) through the UI never journeyed
real data: partly REAL
sources: W6/journey/JOURNEY-A.md:17-19

### JRN-3 · `start ROUTE` fixture machine can never stand for an install
surface: harness/journeys/lints
problem: Scenes still run on a fixture index with roots pre-admitted; every scene, lint and film outside the journeys is judged on a state a person cannot reach (this hid G15 for a wave).
evidence: W6/journey/JOURNEY-A.md:13
status: known; PR:42 states every test until the first clean-install journey ran in the dev shell with fixture roots
real data: FIXTURE by design
sources: W6/journey/JOURNEY-A.md:13; PR:42

### JRN-4 · journey state cache and settle==fresh depend on one live root; parallel harness refused
surface: harness/journeys/lints
problem: The live root is flock-ed for the whole command so only one journey process runs at a time; the state cache key includes the build id so every rebuild re-runs a 10-17 minute install.
evidence: W6/journey/JOURNEY-A.md:37-52
status: by design; cost not measured
real data: N/A
sources: W6/journey/JOURNEY-A.md:37-52

## SECTION 2: review/feel (FEEL.md, FEEL-A.md, KIT.md, kit/README.md, NOTES.txt). Paths relative to W6/review/feel (F).

### FEEL-D1 · popover cards opened by a Hoverable never closed
surface: overlays/popovers
problem: float::trigger keyed the trigger by its own id while the layer matches leave by the card key, so every symbol-page door peek stayed open after the pointer left. MERGED: same root cause seen by R-Sym6 as 'the generic card never closes in the gallery harness' (review/sym6/REVIEW.md:29,68; notes.md:51); the desktop-harness re-check of the real symbol page never done
evidence: F/FEEL.md:70; proof/hover-grammar-before-2255/popover.png vs proof/hover-grammar-after-0500/popover.png
status: FIXED by W-Feel F2 (overlay/float.rs trigger(), test window_tests a_card_opened_by_a_word_with_another_key_closes...; mutation window_tests.rs:358:5); re-verify on the real symbol page not done (fixture index was empty) (F/FEEL.md:123)
real data: gallery + rig only; desktop symbol peek re-check on the real owner never done
sources: F/FEEL.md:37-38,70,94-96,123; F/NOTES.txt (F8)

### FEEL-D2 · Ask (Cmd-K) could not be dismissed or typed into
surface: Ask/search
problem: The query field was drawn nowhere, focus sat on an undrawn handle, Esc did nothing.
evidence: F/FEEL.md:55,71; proof/keys-esc-orbit/ask-escape.png; net/keys-esc-after/keys.txt
status: FIXED by W-Fluid's titlebar (rig test escape_closes_the_topmost_transient_first passes; harness cmd-k>escape works) (F/FEEL.md:71,108)
real data: fixture index harness; Ask on the real library verified by J9 work in final (M-final M23) only for "toml Value"
sources: F/FEEL.md:55,71,108; F/NOTES.txt (F1)

### FEEL-D3 · hovered word stayed lit after pointer left window / reflow / navigation
surface: overlays/popovers
problem: facet::hover cleared only on mouse move.
evidence: F/FEEL.md:54,72; proof/hover-grammar-leave-end.png
status: FIXED by F1 (hover.rs Source enum, MouseExit handler, paint reconcile, hover::clear on close_all)
real data: gallery only
sources: F/FEEL.md:54,72,90-93

### FEEL-D4 · Rig::settle and the first-card whisper looped forever (timer to notify to re-arm)
surface: performance
problem: 100% CPU in tests and a spinning status bar in the product; whisper state machine and bounded settle rewrote it.
evidence: F/FEEL.md:73,101-103
status: FIXED by F5; confirmed by F-Shell-1 (whisper mutation proof hand_tests.rs:176:5, 522 rig tests none at 100% CPU) (final/F-Shell-1.md section 2). Related structural risk recorded in memory "unbounded settle hides render loops"
real data: rig only
sources: F/FEEL.md:73,101-103; final/F-Shell-1.md:section 2

### FEEL-D5 · reader plate morph misbehaves on a shelf-row click (33 px jump, 25 px overshoot, carry out of lockstep)
surface: motion/transitions
problem: On click of a shelf row the plate edges jump and overshoot and reader.carry is out of lockstep with the plate; 3 of 3 storm seeds fail. MERGED: also R-Fit: Library verify storm finding, 1 px jump of reader.plate.left (continuity 685 ms, 187->186 in 8 ms) after a 27 px drag (review/fit/REVIEW.md:45,150; BASELINE.md:118)
evidence: F/FEEL.md:22,74 (net/storm-desktop-orbit; repro click left 92,277 @711); apps/desktop/src/shell/reader.rs:1588-1620,1811
status: probably FIXED by F-Shell-1 (reader.rs Course: each edge now publishes its own target and speed; "J10's motion is clean", final/F-Shell-1.md:42) but the storm on desktop-orbit was never re-run in a final file; M-final M45 says other journeys' motion unreported
real data: REAL for J10 only; storm never re-run
sources: F/FEEL.md:22,74,118; final/F-Shell-1.md:42; PR:185

### FEEL-D6 · Find candidate rows squeeze the name column to 19-32 px, no ellipsis
surface: Find/browse
problem: At narrow widths and 200% text the package name clips without an ellipsis (serde_json-1.0.151 needs 135-270 px in a 19-32 px box). MERGED: also R-Fit defect 10: candidate name boxes 20 px at 390 and 0 px at 360/320 at 200% text; overlap Self over from_str at 2560; contrast at 1024 (review/fit/REVIEW.md:44; BASELINE.md:37,128)
evidence: F/FEEL.md:19,27,75 (verify-desktop-find REPORT; matrix-desktop-find sheets; repro desktop-find --input 'resize 480x320 @367')
status: still open (PR:197; owner apps/facet/src/browse/find.rs; no lane reports a fix)
real data: fixture packages (serde_json, toml); real library not swept
sources: F/FEEL.md:27,75,119; PR:197

### FEEL-D7 · Find at 200% text: settled frame differs from a reduced-motion boot
surface: Find/browse
problem: glacier, 1440 and 1024 wide, 200% text: the settled frame differs from reduced-motion boot, i.e. an animation that never lands where the reduced layout lands.
evidence: F/FEEL.md:27,76 (matrix --scene desktop-find --widths 1440,1024 --themes glacier --text-scales 200)
status: still open (PR:198)
real data: fixture
sources: F/FEEL.md:27,76; PR:198

### FEEL-D8 · graph rail family heads unreadable at normal column opacity in glacier (3.02-3.07:1)
surface: graph/world
problem: MadeBy, Is, Calls heads fail contrast at NORMAL_COLUMN_ALPHA 0.75.
evidence: F/FEEL.md:32,77 (graph/prism/rail.rs)
status: FIXED by F7 (rail_head_tone requires 4.5:1 at full and at 0.75; mutation rail.rs:371:17); passes in final green (final/F-Shell-1.md:section 1)
real data: fixture world
sources: F/FEEL.md:32,77,106

### FEEL-D9 · Library has no hover popover on package words or shelf rows; titlebar icon buttons had no tooltip
surface: overlays/popovers
problem: Nothing on the Library shows a card on hover; icon-button tooltips documented but never drawn.
evidence: F/FEEL.md:42,78; net/pop-orbit-word, net/pop-orbit-shelfrow, net/pop-titlebar-shelf (NO CARD)
status: icon buttons FIXED by F6 (controls/icon_button.rs label as tip; test icon_button.rs:286:9); Library words/rows are an owner design call (keyboard peek Space exists, orbit.rs:233); PR:189,364
real data: fixture Library
sources: F/FEEL.md:42,78,104-105; PR:189

### FEEL-D10 · Cmd-Shift-C (copy the address) gives no feedback
surface: keys/focus
problem: The key works but nothing tells a person it copied; needs a toast.
evidence: F/FEEL.md:56,79 (net/keys-orbit/keys.txt "dead"); apps/desktop/src/shell/root.rs copy_address
status: still open (PR:180)
real data: never looked at
sources: F/FEEL.md:56,79,121; PR:180

### FEEL-D11 · graph footer at 320x480: caption vs where-line
surface: graph/world
problem: The collision was resolved (caption ellipsizes) but the two footer lines sit on different baselines about 10 px apart (label bottom(21) in graph/view.rs, caption bottom(8)); not changed. MERGED: also W-Flip R-T2-1: the graph status line collides with the where-line at rest; .right(px(16.0)) makes all 14 rig transitions pass; the rig ledger was never re-run (transitions/CP2.md:12,88)
evidence: F/FEEL.md:80,111 (net/world-320/desktop-world-abyss-100pct-t1500@2x.png; shell/bodies/graph.rs:1247)
status: collision RESOLVED; baseline misalignment cosmetic leftover, still open
real data: fixture world
sources: F/FEEL.md:66,80,111; F/NOTES.txt (F5)

### FEEL-D12 · hit targets under 24 px: Library chips at 85% text (23 px) and Find inspect controls (20 px)
surface: contrast/a11y
problem: orbit-package-* chips are 23 px tall at 85% text; find-candidate-N-hold 76x20, open-answer 43x20, more-answers, find-inspect-open 124x20, find-inspect-source-toggle, find-inspect-open-package 74x20 all under 24.
evidence: F/FEEL.md:26,27,82 (matrix sheets; orbit.rs:257 .h(measure.row()) needs .min_h(px(24.0)); browse/find.rs)
status: still open (PR:200: chips at 85% and Find inspect controls; R-Folio D14 package badges 21 px is a related instance)
real data: fixture
sources: F/FEEL.md:26-27,82,120; PR:200

### FEEL-D13 · popover dismissal exceeds the 120 ms bar by construction
surface: overlays/popovers
problem: Tip dismissal was 216 ms and rest 450 ms; peek and menu grace plus roll-up (220 ms) stay over the 120 ms bar; floor for an enterable card is grace 100 + roll-up 120 without breaking the aim triangle. MERGED: also R-Sym6: float unfurl enter 220 ms / exit 300 ms (overlay/float.rs UNFURL_ENTER/EXIT) against the 150/120 ms budget (review/sym6/REVIEW.md:68,145)
evidence: F/FEEL.md:36-39,83 (proof/hover-grammar-after-0500/popover.png; net/pop-sym-jump/popover.png)
status: tips FIXED by F3 (rest 350, grace 30, unfurl_exit; test no_card_rests_over_400_ms_and_a_tip_is_gone_120_ms_after_the_leave); peeks/menus reported not changed: owner decision whether the bar or the construction changes (PR:183, 364)
real data: gallery + fixture symbol page
sources: F/FEEL.md:36-39,83,97-98,112; PR:183

### FEEL-D14 · verify world storm: graph-prism still live at 1856 ms, budget ended 1859 ms
surface: graph/world
problem: A 3 ms end-of-run boundary on the graph scene (repro move 430,78 @879; click left 52,75 @916); not reproduced as a leak (scene idle from 0 ms at 2560x900).
evidence: F/FEEL.md:19,84 (net/verify-desktop-world/REPORT.txt; net/world-2560.json)
status: unresolved (not reproduced)
real data: fixture world
sources: F/FEEL.md:19,46,84

### FEEL-D15 · hints codes(count) silently leaves targets past the 256th without a code
surface: keys/focus
problem: hints.rs:43 codes(count) makes targets beyond 256 unreachable in hint mode.
evidence: F/FEEL.md:85; apps/desktop/src/shell/hints.rs:43
status: still open, low (PR:178)
real data: never looked at with 256+ targets (the real Library has 665 sidebar rows)
sources: F/FEEL.md:85; PR:178

### FEEL-D16 · probe::rules test a_text_wholly_past_a_side_is_stranded... red
surface: harness/journeys/lints
problem: A rule test failed at the time (apps/facet/src/probe/rules.rs:146) which matters because the harness skips wholly-hidden text (R-Fit "The lead"); the fully-hidden rule is the same gap.
evidence: F/FEEL.md:86
status: passes by 18:40 (final/F-Shell-1.md:section 1 facet lib 762 passed); the lint gap "a strip laid out past the right edge lints clean" (PR:192) still open
real data: N/A
sources: F/FEEL.md:86; PR:192

### FEEL-D17 · opening Settings swaps the shelf in one frame while the reader still shows the Library ~100 ms, then near-empty
surface: motion/transitions
problem: Two contexts on screen for ~100 ms then a nearly empty reader from t=272 to 352 before rows stagger in.
evidence: F/FEEL.md:47,81 (net/film-settings/settings-frames.png; repro film.py desktop-orbit --every 8 --until 500 --input 'key cmd-, @200')
status: still open (PR:186); owners shelf.rs (lead's) and reader.rs
real data: fixture Library
sources: F/FEEL.md:47,81; PR:186

### FEEL-N1 · Find and Library narrow-width clip/cliffs: 15 Library text boxes snap 3x-14x, 17 shelf rows vanish at 880-840
surface: fluid/phone widths
problem: The Library's package words re-wrap (pflag x -570 px over 40 px at 1000->960, zod at 880->840); the shelf-to-spine change at 880->840 is a mode switch that animates (spring) but its rows vanish.
evidence: F/FEEL.md:64 (proof/cliffs-orbit/cliffs.txt, sheet.png)
status: still open when measured (harness-0056); FLUID-B/C may have changed it; FLUID-C's 285-width sweep on real content: see fluid records
real data: fixture Library
sources: F/FEEL.md:64,122

### FEEL-N2 · semantic colour audit and icon crops at 1x/2x never done
surface: contrast/a11y
problem: "One coral for failure, one amber for change" audit and icon crops (net/orbit-2x) were not done.
evidence: F/FEEL.md:33,113
status: not done
real data: never looked at
sources: F/FEEL.md:33,113,128

### FEEL-N3 · verify/motion/storm/key-table never ran on symbol, package, graph, find contexts
surface: harness/journeys/lints
problem: verify was stopped on load for graph, symbol, package; the key table ran only in orbit (works 20, wrong 0, dead 16 -> re-test with j first); storm 8 seeds x 300 acts on every scene and matrix on world/graph/symbol/package not run.
evidence: F/FEEL.md:18,57,114; F/FEEL-A.md:7
status: not done; the full verify gate "never gave a verdict this wave" (PR:303). J11 (key table) written but unrun (M-final M2)
real data: fixture
sources: F/FEEL.md:14-18,57,114; F/FEEL-A.md:7; PR:303

### FEEL-N4 · key-table dead rows: j>s (PeelSource) and j>cmd-d (Hold) change nothing with a Library row focused
surface: keys/focus
problem: Hold gives no feedback in the Library and Peel has nothing to peel on a package row; 16 rows were dead in orbit (mostly "nothing to act on") and 1 real silence (Cmd-Shift-C).
evidence: F/FEEL.md:56 (net/keys-orbit/keys.txt, net/keys-rows/keys.txt)
status: unresolved (Hold-in-orbit feedback undecided)
real data: fixture
sources: F/FEEL.md:56

### FEEL-N5 · popovers: A to B moves and gallery hover A-B not measured; Library first card never measured
surface: overlays/popovers
problem: A to B popover transition (no empty frames) was only proven on the canary.
evidence: F/FEEL.md:40
status: not done
real data: never looked at
sources: F/FEEL.md:40

### FEEL-N6 · first paint flash before the first quiescent draw cannot be seen in the harness
surface: motion/transitions
problem: The harness takes its first frame after the first quiescent draw, so a flash before first paint needs a real window.
evidence: F/FEEL.md:50
status: never checked in a real window
real data: never looked at
sources: F/FEEL.md:50

### FEEL-N7 · QA kit: harness does not publish element boxes/focus, so keys.py and cliffs.py use pixel diff and text boxes
surface: harness/journeys/lints
problem: Adding targets(bounds+hovered/focused) to gallery/cli.rs frame_json and overlay to harness.rs sample_state would make both exact; proposed, not done.
evidence: F/FEEL.md:124; F/KIT.md:12-14
status: not done (partly overtaken by journeys' state words: final/F-Shell-1.md:72)
real data: N/A
sources: F/FEEL.md:124; F/KIT.md:12-14

### FEEL-N8 · wall-clock figures invalid above load 15; all FEEL numbers are virtual time under load 12-227
surface: performance
problem: No real (wall-clock, release) frame or latency figure exists for any FEEL measurement; kit warns at load > 15.
evidence: F/FEEL.md:10; F/kit/README.md:15-21
status: still open (release budgets never judged, M-final M47)
real data: never looked at in release
sources: F/FEEL.md:10; F/kit/README.md:15-21

## SECTION 3: review/fit (REVIEW.md = phase 1 04:47; BASELINE.md incl. section 7 "After FLUID-B"). Paths relative to W6/review/fit (R). No REVIEW-DONE.md exists: phase 2 ran (bin harness-p2-0639, logs/p2-*, e2e.sh; last p2 test run killed by SIGTERM) but never wrote a report. Code read-only check on 2026-09-30 shows C1-C6/C8 landed (harness/refusals.rs, probe/rules.rs stranded, fit_tests asserts) though PR:293-299 still lists them as open.

### RFit-2 · no hysteresis at shelf thresholds (124 flips per 2 s)
surface: fluid/phone widths
problem: Frame::resolve was memoryless at 900 and 640 so the shelf parked half open.
evidence: R/BASELINE.md:83-89; sheets/hysteresis-orbit.png
status: FIXED by W-Fluid FLUID-B (flips 124->0 in all six scenes, BASELINE.md:163)
real data: fixture
sources: R/BASELINE.md:83-89,163

### RFit-3 · fast shrink squeezes the reader to ~100 px for ~12 frames (Settings heading breaks mid-word)
surface: fluid/phone widths
problem: On a maximise-then-restore or edge snap the shelf keeps 264 px while the window is 360 px; the COLUMNS_SHARE cap was NOT in the FLUID-B binary R-Fit measured, and the fast sweep showed new fly-in words painted past the edge for 3-5 frames.
evidence: R/BASELINE.md:78-80,173; after/sheets/fast-shrink-t8080-before-after.png
status: unverified: cap covered by a rig test; no post-merge harness measurement recorded (BASELINE.md:173); PR:37 says "no reader squeeze on a fast shrink"
real data: fixture; post-merge binary never measured
sources: R/BASELINE.md:78-80,173; R/REVIEW.md:37; PR:37

### RFit-4 · phone widths: package lede/tiles, symbol strip, graph footer, hit targets
surface: fluid/phone widths
problem: At 320/360/390 the package lede runs past the right edge and tiles are 100 px wide; the symbol page sibling strip and as_integer are cut, "Your code and it" collides with "28 places", 18 hit targets under 24 px, text scale 200 at 360 cuts lede and title.
evidence: R/REVIEW.md:38; R/BASELINE.md:28-29,43-44,102
status: symbol page past-edge widths 81->0 (BASELINE.md:166); package page past-edge widths 86->89 (BASELINE.md:172); FLUID-C: package feature chips and repo URL still past the edge below 416 px (PR:230 "94 of 285")
real data: fixture
sources: R/REVIEW.md:38,141-144; R/BASELINE.md:166,172; PR:230

### RFit-5 · drawer had no scrim, click-through, painted under reader tiles
surface: overlays/popovers
problem: A click beside the drawer navigated the page behind it and the HEADS-UP tile painted over the shelf rows.
evidence: R/BASELINE.md:93-98; drawer/ frames
status: scrim + click-close FIXED by FLUID-B on the Library at 360x640 (BASELINE.md:165); the package page drawer (HEADS-UP over rows) re-check was scripted (e2e.sh drawer-pkg after) but no result recorded
real data: fixture
sources: R/REVIEW.md:39,124; R/BASELINE.md:93-98,165; R/e2e.sh

### RFit-6 · shelf collapse cuts text mid-glyph while fading to 1.2:1 (legibility law 298 findings)
surface: motion/transitions
problem: At every crossing of 900 the shelf text falls to ~1.2:1 for 9-12 frames in a 30-frame (480 ms) animation.
evidence: R/BASELINE.md:70,128,136,175; legibility/
status: cut-text widths 32->31 only; the lead's shelf swap-constant change was not in the measured binary (BASELINE.md:175); F-Shell-1 fixed the swap point in side/mod.rs and a rig test (final/F-Shell-1.md:24) but the legibility law was never re-run on a post-merge harness
real data: fixture
sources: R/REVIEW.md:40,123; R/BASELINE.md:70,175; final/F-Shell-1.md:24

### RFit-7 · Library chips jump between rows with no motion; Find inspector glides through results; symbol page pops its margin marker
surface: motion/transitions
problem: pflag chip moves 554 px in one frame; inspector card crosses result rows (text over text for 3+ frames); "Your code and it" marker pops at 1264-1256 (42 elements).
evidence: R/REVIEW.md:41; sheets/orbit-slow-chip-snap.png, sheets/find-flicker-slow-0.png, sheets/value-snap-1264-1256.png
status: Library ring snap 130->1 (FLUID-B); but the ring's glide is long (108-239 frames to land) and chips cross each other and the caption during it (BASELINE.md:174); Find inspector crossing still open (PR:199); symbol page margin marker: FLUID-C found 5 hard thresholds remaining (PR:246)
real data: fixture
sources: R/REVIEW.md:41; R/BASELINE.md:74-75,164,174; PR:188,199,246

### RFit-8 · 2560: Library, package and symbol pages are an 800 px column in an empty window
surface: fluid/phone widths
problem: FOLIO reader width 784 px is the same at 2560; sparse column. MERGED: also GLYPH-A request: the symbol page column stays 640 in a 2296 reader, wider folio at >= 1800 proposed (glyph/GLYPH-A.md:62); FIT.md:151 'a design measure, unchanged' MERGED: also W-Fit FIT.md:151: reading measure 784 px at 100% text is the same at 2560
evidence: R/REVIEW.md:42,126; sheets/static-{orbit,package,value}-wide.png
status: FLUID-B added wide leaves (PR:201 "re-check"); owner decision on the 2560 layout (PR:364)
real data: fixture
sources: R/REVIEW.md:42; PR:201,364

### RFit-9 · Library at 200% text and 360x900: viewport ends at y~550, six of thirteen chips, caption cut both sides
surface: fluid/phone widths
problem: A 350 px empty band under the reader; caption "649 declarations from 265 of 266 fi" centred and wider than the window; two chips past the left edge at 320.
evidence: R/REVIEW.md:43; R/BASELINE.md:112-114; sheets/cascade-orbit-ts200-360.png, sheets/scroll200-orbit-360x900.png
status: still open per PR:202 (reported 02:30, not re-verified)
real data: fixture (13 fixture packages)
sources: R/REVIEW.md:43; R/BASELINE.md:112-114; PR:202

### RFit-12 · matrix --scene desktop-graph and lint --scene all never finish under load
surface: harness/journeys/lints
problem: "the product never went quiet within 120s" because the wait is 120 s of wall clock; a load fault the harness cannot tell from a graph defect; every scene after the fifth (desktop-code) is not linted by lint --scene all.
evidence: R/REVIEW.md:46,149; R/BASELINE.md:108-110 (graphdiag/)
status: still open (PR:302: should be virtual or bounded by work done); final/F-Data-2.md:27 ran capture --scene all (25 scenes, 9 min) but lint --scene all result unrecorded (M-final M49)
real data: fixture
sources: R/REVIEW.md:46,149; R/BASELINE.md:108-110; PR:302

### RFit-C1 · harness refusal record keyed by source mtimes; a bare run poisons later dev-shell runs
surface: harness/journeys/lints
problem: A run outside hx records 13 Toolchain refusals under the same header and the next dev run replays them "not asked again".
evidence: R/REVIEW.md:52-58 (apps/desktop/src/harness.rs:445-507; fp/noenv.err, fp/withenv.err)
status: FIXED in code (apps/desktop/src/harness/refusals.rs keys on OwnerBuild::of_current_exe + ToolchainEnv + root_digest, retired record file); phase-2 e2e run result (e2e/a,b,c,d) not reported; PR:294 lists it stale as open
real data: N/A
sources: R/REVIEW.md:52-58; R/e2e.sh; PR:294

### RFit-C2 · any ClientError recorded as an owner refusal
surface: harness/journeys/lints
problem: A transport error from a dead owner (two harnesses on one state dir) was written into the refusal record and replayed as fact.
evidence: R/REVIEW.md:60-62 (harness.rs:307-309)
status: FIXED in code with a pinned test (harness/refusals.rs:216-222 fate(): only CommandFailed is a refusal; test :417); PR:295 stale
real data: N/A
sources: R/REVIEW.md:60-62; PR:295

### RFit-C3 · standing() calls a refused root with no Ready row Failed too early
surface: harness/journeys/lints
problem: A Loading row or an unlisted row plus a recorded refusal was called Failed after 3 polls, producing a fault-plate capture.
evidence: R/REVIEW.md:64-66 (harness.rs:403-418; mutant m_loading.py survived)
status: FIXED in code with pinned tests (harness/refusals.rs:267-290 standing(): (Some,Loading) and unlisted-while-awaited are Pending; tests :436-440); mutant kill not quoted
real data: N/A
sources: R/REVIEW.md:64-66; PR:296

### RFit-C4 · "13 of 13 refused, failed() empty" hides behind a Ready row (prior generation captured)
surface: harness/journeys/lints
problem: Every capture was of a preserved generation and nothing on screen said so.
evidence: R/REVIEW.md:68-70 (harness.rs:368-380)
status: FIXED in code (Standing::Preserved(reason) for Ready+refused, refusals.rs:256-278, test :450; Fixture::failed()/preserved lists harness.rs:361-363); the summary line "capturing the prior generation for N roots" not verified
real data: N/A
sources: R/REVIEW.md:68-70; PR:297

### RFit-C5 · held-lock test is prose matching ("AlreadyOwned" in a Debug string)
surface: typing
problem: Renaming the variant silently turns a live lock into a cold 4-minute index.
evidence: R/REVIEW.md:72-74 (harness.rs:547-553); apps/desktop/src/harness.rs:580-615 now HELD_LOCK_WORDS const
status: partly FIXED: still a string const "AlreadyOwned" matched with contains() on refusal.to_string() (harness.rs:586,615), documented but not typed; PR:322 still lists it
real data: N/A
sources: R/REVIEW.md:72-74; PR:322

### RFit-C6 · tuples and parallel indices in the harness (FailedRoot, refused BTreeMap)
surface: typing
problem: Vec<(PathBuf,String)> in Fixture::failed(), a usize-keyed refused map read against a parallel Vec<Standing>.
evidence: R/REVIEW.md:76-79
status: FIXED in code (Refusal{root,reason}, Indexed{path,reply,standing} at harness.rs:361-438); PR:323 stale
real data: N/A
sources: R/REVIEW.md:76-79; PR:323

### RFit-C7 · 174-line fixture_with_progress; lane name w-fit in product paths; non-atomic record write
surface: typing
problem: fixture_with_progress does three jobs; fallback_state builds .local/harness/w-fit-*; unit tests write w-fit-unit-* into the repo and panicking tests leave directories behind; record_failures writes non-atomically; harness compiles unix-only.
evidence: R/REVIEW.md:81-86
status: still open in part: fixture_with_progress still exists (harness.rs:262); fit_tests carry lane names in paths (PR:304); ~30 leftover state dirs under .local/harness (PR:353)
real data: N/A
sources: R/REVIEW.md:81-86; PR:304,324,353

### RFit-C8 · tests that prove nothing: survey_every_scene_at_every_size, umask test, fit_tests findings duplicate the harness lint
surface: harness/journeys/lints
problem: The survey asserted nothing, private_umask has no test (mutant survived), fit_tests re-implements clip/edge/overlap and skips wholly-hidden text, temp dir never removed, allow(too_many_lines) for the module.
evidence: R/REVIEW.md:88-93 (fit_tests.rs:163-190; harness.rs:1760)
status: partly FIXED: survey now asserts (fit_tests.rs:206,227) and SIZES has 6 sizes; a stranded (wholly past a side) rule exists in facet/probe/rules.rs:81 and passes; the umask test, the shared implementation and the three fixture_world_tests asserting retired words (PR:299-301) are unverified
real data: N/A
sources: R/REVIEW.md:88-93; PR:298-301

### RFit-C9 · Targets is Clone; Recall re-implements Targets::focus; Tracked{target: bool}
surface: typing
problem: The D1 class of bug (an action holding the list that holds it) stayed writable.
evidence: R/REVIEW.md:95-98 (focus.rs:65-79,190-205,297)
status: partly FIXED: shell/focus.rs List::Clone(Weak) now makes a cloned list weak (focus.rs:121-154); whether Recall duplication and Publish enum landed unknown; PR:325 says still writable
real data: N/A
sources: R/REVIEW.md:95-98; PR:325

### RFit-C10 · Render::render mutates the model (shelf_over_open reset in render)
surface: typing
problem: root.rs sets shelf_over_open=false inside Render::render (a derived state kept as a bool).
evidence: R/REVIEW.md:100-101 (root.rs:1239-1241); now root.rs:1305
status: still open (code still there at root.rs:1305 on 2026-09-30)
real data: N/A
sources: R/REVIEW.md:100-101; PR:326

### RFit-C11 · hand-rolled breakpoints: titlebar 520/760/560/700, frame.rs vs tokens.rs duplicates, Room::of
surface: fluid/phone widths
problem: Titlebar cutoffs at 520 (inbox), 760 (view switch), 560/700 segments; frame.rs and facet/tokens.rs duplicate constants; Measure::room and Room enum dead.
evidence: R/REVIEW.md:103-104,125
status: FLUID-A/B/ADOPT replaced most; remaining KNOWN list in fluid::tests: anatomy/page.rs:86, page/gallery.rs:433, anatomy/gallery.rs:196, prism.rs:23,267,272; dead Room/Measure::room/columns (measure.rs:111-117) still delete-able (PR:259-261)
real data: N/A
sources: R/REVIEW.md:103-104; PR:259-261

### RFit-13 · titlebar search field shrinks to "As..." at 200% text and 320 px
surface: fluid/phone widths
problem: Ask field is unusable at large text on a phone.
evidence: R/REVIEW.md:125 (sheets/matrix-orbit-abyss-narrow-cols.png)
status: unverified after W-Fluid titlebar rewrite
real data: fixture
sources: R/REVIEW.md:125

### RFit-14 · Settings: disabled back chevron 2.66:1 at every size; Theme row wraps at 352-344
surface: contrast/a11y
problem: One lint finding on every Settings frame; the Theme row wraps.
evidence: R/REVIEW.md:130; R/BASELINE.md:39,56
status: unverified (J10 Settings frames pass the zero-lint rule per final/F-Shell-1.md:41, which suggests the chevron is fixed or exempt)
real data: REAL for J10
sources: R/REVIEW.md:130; R/BASELINE.md:39; final/F-Shell-1.md:41

### RFit-15 · Graph: focus card jumps 588 px at each mode change, no motion
surface: graph/world
problem: The focus card still jumps at 888, 840, 672, 648 (BASELINE.md:168); footer line collides with status below 400 px.
evidence: R/REVIEW.md:129; R/BASELINE.md:55,168
status: footer collision resolved (FEEL-D11); card jump still open after FLUID-B
real data: fixture world
sources: R/REVIEW.md:129; R/BASELINE.md:55,168

### RFit-16 · Popovers at 360 not measured; keys, hover, popover latency not run in the R-Fit phase
surface: overlays/popovers
problem: R-Fit measured only the jump-bar sibling menu at 360x640 (stays inside, capped, scrolls).
evidence: R/REVIEW.md:31; R/BASELINE.md:104-106
status: not done
real data: fixture
sources: R/REVIEW.md:31; R/BASELINE.md:104-106

### RFit-17 · Settings matrix cannot script its key act; Settings at 200% variant lint unfinished
surface: harness/journeys/lints
problem: matrix cannot open Settings (a key act), so Settings is only spot-linted.
evidence: R/BASELINE.md:39,44,139
status: unresolved
real data: N/A
sources: R/BASELINE.md:39,44,139

### RFit-18 · Library chip cascade at 200%: contrast findings at 1.2 s were chips under a mask
surface: harness/journeys/lints
problem: Matrix lint at 1.2 s reports chips at 1.02:1 that are still unpainted, so lints on cascades give false failures or hide real ones.
evidence: R/BASELINE.md:34,114
status: unresolved (capture time vs cascade length)
real data: fixture
sources: R/BASELINE.md:34,114

### RFit-19 · fit_tests/fluid_tests carry lane names and leave temp dirs; state dirs pile up
surface: harness/journeys/lints
problem: ~30 .local/harness state dirs left by panicking tests; nix cleanup is the owner's call.
evidence: R/REVIEW.md:83,92
status: owner decision (PR:353)
real data: N/A
sources: R/REVIEW.md:83,92; PR:304,353

## SECTION 4: package page: review/folio/REVIEW.md (R-Folio phase 1, 03:00), folio/FOLIO-A.md (~02:00), folio/FOLIO-B.md (03:15). No FOLIO-C.md and no R-Folio REVIEW-DONE.md exist: W-Folio's lane wrote nothing after 03:15; the working tree shows later unreported edits (PageTarget doors for crest cells folio.rs:215-221, Esc folds via targets.escape() root.rs:993, badge tip on the float layer marks/badges/view.rs:2, description whitespace normalised package.rs:178, heads OVERLAP 6, dwell via QUICK_REST in folio/state.rs:13, ticker from source.releases data.rs:551). Those are code observations, not lane statements: every one is unverified visually. Paths relative to W6/review/folio (RF) and W6/folio (F).

### RFolio-D1 · live resize flickers the whole lower page (crest flips 1 vs 2 rows on alternate frames)
surface: package page
problem: Features bar, header and territory jump 150 px between y=484 and y=630 while the window is dragged; ADVISORIES drifts over Features chips and the weight cell (181 overlap, 44 faded findings). MERGED: also R-Fit: package tile row flickers between two layouts every other frame while dragging, 39 A-B-A pixel frames, 1381 element events; after FLUID-B A-B-A pixels 39->51, text past edge 86->89 widths (review/fit/REVIEW.md:35; BASELINE.md:52,59-62,172)
evidence: RF/REVIEW.md:106 (geo/features-y-during-resize.txt; sheets/resize-adjacent-frames.png; repro resize-sweep.script)
status: still open: FOLIO-B says "Not fixed by this state" (F/FOLIO-B.md:87) and the R-Fit re-measure after FLUID-B shows A-B-A pixels 39->51 (R-Fit BASELINE.md:172); harness storm seed 3 (flow-heads.x jumped 30.5 px at a text-scale change) also open; explicit rows per arrangement never landed (crest.rs:290 still flex_wrap)
real data: fixture (toml, tokio harness scenes)
sources: RF/REVIEW.md:49,106,136; F/FOLIO-B.md:55,87; review/fit/BASELINE.md:172

### RFolio-D2 · fresh window 1440-1600 wide drops ADVISORIES onto its own row
surface: package page
problem: Three crest cells then ADVISORIES alone (156 px extra height); hypothesis (untested): four-arrangement cell widths sum exactly to the row width and float error wraps the fourth; fluid.rs test allows +0.5 and never looks at painted rows.
evidence: RF/REVIEW.md:108 (sheets/new-widths-1920-1152.png rows 1600 and 1440)
status: still open per FOLIO-B (F/FOLIO-B.md:88); "a painted-row test is owed"; PR:226
real data: fixture
sources: RF/REVIEW.md:81,108; F/FOLIO-B.md:88; PR:226

### RFolio-D3 · release ticker and time-travel banner absent on a package indexed from an unpacked registry directory (is_local)
surface: releases/time travel/upgrade
problem: page_mapping.rs:1754 gives versions=Unknown(LocalProject) for a local root so data::ticker returns None and the banner (drawn inside the ticker block) disappears: the address says "viewing 0.5.11" and nothing on the page does.
evidence: RF/REVIEW.md:110 (sheets/toml-board-vs-native-1440.png; shots/past/at-0.5.11/...t1800@1x.png)
status: unverified: tree now prefers source.releases (data.rs:551-567) but the banner is still tied to the ticker and no lane reports a check; on the FINISH installs every package is a registry-cache root (M-final M7 path); real diffs still fixture (G14)
real data: fixture, then real installs: never looked at
sources: RF/REVIEW.md:110,167; F/FOLIO-B.md:89,105; PR:217

### RFolio-D4 · badge hover reflows the card and the grid below (86,600 px change on alternate frames; sentence clipped without ellipsis)
surface: package page
problem: The tip opens inside the badge, the badge widens, the next badge wraps, cards below move.
evidence: RF/REVIEW.md:112 (sheets/badge-hover-zoom.png, badge-hover-grid.png, gal/badge-hover/)
status: probably FIXED in the tree (marks/badges/view.rs header says the sentence floats on the window's float layer) but no lane file reports it; unverified
real data: fixture
sources: RF/REVIEW.md:36,112; F/FOLIO-B.md:90; PR:227

### RFolio-D5 · heads-up stack unreadable at rest (each chip covers half the previous glyph)
surface: package page
problem: Chip 28 px, overlap 14 px, only the top chip's warning glyph is whole.
evidence: RF/REVIEW.md:114 (sheets/heads-stack-rest-zoom.png; repro NUDOX_FOLIO_ONLY=tokio-heads)
status: unverified: OVERLAP is now 6.0 in facet/src/folio/heads.rs:174 (was 14 per review); no lane reports a picture; PR:233 still lists it
real data: fixture
sources: RF/REVIEW.md:114; F/FOLIO-B.md:91; PR:233

### RFolio-D6a · Cargo description with a newline renders as a hard break
surface: package page
problem: "Provides" sat alone on a line at every width.
evidence: RF/REVIEW.md:116
status: FIXED in the tree (package.rs:178 normalises whitespace); not reported by a lane; hero lede at 120 px in a 480x320 window (storm seed 1: lede needs 587 px in a 120 px box) still open per FOLIO-B (F/FOLIO-B.md:54,92)
real data: fixture
sources: RF/REVIEW.md:116; F/FOLIO-B.md:54,92

### RFolio-D6b · hero lede and byline run past the right edge at 430 px and below
surface: fluid/phone widths
problem: See RFit-4.
evidence: RF/REVIEW.md:116
status: FIXED by FOLIO-B for the name (fit_name) and the words column min_w_0; tested at 14 widths (F/FOLIO-B.md:31-32,101); but FLUID-C later found 94 of 285 sweep widths with text past the edge (feature chips and repo URL from 416 px down) (PR:230)
real data: fixture
sources: RF/REVIEW.md:84,116; F/FOLIO-B.md:31,92,101; PR:230

### RFolio-D7 · keyboard on the package page: crest cells, features bar, ticker, berg are not targets; Esc does not fold a module; stale focus ring after Enter; two focus rings at once
surface: keys/focus
problem: (a) heads-up sheet, berg toggle, feature switches and ticker arrows are pointer-only; (b) Esc does not fold an open module; (c) stale ring the size of the old region (63x86 px) after Enter on a module; (d) storm: two focus rings (tb-shelf and module-card-1) and a feature chip focus off screen at 480x320.
evidence: RF/REVIEW.md:118 (sheets/tabs-3-4-5.png, keys-tab-esc.png, x2-opened-crop.png; logs/storm-replay1.log; gal/lint-features.txt)
status: (a) crest cells now doors in the tree (folio.rs:215-221 PageTarget::Licence/Heads/Weight); (b) Esc now asks the reader to fold first (root.rs:992-994); (c) and (d) and features/ticker/berg doors unverified; all unreported by a lane; PR:179 still lists D7a-c open
real data: fixture
sources: RF/REVIEW.md:66-76,118; F/FOLIO-B.md:93; PR:179

### RFolio-D8 · no pointer cursor over regions, shingles, ticker bars and berg blocks
surface: package page
problem: Hand-written Elements (Shingles, Ticker, Berg) never call set_cursor_style.
evidence: RF/REVIEW.md:62,120 (grep -n cursor apps/facet/src/folio/*.rs empty)
status: still open (grep on 2026-09-30 finds no set_cursor_style/cursor_pointer in facet/src/folio/{shingles,ticker,berg}.rs); the harness cannot see the pointer cursor at all
real data: never looked at (harness cannot show it)
sources: RF/REVIEW.md:62,120; F/FOLIO-B.md:94; PR:236

### RFolio-D9 · package page popovers open with no dwell (three cards in a row when the pointer crosses the crest)
surface: overlays/popovers
problem: Licence stamp, heads-up, weight plate open one after another; house rest QUICK_REST 120 ms.
evidence: RF/REVIEW.md:30,122 (sheets/cross-strip.png)
status: probably FIXED in the tree (folio/state.rs:13 LIFT.delayed(QUICK_REST)); unverified; no lane reports it
real data: fixture
sources: RF/REVIEW.md:30,122; F/FOLIO-B.md:94; PR:184

### RFolio-D10 · crest re-lays out on arrival (first frame does not know the reader's bounds)
surface: package page
problem: page_measure uses ctx.reader_scroll.bounds(), unknown on frame 1, so the first Modes/Flow epoch is for the wrong width then animates (crest moves 17 px over 16-192 ms).
evidence: RF/REVIEW.md:124 (sheets/arrival-new-strip.png, tokio-arrival-0-vs-900.png)
status: still open (F/FOLIO-B.md:95); needs Ctx content width from W-Fit; PR:228
real data: fixture
sources: RF/REVIEW.md:51,124; F/FOLIO-B.md:95; PR:228

### RFolio-D11 · module open and berg open are cuts; Phase C "shingles fly to their cards" not built
surface: motion/transitions
problem: Opening a region changes 178,748 px in one frame and berg open 208,847 px with the page jumping 200 px; shingle hover has no easing.
evidence: RF/REVIEW.md:41-42,126 (shots/hover/opened/, shots/hover/weight/)
status: still open (Phase C never built: F/FOLIO-A.md, F/FOLIO-B.md:96); PR:229,237
real data: fixture
sources: RF/REVIEW.md:41-42,126; F/FOLIO-B.md:96; PR:229,237

### RFolio-D12 · README block: reference paragraphs print raw URLs, dangling "(LICENSE-APACHE or )", nothing clickable, left edge off the folio's, clips at 480 px
surface: package page
problem: The README is not rendered as a document: link refs unresolved, no links, misaligned.
evidence: RF/REVIEW.md:128 (sheets/scroll-old.png x=212 vs 137)
status: partly fixed: images and badge rows gone (F/FOLIO-B.md:97), the rest open; PR:218-223
real data: fixture (toml, tokio, serde_json README); the README of a real cache package (12 packages) never looked at beyond toml
sources: RF/REVIEW.md:128; F/FOLIO-A.md:114; F/FOLIO-B.md:97

### RFolio-D13 · region labels clip 0.7-1.2 px below 900 px and at 200% text; weight caption clips
surface: contrast/a11y
problem: LABEL_BAND*k with k below the text scale while the label keeps its full line height; matrix: 152 of 180 cells fail on this one class; "20 packages beneath" sits in an 11.5 px box for a 16 px line.
evidence: RF/REVIEW.md:59,130 (lint/toml-abyss-normal-200.txt; gal/lint-berg.txt); F/FOLIO-B.md:57
status: still open per FOLIO-B (F/FOLIO-B.md:98); PR:235
real data: fixture
sources: RF/REVIEW.md:59,130; F/FOLIO-B.md:57,98; PR:235

### RFolio-D14 · advisories cell inert; badge hit targets 21 px tall (30 lint findings)
surface: contrast/a11y
problem: No handler on the ADVISORIES cell (no card even when advisories exist); badge targets 40.5x21 under 24x24.
evidence: RF/REVIEW.md:35,132 (shots/hover/advis/; gal/lint-cards.txt)
status: still open (F/FOLIO-B.md:99); the ADVISORIES feed itself is undecided (no feed configured, owner unanswered: PR:150-153)
real data: fixture; no advisories exist in the app
sources: RF/REVIEW.md:35,132; F/FOLIO-B.md:99; PR:150,200

### RFolio-D15 · ticker label plate overlaps the "your pin" flag when the pin is near
surface: package page
problem: Plate right edge 11 px over the flag text.
evidence: RF/REVIEW.md:33,134 (sheets/tokio-ticker-hover.png)
status: known, unfixed (F/FOLIO-A.md:115)
real data: fixture
sources: RF/REVIEW.md:33,134; F/FOLIO-A.md:115

### RFolio-N1 · "Yours" (mint shingles, "you use N") is not fed on the package page
surface: package page
problem: Shingles are drawn with yours: false; the board takes usage from sym_uses.json and the app has no such data on the page; the header says nothing about "you use N".
evidence: F/FOLIO-A.md:110; RF/REVIEW.md:134 ("you use N, examples, Your code and it" absent by design)
status: still open
real data: never looked at with a real project's usage
sources: F/FOLIO-A.md:110; RF/REVIEW.md:134

### RFolio-N2 · index-fed signature/summary on outline nodes do not exist; names take declaration and first sentence from the source scan
surface: data-feed
problem: A package without source shows kinds only; brief_of returns (None, None) stub in data.rs:111.
evidence: F/FOLIO-A.md:111,120; RF/REVIEW.md:151
status: requested from W-Open (OutlineNode.signature/summary); unresolved in my files (index rows for the 12 real packages carry signatures? unknown)
real data: fixtures only
sources: F/FOLIO-A.md:111,120; RF/REVIEW.md:151,186

### RFolio-N3 · past tint: the page as a whole is not warmer like the board's; berg block count ignores features you might switch on
surface: releases/time travel/upgrade
problem: Only the banner, amber border and shingle recolour say the page is in the past; the weight berg follows the lock/default features, not the features bar's selection.
evidence: F/FOLIO-A.md:112-113
status: known approximation, open
real data: fixture
sources: F/FOLIO-A.md:112-113

### RFolio-N4 · README badge markdown / first paragraph, process count undercounts
surface: package page
problem: Board rule for "process" does not count use std::process::{Command,...}; adding the pattern makes tokio 8 which the oracle test rejects, so the reader undercounts.
evidence: F/FOLIO-A.md:116
status: known approximation
real data: N/A
sources: F/FOLIO-A.md:116

### RFolio-N5 · module view with 14+ names is a page of its own; very long names (90-char paths) never exercised; 150% text not run
surface: package page
problem: No fixture with very long names; 85/100/200% run only.
evidence: RF/REVIEW.md:86,100
status: not done
real data: never looked at
sources: RF/REVIEW.md:86,100

### RFolio-N6 · advisories, licence, sheet at 360; only heads-up sheet at 360 checked; theme glacier focus ring not checked
surface: contrast/a11y
problem: Focus ring visible in abyss only checked; storm seeds 3-4 not run to the end.
evidence: RF/REVIEW.md:64,75
status: not done
real data: fixture
sources: RF/REVIEW.md:64,75

### RFolio-C1 · code: package page typing debt (Option<Option>, tuples, string protocols, bare bools/f32, allow attributes, dead code)
surface: typing
problem: source_facts Slot.project Option<Option<PathBuf>>; shingles Region rect tuples and direction &str; crest match decision.as_ref() {"deny"}; "pkg-card-" string protocol (now PageTarget partly); heads Signals tuples; manifest graph() 4-tuple; bare bools in scan/docs/manifest; 8 too_many_lines and ~24 cast allows; dead let _ = stubs; brief_of stub; berg indexes blocks[at] directly (panic on malformed lock).
evidence: RF/REVIEW.md:140-168
status: FOLIO-B converted many (state.rs enums, Need/Stage/DefaultFeatures/Origin, Want struct) (F/FOLIO-B.md:22-25); remaining items unreported; phase 2 (R-Folio) never produced a report
real data: N/A
sources: RF/REVIEW.md:140-168; F/FOLIO-B.md:21-25

### RFolio-C2 · code: three hand-written Elements duplicate hover/keys/probe; two keyboard systems for one grid (shingles own keys vs with_doors)
surface: keys/focus
problem: Shingles/Ticker/Berg re-implement hover, focus overlay, hitbox, probe with key: String::new(); k duplicated so doors can drift from paint; paint fns of 357/248/235 lines; mirror types source_facts::Berg vs facet BergFacts; enrich duplicates loops; absolute paths in probe keys ("folio-/Users/.../toml-0.8.23-...").
evidence: RF/REVIEW.md:154-161
status: unresolved in my files
real data: N/A
sources: RF/REVIEW.md:154-161

### RFolio-C3 · code: model vs view: package data.rs lives in the shell and reaches across via super::super::super
surface: typing
problem: backend_library-to-word advisory mapping belongs in model/pages; shelf helpers reached across the shell.
evidence: RF/REVIEW.md:159
status: unresolved
real data: N/A
sources: RF/REVIEW.md:159

### RFolio-C4 · tests missing: painted-row equality of the four crest cells at 1280-1700, ticker on registry package with unknown versions, banner without a ticker, Esc folds a module, crest cells reachable by j, hero newline, heads chip boxes not overlapping; package/tests.rs and facet tests define their own rigs
surface: harness/journeys/lints
problem: Existing arithmetic test allows +0.5 and one test (resting_on_a_badge_opens_its_meaning_inside_it) pins the in-place tip R-Folio shows is wrong.
evidence: RF/REVIEW.md:163
status: unresolved (a_page_after_a_resize_storm_equals_a_fresh_page and the_page_holds_together_at_every_width exist since FOLIO-B)
real data: N/A
sources: RF/REVIEW.md:163; F/FOLIO-B.md:32

### RFolio-C5 · source_facts reading never evicts: a large crate is scanned once per project per process
surface: performance
problem: reading() re-reads when the project changes but keeps every previous scan.
evidence: RF/REVIEW.md:166
status: not user visible today; open
real data: N/A
sources: RF/REVIEW.md:166

### RFolio-A1 · folio capture bin is a lane tool, not product; hard-coded fixture packages
surface: harness/journeys/lints
problem: backend-desktop-folio-capture.rs boots the real shell on fixture reads for tokio, serde_json, toml; recommended NOT committed as product; the harness only has desktop-package-toml and desktop-package scenes; "scenes for tokio and a local project with a lock would let the checkpoint set match the brief".
evidence: F/FOLIO-B.md:79-81; F/FOLIO-A.md:123
status: decision open (git status shows it modified/tracked?); real-package scenes missing
real data: fixture
sources: F/FOLIO-A.md:123; F/FOLIO-B.md:79-81; RF/REVIEW.md:163 (bin compile failure 02:11)

### RFolio-A2 · FOLIO-B storm seeds: seed 1 hero lede clip at 480x320; seed 2 titlebar.tb-flow-name.x jumped 84 px; seed 3 flow-heads.x jumped 30.5 px
surface: motion/transitions
problem: Three deterministic verify --storm failures on desktop-package-toml; matrix 28/180 (later "152 of 180") cells fail on one class (region label clip).
evidence: F/FOLIO-B.md:49-57
status: OPEN "mine / W-Fit / mine with W-Fluid"; never re-run
real data: fixture
sources: F/FOLIO-B.md:49-57

### RFolio-A3 · perf and hover cards at 16 ms frames not repeated; two identical full desktop lib runs still owed
surface: harness/journeys/lints
problem: FOLIO-B could not finish the full desktop run after load hit 150; hover cards at 16 ms not repeated by W-Folio; perf not run (debug build).
evidence: F/FOLIO-B.md:11,49,59
status: superseded by later green runs (final/F-Shell-1.md:1-20)
real data: N/A
sources: F/FOLIO-B.md:11,49,59

## SECTION 5: symbol page: review/sym6/REVIEW.md + notes.md (R-Sym6 phase 1, 04:30), sym6/SYM6-A.md (04:2x), sym6/SYM6-B.md (08:36). SYM6-B says harness real content evidence is "see SYM6-C": NO SYM6-C.md exists and no R-Sym6 phase-2/REVIEW-DONE exists. Paths relative to W6/review/sym6 (RS) and W6/sym6 (S).

### RSym-D1 · phone widths: written type runs past plate edge, mint case count overlaps it, error kinds cut, call rows collapse at 200% text on 360/320
surface: fluid/phone widths
problem: ty_el gives written type flex_none max_w(340), kinds use non-wrapping said, label (58 px x scale) and joint (28 px x scale) columns are flex_none with no stacked mode; matrix 20/24 cells pass, 360w and 320w at 200% fail both themes (s6-fail-when needs 844 px in 34 px box).
evidence: RS/REVIEW.md:56 (sheets/sweep-sym6-value-narrow.png, sweep-sym6-from-str-narrow.png, matrix/from-str/...abyss-comfortable.png, cliffs.txt overflow at 360 and 336)
status: still open (PR:243); FLUID-C also lists it; not addressed in SYM6-B
real data: gallery fixtures (board facts); real symbol pages at phone widths never swept (FLUID-C "real content" sweep: see FLUID-C records)
sources: RS/REVIEW.md:45-46,56; RS/notes.md:53-54,56; PR:243

### RSym-D2 · symbol page layout changes are instant cuts (rail beside to below, verbs two columns to one)
surface: motion/transitions
problem: The rail (a whole column) swaps in one frame (1140 at t=208, gone at 1080 at t=224); layout.epoch is computed but never fed to a Flow; FLUID-C finds 5 hard thresholds on real content, worst SYMBOL_RAIL edge at 1344 where s6-block-source-sub moves 1084 px in one frame. MERGED: also FLUID-C: 5 symbol-page hard thresholds on real content: 1016-1008 (80 elements, 278 px), 1152-1144 (60, 431 px), 1352-1344 (SYMBOL_RAIL, s6-block-source-sub moves 1084 px), 1032-1024, 528-520 (fluid/FLUID-C.md:26-27,64)
evidence: RS/REVIEW.md:58 (film/rail-flip/rail-strip.png; repro film.py sym6-alloc ...)
status: still open: SYM6-A says "hysteretic but not yet animated (needs Flow::epoch(layout.epoch))" (S/SYM6-A.md:75); PR:245-246; FLUID-C "Next" (F-Shell-1.md:20 flow land() landed for reflow only)
real data: gallery + FLUID-C real content sweeps
sources: RS/REVIEW.md:34,58; S/SYM6-A.md:75; PR:245-246

### RSym-D3 · package picker: Escape/chip toggle did not close it, no motion, no keyboard
surface: overlays/popovers
problem: workspace.rs:package_menu was a hand-rolled deferred plate with mouse-down-out.
evidence: RS/REVIEW.md:60 (film/interact/moments.png)
status: FIXED by W-Sym6 SYM6-B (workspace.rs::open_menu on overlay::menu; arrows, Enter, type-ahead, Esc, chip re-press; rig test picking_a_package_from_the_menu_narrows_the_list_and_escape_closes_the_menu); also fixed facet::overlay::float::menu_open (arrows/Enter were swallowed by the shell walk for any menu but the jump bar's) (S/SYM6-B.md:16,31-33). PR:177 tells to check every other popover that takes arrows: Find inspector, release picker, Add dialog's completion list (not checked)
real data: rig + gallery film; the menu on a real workspace with many packages not seen
sources: RS/REVIEW.md:60; S/SYM6-B.md:16,31-33,42-43; PR:177

### RSym-D4 · verbs do not read like the board (Value reads 305 places, names 170; board reads 368, makes 47, changes 1); AllocationInfo fields lack "you read it - N"
surface: symbol page
problem: verb_of classified by line text; a pattern arm as construction; names x266.
evidence: RS/REVIEW.md:62 (sym6/agree.log DIFFER lines; shots/g1/sym6-value-*.png)
status: partly FIXED by SYM6-B (classifier reads what stands around the token the index's span covers; pattern versus construction; field read/write); STILL OPEN: line-local, "a pattern whose => is on the next line reads as a construction" (S/SYM6-B.md:50); PR:248 lists it; the classifier's agreement test threshold (19/51 for AllocationInfo, RS C4) unverified
real data: rig with 4 pinned UseLines and board data; the real Value page on the toml_pin install: "toml's Value looks right" (PR:34) is the lead's statement only
sources: RS/REVIEW.md:62,95; S/SYM6-B.md:13,50; PR:34,248

### RSym-D5 · enum case rows carry bare mint numbers with no words
surface: symbol page
problem: The spec says no number without a reason to read it; struct rows say "you read it - N", cases do not (partly: SYM6-A added "you match it - 2 - build it - 1" case words in derive/tests.rs per SYM6-B).
evidence: RS/REVIEW.md:64; S/SYM6-B.md:29
status: unverified (SYM6-B mentions case words test in derive/tests.rs; PR:249 still lists open)
real data: gallery + rig
sources: RS/REVIEW.md:64; S/SYM6-B.md:29; PR:249

### RSym-D6 · hover/cursor feedback missing on Example fold line, "N more in workspace", rail source link; calls chip hover <6/255
surface: overlays/popovers
problem: Zero changed pixels on hover; also not keyboard targets (K row) - SYM6-B made every action a stop in the walk >= 24 px (rig test) but the hover treatment is unchanged.
evidence: RS/REVIEW.md:66 (film/hover/hover-crops.png)
status: keyboard part FIXED (every_action_on_the_page_is_a_stop_in_the_walk_and_at_least_24_px); hover part still open (PR:250)
real data: gallery only
sources: RS/REVIEW.md:44,66; S/SYM6-B.md:28; PR:250

### RSym-D8 · glacier contrast: quiet "imports" chip 4.23:1
surface: contrast/a11y
problem: kit.rs::chip dims the whole chip with opacity 0.85; glacier value/alloc/serialize scenes fail.
evidence: RS/REVIEW.md:70 (lint/themes.txt)
status: claimed FIXED by SYM6-B ("imports quiet, no opacity", S/SYM6-B.md:14); glacier lint on the new symbol page never quoted; PR:251 lists it open
real data: gallery
sources: RS/REVIEW.md:38,70; S/SYM6-B.md:14; PR:251

### RSym-D9 · function/method kind mark reads as a disclosure caret; enum mark is stacked lines not the board's diamond; two icon systems (facet::icons::kind_mark vs symbol/ink.rs)
surface: symbol page
problem: The set is drawn two ways; glyph fidelity to the board.
evidence: RS/REVIEW.md:72 (sheets/icons-zoom.png, sheets/sbs-value.png); S/SYM6-A.md:47
status: still open (PR:252); GLYPH-A/B lane may address (see glyph records)
real data: gallery
sources: RS/REVIEW.md:41,72; S/SYM6-A.md:47; PR:252

### RSym-D10 · fidelity gaps: "Next to it" name tails/no outcome glyphs; backticks in doc links; Gives row drops Option/Result written type; "343 crates, 83% derive" missing; opt shows "options" twice; Elsewhere in the registry never drawn; lede prints [measure()] brackets
surface: symbol page
problem: Several board details are absent: Beside{signature:None} in shell/bodies/symbol/facts.rs; no derived flag in index; no cross-registry census.
evidence: RS/REVIEW.md:74-81
status: partly: Gives-row inner_text bug gone (function removed); "83% derive" and "Elsewhere in the registry" explicitly deferred (S/SYM6-B.md:51-52); Beside signature open; PR:253-258
real data: gallery + rig
sources: RS/REVIEW.md:74-81; S/SYM6-B.md:51-52; PR:253-258

### RSym-K · symbol page keyboard in the real shell not run
surface: keys/focus
problem: R-Sym6 could not run J/K, Enter, Tab, Escape, Space peek in the shell (harness did not boot); Escape closing the package menu now covered by a rig test; Space peek and other keys untested.
evidence: RS/REVIEW.md:44
status: partly covered by SYM6-B rig tests; real-shell key sweep (J11) not run
real data: never looked at on the real owner
sources: RS/REVIEW.md:44; M-final M2

### RSym-N1 · never measured: OpenSource click dispatch, generic card dismissal in shell, perf, Python/TypeScript pages from the real index, reduced motion, very long names, back-navigation scroll restore, first frame after launch, error card, A-to-B, hover clears on leave/scroll/key/route
surface: symbol page
problem: R-Sym6 phase 1 left 14 validation rows NOT RUN because the desktop harness did not boot at 03:46 (clang -print-sysroot failing); SYM6-B closed the OpenSource click via a rig test (a_click_on_a_place_opens_that_file_at_that_line_in_the_editor).
evidence: RS/REVIEW.md:16-21,30-31,35,43,50,52
status: mostly unverified; Python and TypeScript pages from the real index never opened in the app (PR:34 speaks only of Rust toml Value)
real data: never looked at (non-Rust symbol pages on real packages)
sources: RS/REVIEW.md:16-21,30-31,35,43,50,52; RS/REVIEW.md:144

### RSym-N2 · symbol pages need the source on disk: places from unreadable files are left out
surface: symbol page
problem: A place whose file cannot be read (a registry package, a deleted file) is left out, never guessed, so "N places" counts what the workspace can show; a registry crate's own usage count of a symbol (e.g. serde_core used by toml) is not shown.
evidence: S/SYM6-B.md:49
status: by design; product implications for dependency packages not judged; F-Data-2 symbol page as_str has a false place in a license comment (M-final M27)
real data: REAL as_str page: false place found
sources: S/SYM6-B.md:49; final/F-Data-2.md:51,101

### RSym-N3 · editor setting missing: Settings has no editor field; configured editor is None; failed launch shows nothing?
surface: settings
problem: host/editor.rs always tries code -g, zed, then the platform opener; no place to choose the editor; a failed launch returns io::Error but the GUI reaction unrecorded.
evidence: S/SYM6-A.md:30; S/SYM6-B.md:53; RS/REVIEW.md:127
status: still open
real data: never looked at with a real editor launch
sources: S/SYM6-A.md:30; S/SYM6-B.md:53; RS/REVIEW.md:127

### RSym-N4 · sticky rail: the rail scrolls with the page (spec: sticky)
surface: symbol page
problem: Known approximation of SYM6-A.
evidence: S/SYM6-A.md:75
status: still open
real data: N/A
sources: S/SYM6-A.md:75

### RSym-N5 · error kinds need the error type's page: the shell does not chain Result alias -> Error -> Category, so "If it fails" shows kinds only in tests or the gallery
surface: symbol page
problem: On real symbol pages the fails block lacks the kinds of the error.
evidence: S/SYM6-A.md:75
status: still open (unverified on real data)
real data: real pages show no kinds
sources: S/SYM6-A.md:75

### RSym-N6 · types for untyped languages read "anything" (dotted); ports of Python/JS docs
surface: symbol page
problem: The page says "anything" where the board hand-writes "a pattern".
evidence: S/SYM6-A.md:47,75
status: expected approximation; real TS/Python/Go packages never rendered in the app
real data: never looked at
sources: S/SYM6-A.md:47,75

### RSym-N7 · desktop mutation queue (generic card label, fails when, case holds, editor order) not run in SYM6-A; two identical green rig runs owed
surface: harness/journeys/lints
problem: Rig hang in shell/status.rs (whisper loop) blocked it; whisper loop fixed later (FEEL-D4); SYM6-B ran 18 symbol tests green at 08:1x; desktop mutation queue result never quoted; "rail beside" mutation was invalid (duplicate arm) and to be redone in B.
evidence: S/SYM6-A.md:54,69,71
status: unresolved in my files
real data: N/A
sources: S/SYM6-A.md:54,69,71

### RSym-C1 · code: inner_text drops the written type; is_later/is_many unguarded; language-specific rules (Io, serde_json) in general code; verb classifier from line text
surface: typing
problem: C1 (inner_text None), C2 (Task/Stream/Iter/Deferred read as later/many), C3 (serde/which.sync rules in general derive), C4 (verb_of line text).
evidence: RS/REVIEW.md:89-95
status: C1 FIXED (inner_text no longer exists), C2 FIXED (words.rs:708,731 require !args.is_empty()), C3 addressed by derive/known.rs (named-rule table), C4 partly (see RSym-D4); verified only by reading the tree on 2026-09-30
real data: N/A
sources: RS/REVIEW.md:89-95; S/SYM6-B.md:57-58

### RSym-C2 · code: typing (FoldKey::Options(String), Option<Option>, bare bool call sites, tuples in signatures, String addresses) and abstraction (re-parsing signature text, 24 Hsla per ink() call, per-frame derive/compile/read_all, parallel hover/menu/spacing systems, name_width guess 8.6 px per char)
surface: typing
problem: C5-C9 of R-Sym6; R-Sym6 phase 2 never ran; a few closed by SYM6-B (uses reader deleted C8, package menu on overlay Menu, imports chip by token) and W-Open3 I3.
evidence: RS/REVIEW.md:97-119
status: unresolved for C5, C6, C7 (perf unmeasured), C9; C8 FIXED (S/SYM6-B.md:11: UseLine on the page, Reading placeholder deleted)
real data: N/A
sources: RS/REVIEW.md:97-119; S/SYM6-B.md:11

### RSym-C3 · code: shell reader SymbolFold dead variants; editor template split; gallery facts built from board JSON; gallery Host::target no-op so lint sees 0 targets
surface: harness/journeys/lints
problem: C10 (dead SymbolFold variants), C12 (gallery matches board by construction, no real signatures, 0 targets linted), C13 (editor template split, fixed in SYM6-B).
evidence: RS/REVIEW.md:121,125,127
status: C13 FIXED (S/SYM6-B.md:17,53); C12 partly (gallery host publishes its targets now, S/SYM6-B.md:41); C10 unverified
real data: N/A
sources: RS/REVIEW.md:121,125,127; S/SYM6-B.md:17,41

### RSym-A1 · the desktop harness did not boot at 03:46 (Clang driver sysroot probe "-print-sysroot" unknown argument); after 03:12 the fixture index was empty
surface: harness/journeys/lints
problem: Committed code meeting the current toolchain broke every shell-level proof for hours (all reviewers' shell evidence was gallery/rig only); fixed by W-Index product probe (PR:146).
evidence: RS/REVIEW.md:144; S/SYM6-A.md:42
status: FIXED (W-Index probe fix in HEAD, PR:146); but M-final M37 shows the 5 s sysroot probe limit still fails owner start under load
real data: N/A
sources: RS/REVIEW.md:144; S/SYM6-A.md:42; final/COORD.md:108

## SECTION 6: runtime / open path: review/open3/REVIEW.md (R-Open3 phase 1, 01:55-02:00) and MIGRATE.md (phase 2, 05:35). No REVIEW-DONE.md file exists (MIGRATE.md:95 cites one); p2/ holds logs only. Paths relative to W6/review/open3 (RO).

### ROpen-D1 · warm_anatomy runs a world computation nothing reads and redraws every window
surface: performance
problem: After W-Sym6 dropped its only consumer at 01:18 the anatomy pipeline still ran per visit and each landing called cx.refresh_windows().
evidence: RO/REVIEW.md:8-13,50-54 (store.rs:292; fixture_world.rs:921)
status: appears FIXED/removed: no warm_anatomy in apps/desktop/src on 2026-09-30 (grep); PR:267 still lists it (stale); the fixture_world tests asserting old-body words (RO/REVIEW.md:54,204-206) status unknown
real data: fixture world
sources: RO/REVIEW.md:8-13,50-54; PR:267

### ROpen-D2 · fast route change paints the skeleton of a route already gone (leaving page is the previous place, not the last painted)
surface: motion/transitions
problem: Three routes in one instant show B's "on its way" skeleton under C's title bar for 8 frames (t=16..128 ms); with 1 s reads a double click shows B's skeleton leaving and C's arriving.
evidence: RO/REVIEW.md:29,56-60 (frames/burst-t48-top.png; film-burst/input.txt); shell/reader.rs arrival.leaving, still_page
status: unverified: F-Shell-1 fixed an inert still page for transit (reader.rs:2248 mutation) but no file addresses the "previous place never loaded" case
real data: fixture (old page)
sources: RO/REVIEW.md:29,56-60,180

### ROpen-D3 · Back/Forward pass through a fully empty reader for 3 frames (48 ms)
surface: motion/transitions
problem: cmd-[ and cmd-] dip to nothing (ink 0.000% at t=880 and t=1680).
evidence: RO/REVIEW.md:31,62-63 (frames/rapid-back-blank.png)
status: unverified after F-Shell-1 Course fix and the parent-folded design (final/F-Shell-1.md:21); not re-filmed
real data: fixture
sources: RO/REVIEW.md:31,62-63,180

### ROpen-D4 · every hand touch re-runs the producer walk on the UI thread (291-724 ms dev, ~10 ms release)
surface: hand/holds
problem: Tables::hand compares Vec<Held> including touched_at/held_at, so Intent::TouchHeld changes it on each press; cmd-1..5 and card clicks drop a frame.
evidence: RO/REVIEW.md:39,65-69 (trace/hand.jsonl); fixture_world.rs:651; shell/hand.rs:49
status: still open per PR:268; MIGRATE hand_view_for migration not done (hand_view still called at status.rs:207, root.rs:1079,1115,1452, orbit.rs:205); HandKey fix never reported
real data: fixture world (the hand's arrangement runs on the prototype world; real relations do not exist)
sources: RO/REVIEW.md:39,65-69,125-127; RO/MIGRATE.md:11-24; PR:268

### ROpen-D5 · panic on the world thread is silent and permanent (is_loading() true forever, every later capture hangs)
surface: graph/world/hand
problem: serve() at fixture_world.rs:491 has no catch_unwind; requests.send fails and is ignored.
evidence: RO/REVIEW.md:71-74
status: still open per PR:269; not reproduced (reading only)
real data: fixture world
sources: RO/REVIEW.md:41,71-74; PR:269

### ROpen-D6 · panic on the owner thread leaves the window on "on its way" forever
surface: foot/status/lifecycle
problem: OwnerGate::wait blocks read workers and the actor on a Condvar with no timeout; OwnerThread::drop swallows the join error.
evidence: RO/REVIEW.md:76-77 (host/owner.rs:run; runtime/owner.rs:~125)
status: partly FIXED: host/owner.rs:92 now catches the starter's panic (PR:270); "verify that the gate fails the waiting reads" not done; the 15-minute owner reply timeout remains (PR:272)
real data: fixture; never provoked on the real owner
sources: RO/REVIEW.md:41,76-77,132; PR:270,272

### ROpen-D7 · harness quiet() does not know Reading/Loading states (uses::ask, fixture_releases) so captures can carry placeholders
surface: harness/journeys/lints
problem: determinism could flake on "Reading the lines your packages use it on...".
evidence: RO/REVIEW.md:79-80,184
status: superseded: uses::ask deleted by SYM6-B; runtime::offload::in_flight is read by harness quiet (MIGRATE.md:95)
real data: N/A
sources: RO/REVIEW.md:79-80; RO/MIGRATE.md:95

### ROpen-D8 · unbounded async caches (Tables.pages, readings, outbox, request channel)
surface: performance
problem: A long session keeps every visited declaration's Anatomy.
evidence: RO/REVIEW.md:82-83 (fixture_world.rs:532,535,453,450)
status: unresolved (Memo in runtime/offload.rs exists since phase 2; whether pages/readings were moved onto it is unknown)
real data: never measured on a long real session
sources: RO/REVIEW.md:82-83

### ROpen-N1 · cold-launch pop-in: frame 1 is the index-only body then one frame reflows the whole page (8.24% of pixels at +350 ms)
surface: performance
problem: Measured on pre-I3 product; the anatomy is not in the snapshot; same class of pop-in on the new symbol page's first frames.
evidence: RO/REVIEW.md:33 (frames/before1-anatomy-pop.png)
status: unverified for the new page (SYM6-B moved lines to the page, "no Reading state")
real data: fixture
sources: RO/REVIEW.md:33

### ROpen-N2 · first-frame and route-change budgets (150 ms, 16 ms, 10 ms) never judged; I3.md never landed at R-Open3's time
surface: performance
problem: All R-Open3 numbers are under load 60-180.
evidence: RO/REVIEW.md:14,42
status: see M-final M47 release budgets never judged
real data: never looked at in release
sources: RO/REVIEW.md:14,42

### ROpen-N3 · world thread panic and owner panics: Soak covered only 6 declarations, memory flat (RSS 190-290 MB after 976 MB peak)
surface: performance
problem: The soak cannot see the unbounded anatomy map with 6 declarations.
evidence: RO/REVIEW.md:43 (soak/soak.json)
status: not done at real scale (12 packages, 12,000+ declarations)
real data: never looked at
sources: RO/REVIEW.md:43

### ROpen-N4 · verify gate for value/from-str/package/orbit killed after 63 min with no verdict
surface: harness/journeys/lints
problem: The only verify replay found budget failures on a debug build (draw 113-335 ms vs 100 ms budget), not attributable.
evidence: RO/REVIEW.md:44
status: the full verify gate never produced a verdict this wave (PR:303)
real data: N/A
sources: RO/REVIEW.md:44; PR:303

### ROpen-N5 · pointer move on a reader link re-renders all five regions
surface: performance
problem: hover.jsonl shows shell x2, titlebar, shelf, reader, status re-rendering after each of 5 move acts.
evidence: RO/REVIEW.md:184
status: unresolved (belongs with W-Feel's hover checks; a render-count budget was never set)
real data: fixture
sources: RO/REVIEW.md:184

### ROpen-N6 · new page entrance: variant-name column slides up through "Your code and it" heading (old body); new body may not
surface: motion/transitions
problem: Text-over-text for ~5 frames in the first 90 ms of the old symbol body.
evidence: RO/REVIEW.md:36 (frames/warm-settle-crop.png)
status: unverified for the new body
real data: fixture
sources: RO/REVIEW.md:36,180

### ROpen-N7 · route transition motion-report FAIL: 70 findings (continuity plate.left jumped 166.8 px, overshoot 1176 px past rest 1440, lockstep spread 0.949, settle 11 failed)
surface: motion/transitions
problem: The route transition's own declaration failed the alignment check on the old page.
evidence: RO/REVIEW.md:37 (motion/route.log, route.json)
status: likely FIXED for J10 by F-Shell-1 Course (final/F-Shell-1.md:42) but the symbol-page route transition was not re-run
real data: fixture
sources: RO/REVIEW.md:37; final/F-Shell-1.md:42

### ROpen-C1 · code: five hand-written off-thread caches (fixture_world, fixture_releases, uses.rs, source_facts, reach) with different eviction, no panic containment, all wake via refresh_windows
surface: performance
problem: One runtime::offload::Memo<K,V> was proposed (bounded LRU, single-flight, catch_unwind, targeted notify).
evidence: RO/REVIEW.md:91-99 (A1)
status: Memo exists in runtime/offload.rs (MIGRATE.md:35); callers still hand_view/release_data with Asker::Everyone (MIGRATE groups 1-2 not done)
real data: N/A
sources: RO/REVIEW.md:91-99; RO/MIGRATE.md:11-35

### ROpen-C2 · code: lost wake in Tables::landed(); serve publishes the world only after arranging the hand (~10 ms); gate wait has no timeout; Failed then Ready flip; snapshot thread joined unbounded before first window (dataless iCloud file); join panic swallowed
surface: foot/status/lifecycle
problem: B4-B9: side-effecting landed(), late publish, unbounded waits, low-probability flips, a slow read of the snapshot file delays the first frame with no fallback.
evidence: RO/REVIEW.md:128-138
status: unresolved (no phase-2 report)
real data: never looked at (dataless files, second window)
sources: RO/REVIEW.md:128-138

### ROpen-C3 · code: typing (tuples Loaded/ToSave/Reading/Seed.pages; seed key-value mismatch hidden behind _ arms; stringly snapshot keys/hashes/cursor; three bools in Slot; failures as Arc<str>; NodeId cache key without world identity; Want::of conversions; pub too wide)
surface: typing
problem: C1-C8 of R-Open3; owner said typing is getting worse.
evidence: RO/REVIEW.md:140-160
status: partly done in phase 2: LoadedWorld, PoolLoad exist (MIGRATE.md:9,39,62); ProducerEpoch/Observation newtypes (MIGRATE 7) proposed, "say when you want it", never done
real data: N/A
sources: RO/REVIEW.md:140-160; RO/MIGRATE.md:37-91; PR:271

### ROpen-C4 · MIGRATE: legacy names still in use (hand_view x8 sites, release_data x4 sites, pool_load x3, WorldPair/PROCESS graph handle, install files parameter, from_revision bare u64s)
surface: typing
problem: 7 groups of cross-lane call-site changes (status.rs, root.rs, orbit.rs, shelf.rs, symbol.rs, history.rs, package/data.rs, graph.rs, harness.rs, tests) so the legacy names can be deleted; none executed. Asker::Everyone redraws every window.
evidence: RO/MIGRATE.md:11-91; grep 2026-09-30: hand_view( at shell/status.rs:207, root.rs:1079,1115,1452, orbit.rs:205; release_data( at bodies/symbol.rs:112,179, symbol/history.rs:18, side/mod.rs:242; pool_load at harness.rs:1428, folio-capture.rs:272, shell/tests.rs:421
status: still open
real data: N/A
sources: RO/MIGRATE.md:11-91; PR:271

### ROpen-T1 · desktop lib tests at ~02:00: 309 passed 24 failed; environment refusal "private state parent is not owned by this user"
surface: harness/journeys/lints
problem: 3 tests fail because the harness/test process runs under umask 022 (state dirs not owner-only); browse_tests the_library_page_shows_a_real_tree_read_by_a_real_owner named this; later fixed as "racing settle" (final/F-Shell-1.md:26).
evidence: RO/REVIEW.md:195-235
status: FIXED per F-Shell-1 (suite green at 18:43 except 2 in F-Data files) and F-Data-2; PR:277-282 nine failures at 08:00 - later fixed; 00:01 three real-owner tests failed under load (M-final M37, M48)
real data: N/A
sources: RO/REVIEW.md:195-235; final/F-Shell-1.md:5-19

## SECTION 7: sidebar: side/SIDE-A.md, SIDE-B.md, SIDE-C.md (W-Side, 2026-09-29). Paths relative to W6/side (SD). SIDE-B section 4 "Numbers" is an empty stub; the lane never wrote harness captures ("HARNESS captures are added below as they are taken": none are). Later real-data pass: final/F-Shell-1.md:29-37.

### SIDE-1 · sidebar never captured in the harness or on real data by W-Side; all lane evidence is RIG
surface: sidebar
problem: SIDE-A/B/C cite rig tests on pinned pages and an "empty-Library harness" only; the Numbers section was never filled; the peek, twins, narrowing, state glyphs, sticky ancestors and hoist were not seen in a harness film by the lane.
evidence: SD/SIDE-A.md:42; SD/SIDE-B.md:37-39
status: partly overtaken: F-Shell-1 shows narrowing 25 of 665, told-apart duplicates and toml_pin once on the real install (final/F-Shell-1.md:29-37); peek on the float layer, twins, lens chords (G C/V/R/U, G G), hoist, sticky ancestors, hold chips, trail: no real-data picture in any file (M-final M54)
real data: REAL for narrowing/duplicates/toml_pin; never looked at for peek, twins, hoist, lenses, sticky, hold chips, trail
sources: SD/SIDE-A.md:42; SD/SIDE-B.md:37-39; final/F-Shell-1.md:29-37; M-final M54

### SIDE-2 · sidebar state glyphs and release marks read only the prototype fixture (toml, smallvec)
surface: sidebar
problem: Per-item uses and changes exist only for facet::data::release Crate fixtures; every other package shows no glyph (an unread count is never drawn as zero); the "used by your crates" via chooser has crates only for those two packages.
evidence: SD/SIDE-A.md:14; SD/SIDE-B.md:13,25
status: still open (G14 real diffs; PR:90)
real data: FIXTURE
sources: SD/SIDE-A.md:14; SD/SIDE-B.md:13,25; PR:90

### SIDE-3 · sidebar Contents count semantics ("Contents 747", "88 vs 31 public names")
surface: sidebar
problem: SIDE-A redefined Contents (package intro: items the kind families hold; outline: every reachable name, imports excluded), but on the real install toml's intro shows Contents 88 while the page says "31 public names": two sources (index outline vs source facts).
evidence: SD/SIDE-A.md:99; final/F-Shell-1.md:37
status: not done (F-Shell-1.md:37); M-final M29
real data: REAL
sources: SD/SIDE-A.md:99; final/F-Shell-1.md:37; PR:166

### SIDE-4 · sidebar keys not in Settings > Keys; key-table row for reveal-on-demand (Shift-Cmd-J); chips beyond Cmd-5
surface: keys/focus
problem: SIDE-C section 6: Shift-Cmd-J "reveal on demand" has no key-table row; chips carry Cmd-1..5 only because the hand holds five; Settings > Keys lists sidebar keys only after F-Shell-1 added "In the sidebar" (typing, Backspace, Esc, arrows, Space, H, G C/V/R/U, G G).
evidence: SD/SIDE-C.md:27-29; final/F-Shell-1.md:36
status: Settings > Keys list FIXED by F-Shell-1 (typed side::KEYS + rig test); Shift-Cmd-J reveal binding and Cmd-6..9 not done (PR:169-173)
real data: N/A
sources: SD/SIDE-C.md:27-29; final/F-Shell-1.md:36; PR:169-173

### SIDE-5 · drawer instance of the sidebar (phone widths) has no keyboard
surface: keys/focus
problem: The shell zones only include the inline shelf, so on a phone the opened drawer cannot be walked, narrowed or peeked by keys.
evidence: SD/SIDE-C.md:30
status: still open (final/F-Shell-1.md:37; M-final M30)
real data: N/A (phone width behaviour never real)
sources: SD/SIDE-C.md:30; final/F-Shell-1.md:37

### SIDE-6 · twin hover: package page cards register targets with no source, so a sidebar row does not light its card
surface: package page
problem: W-Folio's PageTarget::Card targets do not carry source: Some(symbol); twins on the package page only work sidebar to page rows, not cards.
evidence: SD/SIDE-C.md:10
status: unverified: the tree now has PageTarget in package/target.rs (source unknown); lane did not report
real data: never looked at
sources: SD/SIDE-C.md:10

### SIDE-7 · narrowing widen row sends words to Find, not Ask, because Ask cannot be opened with words
surface: Ask/search
problem: "folds in the whole library" widens to Find(query); Ask::opened clears its field.
evidence: SD/SIDE-B.md:11
status: by design (workaround); Ask cannot be pre-filled
real data: REAL for narrowing only (Find widen never run)
sources: SD/SIDE-B.md:11

### SIDE-8 · sidebar chord G vs words starting with g/c/v/r/u: a query that begins gc, gv, gr, gu cannot be typed
surface: keys/focus
problem: A chord with no field trades a typing case; "get" narrows because G waits 700 ms.
evidence: SD/SIDE-B.md:14
status: known price (owner decision unrecorded)
real data: N/A
sources: SD/SIDE-B.md:14

### SIDE-9 · optional quiet "uses" group for imports not added; imports omitted from every list and count
surface: sidebar
problem: `use` imports are excluded from lists/search; the lead's optional "uses" group was skipped.
evidence: SD/SIDE-A.md:98
status: decision (omitted by design)
real data: REAL fix confirmed on toml (final/F-Shell-1.md:31)
sources: SD/SIDE-A.md:98

### SIDE-10 · sidebar row repeats: Datetime and Date many times, once per impl block, on the desktop-record-rust sidebar
surface: sidebar
problem: Rows for a type repeat per impl block on real data.
evidence: final/F-Data-2.md:104 (M-final M28)
status: still open
real data: REAL
sources: final/F-Data-2.md:104; M-final M28

### SIDE-11 · registry package name shown by folder (toml-0.8.23) not name; release_version drawn beside names; verified with one version per crate
surface: sidebar
problem: F-Data-2 made PackageRef::display_name/release_version show "toml 0.8.23", but two versions of one crate in one Library have never been on screen.
evidence: final/COORD.md:72,82; M-final M31
status: partially verified
real data: REAL (single version each)
sources: M-final M31

### SIDE-12 · sidebar told-apart names for two checkouts/copies (backend/... vs tree/...)
surface: sidebar
problem: side/listing.rs told_apart uses the nearest non-shared folder; an unusual layout (two copies of a tree) was seen only in an old harness build; a sibling-name collision on the real Library not looked at.
evidence: final/F-Shell-1.md:31
status: unit-tested; picture from a harness built before the fix
real data: partly
sources: final/F-Shell-1.md:31

### SIDE-13 · sidebar fluid width: SIDE ladder (232 design px) and mode swap constants; legibility law (298 findings) not re-run
surface: fluid/phone widths
problem: The lens strip drops to chord letters below 232 design px; the shelf swap point was changed by F-Shell-1; the R-Fit legibility law over the slow sweep was measured only on a pre-fix binary.
evidence: SD/SIDE-C.md:24; final/F-Shell-1.md:24
status: rig test only
real data: fixture
sources: SD/SIDE-C.md:24; review/fit/BASELINE.md:70,175

## SECTION 8: fluid: fluid/FLUID-A.md, ADOPT.md, FLUID-B.md (05:35), FLUID-C.md (08:05+). Paths relative to W6/fluid (FL). FLUID-C says "Still running" for the h9 binary re-run and a `real/pics/lib-200/` caption picture: no later file reports either.

### FLUID-1 · post-merge harness pictures never taken for the shell frame changes (COLUMNS_SHARE fast-shrink cap, drawer paint priority, Library ring per-reader flow, inbox flow removal, disabled chevron ink, Find name floor, Find inspector arriving after rows, shelf swap constants)
surface: fluid/phone widths
problem: FLUID-B measured on a pre-merge binary (harness-pre1); the items listed were covered by rig tests only; FLUID-C's post-merge binary (harness-h9) "queued" and its pictures ("re-run the pictures that matter") never reported.
evidence: FL/FLUID-B.md:8,49-50,63; FL/FLUID-C.md:7,33
status: still open (unverified in a harness film); rig tests pass
real data: fixture then FLUID-C real 13-package clone (pre-05:15 tree)
sources: FL/FLUID-B.md:8,49-51; FL/FLUID-C.md:7,33

### FLUID-2 · package page hard thresholds on real content: 16 thresholds, 94 of 285 widths with text past the edge (feature chips + repo URL from 416 px down), snap events worse 602 to 1458
surface: package page
problem: Reflow of the blocks under a changed crest/cards mode jumps by the changed block's height in one frame; the crest cells and cards glide but the page's own blocks are not Flow items; chips need flex_wrap and min_w(0) with ellipsis.
evidence: FL/FLUID-C.md:20,25,62-63 (real/analysis/package.txt; thresholds.py); top: 440->432 (202 elements, 307 px), 1288->1280 (200, 259 px), 488->496, 1328->1336
status: still open (PR:187,230; FLUID-C "Next" 1-2)
real data: REAL 13-package clone (pre-F-Data tree)
sources: FL/FLUID-C.md:20,25,34,62-63; PR:187,230

### FLUID-4 · in-flight Library chips cross each other after a fast step; chips fly in from outside the window (painted past the edge for up to 5 frames)
surface: motion/transitions
problem: A flight from outside the window could start at its edge (FlowItem::prepaint) and a long flight between wrapped rows could cross-fade; both change facet::motion::flow, which every flow uses, and were left for the lead.
evidence: FL/FLUID-C.md:32,65 (real/pics/lib-fast/)
status: still open; the ring's glide is long (108-239 frames, R-Fit BASELINE.md:174); M-final M17 chips overlap during the install growth
real data: REAL
sources: FL/FLUID-C.md:32,65; PR:188

### FLUID-5 · legacy Measure::room, Measure::columns, Room enum dead but kept (rule: never remove public facet API)
surface: typing
problem: measure.rs:111-117 is the last KNOWN entry of the breakpoint scan test; nothing calls it.
evidence: FL/FLUID-C.md:56,66; FL/ADOPT.md:42
status: waiting for the lead's call; delete (PR:261)
real data: N/A
sources: FL/FLUID-C.md:56,66; FL/ADOPT.md:42

### FLUID-6 · remaining hand-rolled widths in facet (KNOWN list): anatomy/page.rs:86, page/gallery.rs:433, anatomy/gallery.rs:196, prism.rs:23,267,272
surface: fluid/phone widths
problem: The retired v4 symbol page and gallery scenes still compare a width with a number; tokens exist (PAGE_SPINE, PAGE_GEM, SYMBOL_PRISM).
evidence: FL/ADOPT.md:25-27; FL/FLUID-C.md:52-55
status: still open (PR:259-260); production reads only anatomy::page::words_w
real data: N/A
sources: FL/ADOPT.md:25-27; FL/FLUID-C.md:52-55

### FLUID-7 · symbol page keeps the narrow margin placement after a live resize (old Spots memory, not a function of the room)
surface: symbol page
problem: W-Fit R3: a page after a resize storm should equal a fresh page except near an edge.
evidence: FL/ADOPT.md:30 (fit/resize/r1/symbol-band.png)
status: likely fixed by the new symbol page (SYM6-A on Modes); a storm-equals-fresh test exists for the package page only (F/FOLIO-B.md:32); none for the symbol page
real data: fixture
sources: FL/ADOPT.md:30

### FLUID-8 · hit targets under 24 px on the symbol page (page-badge 74x21, page-case-door 44-73 x 16-20) are "not a width matter; the reader does not raise them"
surface: contrast/a11y
problem: No lane owns minimum hit target raising; SYM6-B rig test asserts s6- stops >= 24 x 24.
evidence: FL/ADOPT.md:32
status: symbol page FIXED by SYM6-B (rig assertion); package page badges 21 px (RFolio-D14) still open
real data: fixture
sources: FL/ADOPT.md:32; S/SYM6-B.md:28

### FLUID-9 · Flow continuity false positives: harness continuity law counts each followed layout step as a jump
surface: harness/journeys/lints
problem: motion-report prints "continuity FAIL jumped 4.000 in 12 ms" for the Library ring during a scripted drag; not a visible jump; readers must not treat them as regressions; the harness cannot tell a followed step from a jump.
evidence: FL/ADOPT.md:45; FL/FLUID-B.md:64
status: known noise; no lint change reported
real data: N/A
sources: FL/ADOPT.md:45; FL/FLUID-B.md:64

### FLUID-10 · Find/Compare inspector at 760 mode change moves twice (was four times); inspector "arrives after its rows" not in measured binary
surface: Find/browse
problem: Find snap events 128 to 66 only.
evidence: FL/FLUID-B.md:60
status: partly open (FEEL-D6, RFit-7)
real data: fixture
sources: FL/FLUID-B.md:60

### FLUID-11 · Ask results plate: ASK ladder (sheet across window below 640, panel over the shelf column above, 320-440 px)
surface: Ask/search
problem: Verified by a rig test at 1440, 800, 360 only; long result names, many results, Ask on a phone with the real library (665 rows) not looked at; wording of the raw protocol error (M-final M24) unreviewed. MERGED: also wave-4 audit: Ask4 trailing hint never folds at narrow widths; typing into Ask panicked the harness (leaked DataStore handle) then; populated state matches Ask4 unverified (L/wave4/audit/LEDGER.md:145-195)
evidence: FL/FLUID-B.md:15,26
status: unverified on real data (J9 ran "toml Value" only)
real data: fixture rig; one real query
sources: FL/FLUID-B.md:15,26; M-final M23

### FLUID-12 · graph card carried between beside and sheet (Flow) and where-line wrap; Rose and Comb on Modes
surface: graph/world
problem: The 588 px focus-card jump is now a glide (rig test only); Rose form and version comb hold their modes (rig tests only).
evidence: FL/FLUID-C.md:38-44
status: rig-tested only; harness picture pending (h9)
real data: fixture world
sources: FL/FLUID-C.md:38-44

### FLUID-13 · segmented control column form at phone width (Theme/Density/Motion rows 562/517/427 px at 200% on 360; Theme at 125% on 320)
surface: settings
problem: seg.rs stands the choices in a column with a gliding plate when the row is wider than the room; rig-tested; the Settings page on a real phone width never captured.
evidence: FL/FLUID-C.md:41
status: rig only
real data: never looked at
sources: FL/FLUID-C.md:41

### FLUID-14 · Library count caption wrap at 200% on a phone; Library snap left: caption re-wraps at 528/536 px
surface: Library
problem: The caption line "N declarations from X of Y files" now wraps (min_w(0)); the rig fixture has no health line so the caption's own proof is a pending harness picture.
evidence: FL/FLUID-C.md:24,33 (real/pics/lib-200/ pending)
status: unverified in a picture
real data: never looked at at 200% on the real Library (the real caption text differs after F-Data)
sources: FL/FLUID-C.md:24,33

### FLUID-15 · window resize on the real 13-package Library measured at pre-F-Data state only; the 12-package toml_pin install (as shipped) never swept 320-2560
surface: fluid/phone widths
problem: FLUID-C used a cloned 13-package fixture index. DoD 8 (no crash/fault/empty reader at 320/390/760/1440/2560 in both themes on any page reachable from the Library) never demonstrated on the shipped install; J12 crawler not written (M-final M3, M53).
evidence: FL/FLUID-C.md:8
status: still open (FLUID-C "real-content sweeps" is FINISH's F-Shell item 5, listed left for milestone 2)
real data: REAL clone (13 packages, three refused roots served preserved), not the shipped install
sources: FL/FLUID-C.md:8; M-final M3, M53

### FLUID-16 · titlebar ladder BAR (Bare 0, Snug 560, Full 760); Ask input; view switch and whole trail from Full; inbox from Snug
surface: shell/frame
problem: Titlebar behaviour below 560 (no inbox, no trail) at 200% text, many-segment paths (deep module paths), and the jump bar on a phone: never captured with real deep addresses.
evidence: FL/FLUID-B.md:16
status: rig tests only
real data: never looked at
sources: FL/FLUID-B.md:16; review/fit/REVIEW.md:125

### FLUID-17 · drawer confirmation on the package page (HEADS-UP over rows) waits for a working index; never re-checked
surface: overlays/popovers
problem: Drawer deferred at priority 100; needs a package page.
evidence: FL/FLUID-B.md:50; review/fit/e2e.sh (drawer-pkg after)
status: unresolved (unrecorded result)
real data: never looked at
sources: FL/FLUID-B.md:50

### FLUID-18 · Library orbit chips 23 px at 85% text and 2560 sparse column
surface: fluid/phone widths
problem: See FEEL-D12, RFit-8.
evidence: FL/FLUID-B.md:17 (WIDE_FOLIO 1120 at 2560)
status: wide leaf for Library only; package/symbol at 2560 owner decision (PR:364)
real data: fixture
sources: FL/FLUID-B.md:17; PR:201

## SECTION 9: W-Fit lane report: fit/FIT.md (final 02:10). Paths relative to W6/fit (FT).

### FIT-1 · product launch umask: the harness and journeys set umask 0077 but the product does not; a shell (or Finder) with umask 022 makes the owner create 0755 subdirectories and refuse its own second launch
surface: onboarding/install
problem: durable.rs (crates/platform/durable.rs) refuses any directory that is group- or world-accessible; the owner creates its subdirectories (compiler/, forge/, registry*/, semantic-objects/, objects/) with the process umask; FIT says "The harness now sets umask 0077; the product does not"; the journey machine's state_dir() (apps/desktop/src/harness/install.rs:94) also calls private_umask(), so the J0/J1 "Finder launch" journeys cannot see the hazard. macOS default umask is 022.
evidence: FT/FIT.md:25,44; review/fit/REVIEW.md:55; apps/desktop/src/harness/install.rs:94; apps/desktop/src/harness.rs:622-631; host/mod.rs:22-35 (private_dir is #[cfg(test)] only)
status: unverified hypothesis: nobody launched the built Nudox.app twice from Finder/launchd with umask 022; F-Data-2's relaunch tests run in a process with the harness umask or test dirs (final/F-Data-2.md:22, m2c relaunch)
real data: never looked at
sources: FT/FIT.md:25,44; apps/desktop/src/harness/install.rs:94

### FIT-2 · R4 margin note "28 places - 1 crate" cut by the reader's left edge at 1280x800
surface: symbol page
problem: The old symbol page read "1 places - 1 crate".
evidence: FT/FIT.md:83 (sweep/before/sheets/symbol.png)
status: superseded by the SYM6 rewrite (margin marker no longer exists in the simple form); unverified
real data: fixture
sources: FT/FIT.md:83

### FIT-3 · R9 contrast of separators: the "." between a row's words is ink4 2.62-2.72:1 on the Library tree page and Compare; Compare rows measured 1.02:1 mid-fade at 800x600
surface: contrast/a11y
problem: browse/** separators (library-twice-row-N-dot-1) are ink4; the fit_tests::no_words_in_the_shell_are_set_in_ink4 guard scans src/shell only, not apps/facet/src/browse. MERGED: also W-Tissue: jump-bar separator was ink4 2.72:1, fixed to ink3 in titlebar.rs:367 (tissue/J1.md:187-221)
evidence: FT/FIT.md:97 (sweep/extras/lint/tree-*.txt)
status: still open (guard scope excludes facet; no fix reported)
real data: fixture
sources: FT/FIT.md:97; final/F-Shell-1.md:30

### FIT-4 · below 760 effective px the view switch (Graph, Page, Code) is not drawn: keys only, no pointer path
surface: shell/frame
problem: "gives way to the bar"; a design decision, not changed.
evidence: FT/FIT.md:103,163
status: owner decision open (a phone user cannot switch view by pointer)
real data: N/A
sources: FT/FIT.md:103,163

### FIT-5 · pins column (1900+ px, only when something is pinned) never captured; pinned peek not testable
surface: shell/frame
problem: "Not done: pins column (needs a pinned peek)".
evidence: FT/FIT.md:154,167
status: never verified
real data: never looked at
sources: FT/FIT.md:154,167

### FIT-6 · sweeps at 200% text for the extras (tree, compare, find-home, find-empty), Glacier theme and density never run by W-Fit
surface: contrast/a11y
problem: W-Fit swept 100% text, Abyss, comfortable density for extras.
evidence: FT/FIT.md:154
status: not done; partially covered by FEEL matrix (orbit, find only)
real data: fixture
sources: FT/FIT.md:154; review/feel/FEEL.md:25-27

### FIT-7 · Library tree page, Compare, find-home, find-empty (browse extras) are only checked in the rig/lint at six sizes; not reachable/verified through the real shell on real data
surface: Find/browse
problem: "Library tree page" and Compare were captured as scenes on fixtures; the shipped product has Compare only via Held cards (hand) and the Library tree through browsing; nothing reports either on the shipped install.
evidence: FT/FIT.md:97,114
status: unverified on real data
real data: fixture
sources: FT/FIT.md:97,114

### FIT-9 · window minimum 320x480 and the 'MINIMUM' applies via window_min_size; below 480 px pages overflow (R8); graph label/caption overlap at 320
surface: fluid/phone widths
problem: R8 symbol strip, package tiles, graph caption.
evidence: FT/FIT.md:91-95
status: graph collision resolved (FEEL-D11); package/symbol partly FIXED by FOLIO-B/SYM6/FLUID-B; package chips and URL still past the edge (FLUID-2)
real data: fixture
sources: FT/FIT.md:91-95

### FIT-10 · macOS traffic-light inset 78 px hard-coded; native window chrome never exercised on other platforms
surface: shell/frame
problem: titlebar.rs assumes a 78 px inset on macOS; GPUI Windows/Linux titlebar never run (PR:309).
evidence: FT/FIT.md:167
status: never verified off macOS
real data: never looked at
sources: FT/FIT.md:167; PR:309

### FIT-11 · first-run state dir refusal: .local/harness/desktop from another build gives "unsupported view DTO version"
surface: onboarding/install
problem: A view journal from an older wire is refused whole (later set aside by F-Data-2: embedded_host.rs:64 journal_refusal) but the person sees...?
evidence: FT/FIT.md:25; final/F-Data-2.md:26
status: FIXED as a set-aside (test); what the person sees is undescribed (M-final M56)
real data: fixture
sources: FT/FIT.md:25; M-final M56

## SECTION 10: glyph/GLYPH-A.md, GLYPH-B.md (W-Glyph, 2026-09-28, the OLD symbol page: sigils, reach bar, decks, hop). The SYM6 rewrite (2026-09-29 01:18) deleted the old symbol body, so most pieces are dead. Paths relative to W6/glyph (GL).

### GLYPH-1 · shared-element "hop" continuity (name and mark fly to the chip, ringed "from", arrived_from) built for the old page; status on the new simple symbol page unknown
surface: motion/transitions
problem: GLYPH-B built the hop on the old page's doors (named_as, Doors::mark, Door.from, Ctx.arrived_from); SYM6-A deleted doors.rs and the old body; whether the simple page keeps any hop/shared-title landing ("the symbol you leave lands on its relation") is unrecorded; hop_tests were stubbed to placeholders (RS/notes.md:41).
evidence: GL/GLYPH-B.md:5-16,56-70,87-88; review/sym6/notes.md:41
status: unresolved; likely lost in the rewrite; a wrinkle (same word twice at +10 ms) was never fixed
real data: gallery only (harness did not capture)
sources: GL/GLYPH-B.md:24-26,87; review/sym6/notes.md:41

### GLYPH-2 · Push transition (T5), hero plate frame, time travel (pin a release and redraw the page there), band held-in/takes rows, "what its file stands on", "also called X", trait "every one also gets"/"asked for by", relation rows travelling to their ports, camera pull-back, title becomes the node label: NOT BUILT
surface: motion/transitions
problem: The transitions catalog's push and the hop's landing side are decided by the page's grammar, not the plate; time travel on the symbol page is a board feature with no native build; the index does not serve several relation kinds.
evidence: GL/GLYPH-B.md:72-78
status: never built (page was rewritten simply); time travel appears again only as the package ticker (RFolio-D3) and the symbol page "Across releases" rail
real data: never looked at
sources: GL/GLYPH-B.md:72-78

### GLYPH-3 · no mutation proofs for the deck fan, scrub, sigil prongs, hop landing
surface: harness/journeys/lints
problem: "Mutations: none were run" (wrap-up instruction came first).
evidence: GL/GLYPH-B.md:54
status: moot for deleted code
real data: N/A
sources: GL/GLYPH-B.md:54

### GLYPH-4 · reach/"Your code and it" depends on the fixture world's caller edges (reach_world::reach_of); lines mined from the prototype world; index-only fallback gives counts and no lines
surface: symbol page
problem: The reach bar, decks and "who uses it" needed a real relation source; the new page reads UseLine from the page (workspace), but the fixture world still feeds the hand's arrangement and the graph.
evidence: GL/GLYPH-A.md:29-38; GL/GLYPH-B.md:92
status: symbol page moved to index use sites + line reading (SYM6-B); the graph and hand still read the fixture world (PR:91)
real data: FIXTURE (prototype world)
sources: GL/GLYPH-A.md:29-38; PR:91

### GLYPH-5 · index gaps requested: ReferenceSite excerpt text, sibling signatures, world rails carry no link, Anatomy should carry a Reach
surface: data-feed
problem: The index keeps a span, not text (the desktop now reads the line from disk); siblings lack signatures so chips say kind words only ("from bytes" is board-hand-written); world rails cannot be doors.
evidence: GL/GLYPH-A.md:58-65
status: line text solved for local files by workspace_lines; sibling signatures still absent ("Next to it" shows name tails, RSym-D10)
real data: REAL for local files
sources: GL/GLYPH-A.md:58-65; review/sym6/REVIEW.md:75

### GLYPH-8 · jump bar shows the current declaration's path, not the trail
surface: titlebar/jump bar
problem: "the jump bar shows the current declaration's path, not the trail; a trail crumb across packages is yours."
evidence: GL/GLYPH-A.md:63
status: unresolved (lead's jump.rs)
real data: never looked at
sources: GL/GLYPH-A.md:63

### GLYPH-9 · time-travel history instrument reads only toml and smallvec from fixture_releases; other symbols draw none
surface: releases/time travel/upgrade
problem: "Unread releases carry no cap and the page says 'only your pin is on disk: other releases not read'"; clicking a bar to pin a release not built.
evidence: GL/GLYPH-B.md:30
status: same as G14; the new page's "Across releases" rail reads the same fixture
real data: FIXTURE
sources: GL/GLYPH-B.md:30; W6/journey/GAPS.md:129-133

## SECTION 11: acquire/ACQUIRE-A.md (W-Acquire, 2026-09-29 06:00; the "## Files" section is unfilled, no ACQUIRE-B was written) and install/INSTALL-A.md (W-Install, 2026-09-29 07:xx-08:2x; only checkpoint A exists; "B will report those with their proofs" and C never appeared). Paths relative to W6/acquire (AQ) and W6/install (IN).

### ACQ-1 · ACQUIRE-B never written: releases on demand designed (runtime/releases.rs replaces fixture_releases; route_package fall back to source root_of; choosing an unindexed release adds it; lens diff = SurfaceCommand::Diff pin root vs viewed root with signatures filled in)
surface: releases/time travel/upgrade
problem: The design exists; an untracked runtime/releases.rs (98 lines) is in the working tree, unreported; the DiffRecord has no signature text so facet's respelled cannot tell a spelling move from a change.
evidence: AQ/ACQUIRE-A.md:45; crates/library/surface.rs:888; final/COORD.md:14
status: in flight, unverified (M-final M7-M9)
real data: never looked at
sources: AQ/ACQUIRE-A.md:16,45; M-final M7, M8, M9

### ACQ-2 · network Download source not composed: "the offer says needs a download and its button is disabled"
surface: Find/browse
problem: CargoCache is the only source; a purl index from the desktop downloads (production fetch) and no agent was allowed; how the app shows "available offline" versus "would download" is undecided.
evidence: AQ/ACQUIRE-A.md:9,38-39
status: owner decision (network policy, PR:119,361)
real data: never looked at
sources: AQ/ACQUIRE-A.md:9,38-39; PR:119

### ACQ-3 · archive-only release (registry/cache/*.crate) unpack path: sha256 vs registry index cksum, confined tar extraction, generated one-member Cargo workspace
surface: onboarding/install
problem: Verified by real-owner tests for the toml_pin 12 packages (F-Data-1); other archive shapes (yanked, +metadata versions like 1.1.6+spec-1.1.0, md-5) covered only by from_stem tests.
evidence: AQ/ACQUIRE-A.md:34,37
status: partly verified (final/COORD.md:11)
real data: REAL for toml_pin
sources: AQ/ACQUIRE-A.md:34,37; final/COORD.md:11

### ACQ-4 · Add-from-browsing states: offer, seam progress, added/failed, "already indexed registry tree never offered twice", top of a package page for a not-indexed dependency
surface: Find/browse
problem: The whole component (facet browse/acquire.rs add_control, shell/acquire.rs) has no real-data picture; the Add button on a dependency's page (an unindexed dependency) and its seam were never seen.
evidence: AQ/ACQUIRE-A.md:43
status: unverified (M-final M10)
real data: never looked at
sources: AQ/ACQUIRE-A.md:43; M-final M10

### INS-2 · no small real failing fixture: the real owner refuses almost nothing small (syntax error, unresolvable dep, generics-heavy crate all index Ready); the failed-state capture replays serde_core's recorded refusal for any project (NUDOX_INSTALL_REFUSE)
surface: onboarding/install
problem: J13 (failure and retry) cannot be a real journey; the failure card was never seen with a genuinely refused project name.
evidence: IN/INSTALL-A.md:62-63,87
status: still open (PR:291); after F-Data-2 two thin packages (serde_core, hashbrown) show "compiler could not finish 2" inside a Ready project, which is a different card
real data: fixture (replayed refusal)
sources: IN/INSTALL-A.md:56,62-63,87

### INS-3 · per-project Library scoping impossible: the owner serves one flat package list (runtime/client.rs::request_root)
surface: Library
problem: With two projects the Library cannot show "the packages this project uses"; F-Shell made the ring list "what your projects use" as a union; nothing in the owner's list ties a package to a project except F-Data's ProjectTree read.
evidence: IN/INSTALL-A.md:84
status: still open in the owner; the GUI computes a project-to-packages relation from cargo metadata (COORD 17:28) - multi-project real run never done
real data: never looked at with two projects
sources: IN/INSTALL-A.md:84; final/COORD.md:11

### INS-4 · Reveal and Remove from the SHELF's rows (W-Side's file); note for a damaged session and a set-aside state need a real relaunch capture; window resize real capture
surface: onboarding/install
problem: The failure card has Reveal/Remove but the sidebar row for a failed project offers none; notes ("session damaged", "index set aside": host/aside.rs, lifecycle notes) are unit-tested only.
evidence: IN/INSTALL-A.md:84
status: still open for shelf rows; notes verified in unit tests (host/lifecycle_tests.rs 4 tests); F-Data-2 covered persistence
real data: never looked at
sources: IN/INSTALL-A.md:43,84; final/F-Data-2.md:26

### INS-5 · INSTALL C never ran: wide net on indexing/failed/notes at every width and both themes, hover on the shingle and buttons, popover latency of the door tips (tip rest 350 ms, not QUICK 120), keys sweep
surface: onboarding/install
problem: Only checkpoint A exists; the dialog was matrix-linted only on the Ready Library ("--input was ignored by matrix").
evidence: IN/INSTALL-A.md:57,85
status: not done
real data: dialog 320-2560 strip in one boot only
sources: IN/INSTALL-A.md:56-57,85

### INS-7 · Add dialog: completion list keys and arrows not checked for the "shell walk swallows arrows" class (PR:177)
surface: keys/focus
problem: The dialog handles arrows itself; rig tests cover ↑↓/Tab; the same trap that hid menu keys (SYM6-B) could affect the dialog's completion list under the shell's j/k bindings.
evidence: IN/INSTALL-A.md:36; PR:177
status: unverified (rig test "Tab completes" only)
real data: fixture folders
sources: IN/INSTALL-A.md:36; PR:177

### INS-8 · first-run copy promises "compiles it and every package it uses ... ready to browse" and "a first index takes a few minutes"
surface: onboarding/install
problem: Real installs took 10-17 minutes on a loaded machine; copy is in the Library (library.rs); the promise "Pages open when it finishes" was made untrue by F-Data-2 (pages open earlier) and never re-edited. MERGED: also INDEX.md: first clean install 1215 s (11 packages) and second run 969 s (13 packages) at load 38-62 (index/INDEX.md:132-135); F-Data-1/2 measured 826-1032 s wall (M-final M35)
evidence: IN/INSTALL-A.md:32,41; final/COORD.md:25; M-final M35
status: still open (words not revisited)
real data: REAL timing
sources: IN/INSTALL-A.md:32,41; M-final M35

### INS-9 · privacy claim "Your source stays on this machine" on first run
surface: onboarding/install
problem: A promise in copy; no audit that no path (Ask, advisories, network source, crash reporting, update) sends source; network policy undecided.
evidence: IN/INSTALL-A.md:32
status: unverified claim
real data: N/A
sources: IN/INSTALL-A.md:32

## SECTION 12: index/INDEX.md (W-Index, 2026-09-29 06:xx). Data-plane root causes are compiler/owner internals; only items that block or shape a GUI surface are kept. Paths relative to W6/index (IX).

### IDX-1 · a refused package is invisible until the next boot (11 packages on first run, 13 on the second: zod and pflag appear only then)
surface: Library
problem: CommandAdapter::add returned the compile refusal before publish_view so a refused root stayed out of the view; the Library and failure UI read that list.
evidence: IX/INDEX.md:149
status: FIXED in code (adapter.rs:813-819 now publishes the view before returning the refusal, comment says so); the GUI effect (a refused package listed with its refusal named on the same boot) never seen in a picture (M-final M55)
real data: UNVERIFIED
sources: IX/INDEX.md:149; PR:85-87; M-final M55

### IDX-2 · desktop-record-rust (and -ts, -go) scenes panic at boot on real semantic data (no exact candidate: semantic decls have path=None line=None), stopping capture --scene all after 16 scenes
surface: harness/journeys/lints
problem: The scene resolver requires decl.path == Some("src/datetime.rs") which only structural rows carried; more scenes for TS/Go/Python/Clang/Java/C# may panic the same way.
evidence: IX/INDEX.md:150 (debug-record-rust/run.log; harness.rs outline_symbol_candidates)
status: FIXED by F-Data-1 (rows carry file; capture --scene all now runs 25 scenes, final/F-Data-2.md:27) but only desktop-record-rust and desktop-value-as-str were read (M-final M49); -ts and -go scenes: pflag and zod are still refused so their scenes cannot be real
real data: REAL for Rust; TS/Go pages never drawn from real semantic data
sources: IX/INDEX.md:150; final/F-Data-2.md:27; M-final M49

### IDX-3 · non-Rust languages: zod (TypeScript) and pflag (Go) remain refused (Authority Open/Binding and Resolve/Authority); Python, Java, C#, Clang never indexed through the desktop
surface: onboarding/install
problem: The GUI is only exercised on Rust packages; every non-Rust language page (symbol page derive for Python/TS/Go, badge readers in marks/badges/other.rs) rests on gallery facts or rig-pinned pages.
evidence: IX/INDEX.md:47,143-144; PR:59,139-142
status: still open
real data: never looked at (non-Rust packages on the real owner)
sources: IX/INDEX.md:47,143-144; PR:59,139-143

### IDX-4 · state from another build is set aside (workspace moved to <workspace>/from-another-build) and the owner starts fresh; the GUI note for a set-aside index needs a real capture
surface: onboarding/install
problem: What a person sees after an update that changes the index layout: the window re-indexes from scratch (10-17 minutes) with a note; the old state (79 MB in the test) is kept but nothing offers to delete or restore it; a real capture with an earlier build's state dir never taken.
evidence: IX/INDEX.md:61-68; IN/INSTALL-A.md:84
status: unverified (unit tests: host/embedded_owner_tests.rs, lifecycle_tests)
real data: never looked at
sources: IX/INDEX.md:61-68; IN/INSTALL-A.md:84

### IDX-5 · update policy: every build that changes a key layout forces a full re-index; no in-place migration
surface: releases/time travel/upgrade
problem: Product consequence of set-aside: an app update can silently discard the library and start a quarter-hour re-index; there is no update mechanism at all (PR:99) and no migration story.
evidence: IX/INDEX.md:37,66
status: owner decision (unrecorded)
real data: N/A
sources: IX/INDEX.md:37,66; PR:94-105

### IDX-6 · tokio-1.53.1: intent queue byte bound refuses the source frontier (Queue(Bytes)), not a compile refusal
surface: data-feed
problem: A large real crate cannot be indexed at all; "Each one is a package a person can't read" (PR:144).
evidence: IX/INDEX.md:145,151
status: not investigated (M-final M26)
real data: REAL
sources: IX/INDEX.md:145,151; M-final M26

### IDX-7 · dev shell exports only NUDOX_RUSTC; NUDOX_CARGO/NUDOX_CARGO_HOME/NUDOX_GO_ROOT; lane clang adapter dead
surface: harness/journeys/lints
problem: Other embedders (CLI-embedded owner, MCP) must supply their own compiler_environment; .local/devenv/clang-sysroot-adapter can be deleted.
evidence: IX/INDEX.md:154-155
status: open cleanup (PR:146-147)
real data: N/A
sources: IX/INDEX.md:154-155; PR:146-147

### IDX-9 · the only fixtures with dependencies are the repo's own crates plus registry sources admitted as roots; the desktop's real "project" concept (a folder with a Cargo.lock) was proven with toml_pin only
surface: onboarding/install
problem: Every real journey uses one project (toml_pin 25 declarations + 12 packages); a workspace with many members, path dependencies, git dependencies, build scripts and proc macros, a Cargo workspace root, a non-Cargo project, or a monorepo with several languages has never been added through the dialog.
evidence: IX/INDEX.md:132; final/COORD.md:11
status: never verified
real data: never looked at
sources: final/COORD.md:11; IN/INSTALL-A.md:61-62

## SECTION 13: instant open: instant/PLAN.md, LEDGER.md (2026-09-28, f9359 dataset), I1.md (window first), I2.md (snapshot), I3.md (world on demand; W-Open3, 2026-09-29 05:13). Paths relative to W6/instant (IT). All budgets are release-build figures on a loaded machine except where said.

### INST-1 · product relaunch refuses its own 0755 directories (exit 70 after ~23 s at HEAD 25fe3d18a); the fix ("create 0700, migrate or rebuild instead of refusing") was requested from the owner/index lane and never reported
surface: onboarding/install
problem: The owner creates compiler/, forge/, registry*/, semantic-objects/ with the process umask (0755); durable.rs:485-501 refuses them on the next start; I1 turned the failure into a window with a notice (2.3-3.0 s) but the underlying refusal remains for any user with umask 022 unless the owner or host chmods.
evidence: IT/LEDGER.md:265-283; IT/PLAN.md:320-327; IT/I1.md:168-174,307-310; IT/I3.md:347-348
status: unverified/open: harness and journeys set umask 0077 (apps/desktop/src/harness.rs:622, harness/install.rs:94) and host::private_dir (host/mod.rs:23) is #[cfg(test)] only; no file says a real Finder-launched product relaunch was run with umask 022 (see FIT-1)
real data: the failure was REAL on the product binary at 25fe3d18a; current status never measured
sources: IT/LEDGER.md:265-283; IT/I1.md:168-174,307; FT/FIT.md:44

### INST-2 · first-exec of the 89 MB binary: 678-810 ms before main() after reboot / new file, 4 491 ms once at load 5.7 (unexplained); 150 ms budget cannot hold for the first launch after a reboot or an update
surface: performance
problem: Binary size and packaging are the levers; the row is "re-measured on a signed and notarized bundle before anyone optimises it"; there is no signed bundle (PR:95-105).
evidence: IT/LEDGER.md:144-150,297-298; IT/PLAN.md:8-16
status: still open (PR:95-105 signing/notarization missing)
real data: measured on the dev release binary only
sources: IT/LEDGER.md:144-150,297; IT/PLAN.md:8-16

### INST-3 · runtime Metal shader compile on every launch (gpui runtime_shaders): 140 ms main-thread stall when the Metal cache misses; durable fix = build shaders at build time (drop runtime_shaders; needs xcrun metal)
surface: performance
problem: apps/desktop/Cargo.toml:66 still enables runtime_shaders; window creation outliers of 160-306 ms in 5 of 17 runs.
evidence: IT/I1.md:217-267 (runs/xctrace/xc-sysloop4); IT/LEDGER.md:156-160
status: still open (Cargo.toml:66 unchanged on 2026-09-30)
real data: REAL (Instruments trace)
sources: IT/I1.md:217-267; IT/LEDGER.md:156-160

### INST-4 · first meaningful frame < 150 ms achieved only with a launch snapshot and only at "quiet"; not reliable at load > 20; snapshot exists only for pages visited before; first-ever launch and first launch after an update paint nothing
surface: performance
problem: I2 measured 134-136 ms (relaunch of the real smallvec workspace, load 8-15); the writer run, a new build (writer identity is the exe length/mtime so every rebuild/update invalidates trust), and a fresh install still wait for the owner (2.7 s / 1.7 s in the writer runs); the real toml_pin install relaunch has not been measured.
evidence: IT/I2.md:7-16,217-235; IT/I1.md:205-206
status: partially verified on smallvec; never on the shipped toml_pin install or a Finder launch
real data: REAL smallvec workspace (not the 12-package install)
sources: IT/I2.md:7-16,217-235

### INST-5 · first frame is text-bound (cold glyph rasterisation 35%, shaping 22%, layout 26%): a plan cache saves 0.3 ms; a persisted glyph atlas / prewarm was proposed ("option b") and awaits a ruling
surface: performance
problem: The 18-19 ms first frame of the symbol page from snapshot cannot reach the "<= 10 ms first paint" budget without gpui text-system work.
evidence: IT/I2.md:303-360
status: unresolved (lead ruling requested; the PlanKey cache was NOT built; the new symbol page has less text)
real data: REAL
sources: IT/I2.md:303-360

### INST-6 · first text focus (Cmd-K into Ask) may still stall ~105-140 ms once per process (TextInputUI dlopen); prewarm proposed with I5 watchdog; risk: function-modifier key equivalents dispatched twice when no input context
surface: keys/focus
problem: IME deferral moved the cost to first text focus; not measured (needs accessibility permission); the double keyDown risk reasoned but not measured.
evidence: IT/I2.md:279-301
status: unverified (vendor/gpui_ce_macos patch in HEAD, commit 22de2eda1)
real data: never looked at (Ask first-focus latency; arrow-key double dispatch)
sources: IT/I2.md:279-301; IT/I1.md:269-285

### INST-7 · the snapshot keeps only the restored route's pages, the dossier and Orbit; back/forward neighbours, the hand's cards, the graph camera and plans are not snapshotted (I3's prefetch never landed)
surface: performance
problem: A relaunch on a graph route or after Back paints nothing from the snapshot; prefetch on keyboard focus, back/forward, hand cards and pointer rest never built; a shelf-row click still waits ~1 s for a symbol page read (954/999 ms) and a package to symbol change 995/1027 ms.
evidence: IT/I2.md:387-391; IT/LEDGER.md:74-79,221-223; IT/PLAN.md:249-262
status: still open (I3 rescoped to world/Memo; "one owner round trip per outline" and single-flight OutlineCache not reported)
real data: REAL numbers on f9359 fixture
sources: IT/LEDGER.md:74-79; IT/PLAN.md:228-262; IT/I3.md:0

### INST-8 · owner start 5.8-9.0 s (arrangement rebuilt every start, 72%; view.journal 52 MB load 25%); every readiness reply rebuilds tree-sitter queries (21-46% of request time); relations recomputed per request (~0.5 s each); first search builds Tantivy (1.1-2.0 s)
surface: performance
problem: Owner-side hot spots filed as requests (PLAN §7); GUI sees a symbol page read of ~1 s, search 1.1-2.0 s ("a few seconds in a debug build" in M-final M21).
evidence: IT/LEDGER.md:120-134,164-206; IT/PLAN.md:307-327
status: still open (no index-lane report); search freshly measured at "a few seconds" (M-final M21)
real data: REAL
sources: IT/LEDGER.md:120-206; M-final M21

### INST-9 · graph open 393-432 ms (budget <= 100 ms): layout_of + Discovery::prepare on first open; Recipes::new built twice; I4 (world as columns) moved to the queued W-World lane, never built
surface: graph/world/hand
problem: The graph still loads the 16 MB fixture world JSON (parse 83-150 ms, identities 61-96, tables 96-168) and lays it out on demand; WorldColumns/zerocopy never built.
evidence: IT/LEDGER.md:227-239; IT/PLAN.md:264-281; IT/I3.md:333-341
status: still open (PR:91; W-World brief at W6/world/BRIEF.md)
real data: FIXTURE world
sources: IT/LEDGER.md:227-239; IT/PLAN.md:264-281

### INST-10 · watchdog (I5/I6): report-mode task watchdog, grep guard against UI-thread I/O, keystroke p99 pin: never built
surface: performance
problem: Main-thread tasks > 4 ms: frames > 4 ms per interaction (16 on shelf hover/click, 12-13 symbol to source); gpui profiler hooks exist (vendor/gpui-ce/src/profiler.rs).
evidence: IT/PLAN.md:282-305; IT/LEDGER.md:87,253-259
status: not built
real data: never looked at
sources: IT/PLAN.md:282-305; IT/LEDGER.md:87,253-259

### INST-11 · hover-intent prefetch cannot beat a 1 s read; hover to peek starts after WARM 300 ms (model.rs:40) while DIRECTION says 350; index-key peek shows a name-only card until the page read lands (~1 s for a symbol)
surface: overlays/popovers
problem: Peek on a symbol row is slow on real data; FEEL F3 changed rests (Tip 350, Peek/Lens unchanged 350).
evidence: IT/LEDGER.md:221-223,247-251
status: unresolved for index-key peeks (sidebar peek uses release data or page lines)
real data: REAL numbers on f9359
sources: IT/LEDGER.md:221-223,247-251

### INST-12 · Ask / Filter results take 1.1-2.0 s after the last keystroke; UI to keep last answers stale under the new query with no spinner (proposal) not built
surface: Ask/search
problem: The search wait is the owner's; no incremental results.
evidence: IT/LEDGER.md:85,241-245; IT/PLAN.md:300-306
status: still open (M-final M21: a few seconds in debug)
real data: REAL
sources: IT/LEDGER.md:85,241-245; IT/PLAN.md:300-306

### INST-13 · snapshot format is JSON+SHA-256, not postcard+blake3; symbol decode 3.1-3.2 ms (budget <= 3 ms)
surface: performance
problem: 0.1-0.2 ms over the plan budget; 569 KB for SmallVec; postcard needs a Cargo.toml edit; snapshot cap 8 MB/LRU not implemented as specified; window-visible only for local roots.
evidence: IT/I2.md:36-47,237-248,392-393
status: open, low
real data: REAL smallvec
sources: IT/I2.md:36-47,237-248,392-393

### INST-14 · dev link blocker: desktop lib test binary exceeds ARM64 branch range (+/-128 MB) whenever code grows; fixed by opt-level entries in Cargo.toml (deb0c2298) and needs extending again as code grows
surface: harness/journeys/lints
problem: Recurrent (I1 §6, I2 §2, FIT.md:152); the durable answer (split the owner out of the desktop link, smaller binary) is undecided. MERGED: also W-Tissue LINK.md: desktop lib test binary __TEXT,__text grew 135 to 166 MB in a day; no alternative linker (lld not realised, ld-prime fails identically); fix = opt-level 2 on ra_ap_* and backend-local-service (Cargo.toml:116-141), buys ~18 MB (tissue/LINK.md:15-200)
evidence: IT/I1.md:76-79,301-306; IT/I2.md:183-189; FT/FIT.md:152
status: worked around; recurrence risk
real data: N/A
sources: IT/I1.md:301-306; IT/I2.md:183-189

### INST-15 · typed owner refusals (FaultCode::OwnerRefused{reason}) not built: the owner's refusal words reach the GUI as prose; the notice takes 2.3-3.0 s because the lease waits 2 s for a contending owner even for refusals unrelated to the lock
surface: foot/status/lifecycle
problem: A refusal not about the lock waits ATTACH_ATTEMPTS 40 x 50 ms; "Try again" on a snapshot notice (status foot) was added later (R7).
evidence: IT/I1.md:208-211,315-316; IT/I2.md:363-372
status: partly open; F-Data-2 typed StateFromAnotherBuild only for a state layout; OwnerFault typed on the gate (I3)
real data: REAL measured
sources: IT/I1.md:208-211; IT/I2.md:363-372

### INST-16 · I3: the claim that starting the world thread in prepare brings the graph earlier on a quiet machine was not demonstrated
surface: graph/world/hand
problem: On the loaded machine identities took 211-279 ms (starved); "re-run tools/i3_runs.sh on a quiet machine" requested.
evidence: IT/I3.md:50-54,351-355
status: unverified
real data: REAL but load 21-190
sources: IT/I3.md:50-54,351-355

### INST-17 · I3 verify --matrix gate never finished (50 min at load 90-190, timeout); one storm failure (desktop-value seed 1) not replayed; no verify verdict from the lane
surface: harness/journeys/lints
problem: Same story as PR:303: the gate never gave a verdict this wave.
evidence: IT/I3.md:325-329
status: still open
real data: N/A
sources: IT/I3.md:325-329; PR:303

### INST-18 · I3 cold-visit film: 1 STALL at t=160 inside the package body's own entrance easing (W-Folio's motion)
surface: motion/transitions
problem: One identical frame inside the easing before any route act.
evidence: IT/I3.md:311-315 (film/package-toml-strips/STALL-010.png)
status: not investigated
real data: fixture
sources: IT/I3.md:311-315

### INST-19 · perf: package page first frame 13.9 ms, p95 27.5 ms, max 36.3 ms (debug); value page p95 7.1 ms; release numbers "NOT BUDGETED"
surface: performance
problem: The package page is the slowest to draw (crest + shingles) - debug figures only; no release perf on any page; harness perf marks debug "NOT BUDGETED".
evidence: IT/I3.md:317-324
status: unresolved (M-final M47)
real data: fixture, debug
sources: IT/I3.md:317-324; M-final M47

### INST-20 · Memo migration: uses.rs done (page carries lines); source_facts Service (W-Folio, unbounded, Slot.project Option<Option>) not moved to Memo; shell callers still on hand_view/release_data (Asker::Everyone redraws every window)
surface: performance
problem: Five off-thread caches reduced to at most three; source_facts still unbounded.
evidence: IT/I3.md:269-293
status: still open (see ROpen-C4)
real data: N/A
sources: IT/I3.md:269-293

### INST-21 · frame_tests an_idle_shell_with_a_running_index_requests_no_frames failed 3 times with leaked DataStore handles after fixture_releases moved onto Memo, then passed six times unchanged (load 100+)
surface: performance
problem: A possible refresh_windows() landing during test-app teardown; cure would be Asker::View in the shelf; unresolved intermittent.
evidence: IT/I3.md:359-364
status: unresolved (intermittent under load); M-final M48 suite not certified green
real data: N/A
sources: IT/I3.md:359-364

## SECTION 14: tissue/J1.md (W-Tissue J1 fix report, 2026-09-28/29), LINK.md, VERSIONS.md, TIMING.md. J1 there is the OLD walk journey (apps/desktop/journeys/J1.journey: Orbit, your project, stop-1, code, back-1..3), not the later real-install J1. Paths relative to W6/tissue (TS).

### TIS-1 · Back-restores-focus only proven for the Library project tile (back-3); back-1 ("Start here" anchor on the package page) and back-2 (the "toml" row under "Depends on") were never made focus anchors
surface: keys/focus
problem: Back re-focuses the row that led away only where a body calls targets.focus + remember_leave (orbit.rs project tile); bodies/package.rs dependency rows are not published targets (dep_link not tracked); "Start here" section does not exist ("no fixture-ranked tour").
evidence: TS/J1.md:103-119,284-362
status: unverified: the package page was rewritten (folio) and PageTarget/Recall exist (folio.rs:109,300-325, RFolio-D7) - Back restores module+card focus per R-Folio (RF/REVIEW.md:72 "Cmd-[ after a card click: works and restores the module and the card focus"); dependency rows and Library shelf rows: unknown
real data: fixture
sources: TS/J1.md:103-119; review/folio/REVIEW.md:72

### TIS-2 · shelf.rs project row (Library sidebar) activates but never navigates (same gap the orbit tile had)
surface: sidebar
problem: Clicking a project row in the sidebar Library only calls ActivateProject.
evidence: TS/J1.md:277-282 (shell/shelf.rs:311-324 at the time; now side/*)
status: unverified after the sidebar rewrite (SIDE lens/rows Do::Go)
real data: never looked at
sources: TS/J1.md:277-282

### TIS-3 · the jump-bar current-name 0 px collapse was fixed by flex_1 + text_ellipsis but no headless test reproduces it (test kept as guard only; proof = the real J1 run, which never happened post-fix)
surface: titlebar/jump bar
problem: bar gained flex_1 (titlebar.rs:230) and the name text_ellipsis (:327); a trap mutation of both reverted still passes the guard test; the honest proof was the post-fix J1 run, which the lane could not obtain (toolchain gate).
evidence: TS/J1.md:223-265,284-310
status: unverified visually on the real bar (three-segment breadcrumb toml-0.8.23::src/de.rs::from_str) - the later journeys use one-segment paths; the deep-address jump bar on real symbol pages is unchecked (FLUID-16)
real data: never looked at
sources: TS/J1.md:223-265

### TIS-5 · shelf-row double probe registration lint fixed; the same pattern (Said + outer probe::text) may exist elsewhere
surface: harness/journeys/lints
problem: kit::text() already registers a probe text; an outer wrapper duplicated it and the overlap lint fired for 7 of 8 checkpoints; other places that wrap a Said in probe::text unaudited.
evidence: TS/J1.md:144-185
status: shelf FIXED (side rewrite later re-did the row); audit not done
real data: N/A
sources: TS/J1.md:144-185

### TIS-6 · harness orbit workspace injection: prepare_with_progress admits toml_pin as a project for Target::Orbit only in fixture scenes
surface: harness/journeys/lints
problem: The old J1 needed the Library to show a project; the real production machine (later journeys) does not use this; scenes still differ from an install.
evidence: TS/J1.md:17-56
status: known; JRN-3
real data: FIXTURE
sources: TS/J1.md:17-56

### TIS-7 · review_diag eprintln stays (silent unless NUDOX_REVIEW_DIAGNOSTICS) in harness.rs
surface: harness/journeys/lints
problem: Debug print left in the harness on purpose.
evidence: TS/J1.md:267-275
status: accepted
real data: N/A
sources: TS/J1.md:267-275

### TIS-9 · versions-query poison: one indexed directory with no provable manifest (Go pflag) made indexed_semantic_versions/indexed_package_records/indexed_package_coordinates fail every query (versions=Unknown(Gap ReadFailed) for toml_pin and toml-0.8.23)
surface: releases/time travel/upgrade
problem: Fixed in product_state.rs with tests; the GUI effect (ticker, Versions lens and banner on local roots) never re-observed; R-Folio D3 (ticker absent on is_local roots) is a different cause (page_mapping.rs:1754 LocalProject).
evidence: TS/VERSIONS.md:1-214
status: data FIXED (tests); GUI unverified
real data: fixture
sources: TS/VERSIONS.md; RF/REVIEW.md:110

### TIS-10 · TIMING.md: wall-clock assertions replaced by operation counts in four local-service/library tests; three flaky timing tests named earlier (borrowed_row_claims..., package_row_changes..., borrowed_file_splice...) 
surface: harness/journeys/lints
problem: The remaining GUI tests that assert wall-clock or virtual-time budgets under load (hover rest, animation settle budgets, perf budgets in debug) were not audited for the same flake class.
evidence: TS/TIMING.md:1-360
status: data-layer only; GUI-layer audit not done
real data: N/A
sources: TS/TIMING.md

### TIS-11 · two compile breaks at HEAD 249dd537d in local-service tests (discovery_search.rs:6024 E0382, no_result_retirement.rs:879 E0507) reported not fixed
surface: harness/journeys/lints
problem: Unrelated to the GUI but blocked `cargo test -p backend-local-service`; F-Shell-1 later ran local-service tests green (639 + 1 passed), so fixed.
evidence: TS/TIMING.md:349-360
status: FIXED (final/F-Shell-1.md:16)
real data: N/A
sources: TS/TIMING.md:349-360; final/F-Shell-1.md:16

## SECTION 15: transitions: transitions/BASELINE.md, CP1.md (T0+T1), CP2.md (T2 + handoff), CHECKPOINT-1.md (pointer). W-Flip's last checkpoint was CP2; "the owner has moved transitions to a Sonnet lane" but NO CP3 or later transitions file exists. Paths relative to W6/transitions (TR).

### TRN-1 · T3 FLIP (flow.rs and presence.rs to the three-phase rules) not done
surface: motion/transitions
problem: Baseline gallery films still FAIL: flow-list overlap 11 faded 10, flow-reflow 57/0 (card descriptions cross), flow-descent 2/2, flow-graph 9/5 (crossfaded_name); Find narrowing in the desktop, the shelf roll (shelf.rs:242,254), text-size Anchor and "pin: the card travels" (float-pin 0/2) never done. MERGED: also wave2 W-Flow: flow-list (22/2460 continuity, 2/40 settle) and flow-reflow (238/6041) failing since wave2: a Presence room animation reports as jump; epoch rebase does not absorb a room-class change during a drag (L/wave2/flow/CHECKPOINT-2.md tail)
evidence: TR/CP2.md:116-121; TR/BASELINE.md:20-35
status: still open (no later transitions file; in-flight Flow chips cross: FLUID-4)
real data: gallery + rig; never on the real Library
sources: TR/CP2.md:116-121; TR/BASELINE.md:26-35

### TRN-2 · T4 "the purge": every remaining fade in CATALOG section 4 (toast-film 0/5, dialog-film 0/8, version-comb 8/15, controls-live 0/10, flow-presence 2/6, float-sweep 1/0, film-rose 1/0); reduced motion becomes a cut plus motion::mark
surface: motion/transitions
problem: Fades still exist in toast, dialog, controls, version comb; the legibility catalog was 7 pass, 15 fail, 3 not exercised at the T0 baseline.
evidence: TR/CP2.md:122; TR/BASELINE.md:16-36
status: not built; legibility --catalog was never re-run after FEEL's float changes (FEEL F3 changed tip rests) or on the final tree
real data: gallery only
sources: TR/CP2.md:122; TR/BASELINE.md:16-36

### TRN-3 · T5 Push, Peel, Reel, Odometer not built: across (settings, compare, symbol to symbol) plays Open/Close by history; Cmd-. (page to code) is a cut
surface: motion/transitions
problem: A view switch between page/code/graph has no Peel; sideways moves have no Push; the odometer for counts and the reel do not exist (grep of apps/facet/src/motion finds none of Peel/Odometer/Reel).
evidence: TR/CP2.md:123-125; TR/CP1.md:131-133
status: not built
real data: never looked at
sources: TR/CP2.md:123-125; TR/CP1.md:131-133

### TRN-4 · title rides the plate's edge (R1): the page title keyed as a shared element so the name grows out of the clicked row on Open and into the node label on Fold; needs bodies/symbol.rs title wrapped in shared(("title", key))
surface: motion/transitions
problem: The symbol body was rewritten (SYM6) after the request; whether the new title is shared is unrecorded; COHESION vs PLAN section 2a disagree on "lands at title size in one swap" vs "grows to Display" (lead's call). MERGED: also W-Page2: shelf rows and jump rows lack title_key(symbol.as_str()) so a row-to-page title morph has no start on those surfaces (page/CP2.md:118,227); W-Glyph asked module cards to call shared::remember on click (glyph/GLYPH-A.md:61) MERGED: (see PG-3 merge) module cards do not call facet::motion::shared::remember on click (glyph/GLYPH-A.md:61)
evidence: TR/CP1.md:136-143; TR/CP2.md:89
status: unresolved (R1 still open at CP2)
real data: rig only
sources: TR/CP1.md:9,136-143; TR/CP2.md:89

### TRN-5 · rest of T2: prism rail labels at 60-70% of their best for 226-233 frames and the prism gather fades at 1056-1136 ms; stubs do not travel to their ports (needs anatomy::page::Anchors); the camera does not pull back (GraphView::enter_from); title to node label
surface: graph/world/hand
problem: graph-flight-a still overlap 7 faded 15, graph-journey 12/15, graph-hover 0/1 after T2.
evidence: TR/CP2.md:111-115,71-78
status: not done; prism has its own contrast test (FEEL-D8 fixed)
real data: fixture world
sources: TR/CP2.md:111-115,71-78

### TRN-7 · the pixel transition catalog (legibility --catalog --only A-routes) never ran on the desktop harness; desktop route transitions judged only by the gpui-rig route ledger (text trace, no contrast, cannot see opaque plates)
surface: harness/journeys/lints
problem: Every desktop scene died in fixture boot at the time; the rig ledger is "stricter about crossings, blind to contrast"; the later harness fixes (W-Index) were never used to run the pixel catalog.
evidence: TR/CP1.md:24-53; TR/BASELINE.md:38-51
status: still open
real data: fixture/rig
sources: TR/CP1.md:24-53; TR/BASELINE.md:38-51

### TRN-8 · coverage the catalog lacks: keyboard peek and pin, graph tour, "to world" Esc, jump menus, sibling (Option-Down), drag reorder, desktop toasts and dialogs, reduced-motion variants; film-comb, film-mosaic, graph-focus draw no changing text (NOT EXERCISED)
surface: motion/transitions
problem: No film for these.
evidence: TR/BASELINE.md:71-73
status: not done
real data: never looked at
sources: TR/BASELINE.md:71-73

### TRN-9 · queued transitions after the slices: the sidebar lens switch (the list rolls, the reader never moves), hoist push/pop, Library card to package (shingles fly to row marks while the page unrolls; board ?v=film-browse)
surface: motion/transitions
problem: Queued by the lead in COHESION.md, not started.
evidence: TR/CP1.md:162-163; TR/CP2.md:126-129
status: not built (RFolio-D11 Phase C)
real data: never looked at
sources: TR/CP1.md:162-163; TR/CP2.md:126-129

### TRN-10 · Open/Close: view switch, graph arrivals/departures and settings/compare/find have no origin row so the plate enters from the reader's right edge; per-line settles need the page lane's print(order) wrappers; Close's row tint is periwinkle 0.09 (probe key reads oddly for page-case-0-carries-0-door)
surface: motion/transitions
problem: Decisions 2-4 of CP1 (no invented origin, across is Open for now, one edge not per line) are accepted stopgaps.
evidence: TR/CP1.md:128-143
status: accepted; T5 to replace
real data: rig
sources: TR/CP1.md:128-143

### TRN-11 · rig judge is stricter about crossings but blind to contrast and cannot see an opaque plate hiding a line; graph native scenario test failed once under load ("quiet tail requests frames") then passed three times: unexplained
surface: harness/journeys/lints
problem: An intermittent failure recorded as unexplained; the "flaky is a defect claim" rule says loop it in-suite.
evidence: TR/CP2.md:56
status: unresolved
real data: N/A
sources: TR/CP2.md:56

### TRN-12 · films from the text trace (not pixels) on a 3-node fixture graph, so the fold/unfold look sparse; face approximated (Geist)
surface: motion/transitions
problem: The storyboard comparison for Open/Close/Fold/Unfold never used real pixels.
evidence: TR/CP1.md:121; TR/CP2.md:85
status: never redone on pixels
real data: fixture
sources: TR/CP1.md:121; TR/CP2.md:85

## SECTION 16: page/BRIEF.md, CP1.md, CP2.md (W-Page2, Opus, 2026-09-28: the "drawn page", direction A: fork / bracket / pipe / socket / rails). This design was SUPERSEDED on 2026-09-29 by the simple docs.rs-like symbol page (SYM6) and the folio package page (FOLIO); W-Page2 stopped after CP2 (no CP3, S8-S9 never built). world/BRIEF.md (W-World, queued, never launched) and wave6/PLAN.md are covered here too. Paths relative to W6/page (PG), W6/world (WD), W6.

### PG-1 · W-Page2 slices S5 bracket (nested inherited base, presence grid), S8 failure tree/comb/markdown docs, S9 package page + J1's package checkpoints ("Start here" order, "0.8.23" on the page, deps clickable): never built; replaced by the folio and simple symbol page
surface: symbol page
problem: The drawn page survives only in the gallery (page-* scenes, facet/src/anatomy/page/**) and the world-fed pieces; the desktop symbol body no longer mounts it (SYM6-A deleted the plan-driven page).
evidence: PG/CP2.md:233-238; PG/CP1.md:264-272; PG/BRIEF.md:96-104
status: superseded / abandoned; the old facet files remain for gallery scenes, graph peek, Find and Compare (S/SYM6-A.md:7)
real data: gallery only
sources: PG/CP2.md:233-238; S/SYM6-A.md:7

### PG-2 · data gaps found by W-Page2 that still bound the symbol page: TS type-alias right-hand side not recorded ($ZodIssue signature cut at "="), Go constants not members of their named type, no "Returns" link kind (a free function returning a type is only a TypeReference), pipe for methods (receiver makers) unbuilt, Go policy fork/Io pruned/TS throws from bodies unbuilt
surface: data-feed
problem: Index producer gaps block discriminated-union forks, "made by"/"getting one" lists for types made by free functions (relation_label for RelationLabel), Go enums (each constant is its own page read), Go implemented_by, whether Go groups methods by receiver.
evidence: PG/CP1.md:220-225,254-256; PG/CP2.md:200-225
status: still open (index-lane requests; SYM6 "elsewhere in registry" and Beside signature share the same class)
real data: pinned tests only; never seen on real Go/TS packages
sources: PG/CP1.md:254-256; PG/CP2.md:200-225

### PG-4 · reader watches only its route's keys: companions (Go constants) need a body-declared extra key list; Lens, set_lens, jump_symbol_section have no callers from the symbol page
surface: keys/focus
problem: W-Page2 asked W-Flip/the reader to accept extra page keys and delete dead lens code.
evidence: PG/CP1.md:249-252
status: unresolved (reader.rs SymbolFold cleanup RSym-C3)
real data: N/A
sources: PG/CP1.md:249-252

### PG-5 · owner-visible first-viewport laws (hero + specimen + first section above the fold at 1440x900; one row per member 24 px; counts on strokes; section heads as spine marks) were tested for the fork page only
surface: symbol page
problem: The compactness laws were the owner's request ("compactness and elegance") and drove the SYM6 design; the simple page has its own 8+ page tests; the laws for the package folio (first viewport) unverified on real packages.
evidence: PG/BRIEF.md:61-77; PG/CP1.md:116-131
status: partially carried over
real data: fixture pages
sources: PG/BRIEF.md:61-77; PG/CP1.md:116-131

### WLD-1 · W-World never launched: "one universe": the desktop graph and the anatomy still read the design-system snapshot (fixture_world.rs parses Nudox-Design-System/v4/graph/world.json, 16 MB); the whole-world projection surface on the owner, the columnar mmap format, LOD label placement, graph open <= 100 ms, package tour from index evidence, deleting "Graph fixture" from the foot were never built
surface: graph/world/hand
problem: The world view shows the backend's own crates (store/library/engine) while the Library lists toml/serde; the hand's arrangement and the graph read the prototype world; the package tour ("Start here") was deleted and never returned; a stranger sees a labelled fixture.
evidence: WD/BRIEF.md:1-75; PR:91; apps/desktop/src/shell/bodies/graph.rs:2 (label)
status: still open (owner decision: ship labelled, hide until real, or schedule the data work: PR:365)
real data: FIXTURE (prototype world)
sources: WD/BRIEF.md; PR:91,365; IT/PLAN.md:264-281

### WLD-2 · graph tests pin "Graph fixture" provenance strings; flipping them to projected provenance is part of the unbuilt W-World CP2
surface: graph/world/hand
problem: Tests and copy encode the fixture.
evidence: WD/BRIEF.md:59; reader.rs:664, bodies/graph.rs:413,1262, runtime/graph_focus.rs:48
status: open
real data: FIXTURE
sources: WD/BRIEF.md:59

### PLN-1 · wave-6 walk findings from 2026-09-28 (PLAN.md): several remain unrecorded as fixed - Search in four places (Ask, Find page with FIND header, graph "Find a symbol" field facet/src/graph/view.rs:752, shelf Filter) that "don't know about each other"; Home as a chip cloud repeating the shelf, no resume line, no project centre; code view no syntax colour, a second breadcrumb, a lone margin doc; Find has a Compare button on every result row and a mint "Explore declaration" button (mint = yours only); the foot empty except the graph fixture text
surface: shell/frame
problem: The lead's "shell is one space" build items: one query (jump bar as query when you type; results scoped here/package/everywhere; the shelf narrows; names on the page take the hover underline; graph matches light), Home is the map (Orbit4 rings, resume line, new-release line, Map/List), the foot (address nudox://..., click offers Copy link / Open in editor / Reveal, hand marks, live index rule) - status of each unrecorded in the lane files.
evidence: W6/PLAN.md:13-81
status: partly overtaken (sidebar narrowing landed as SIDE; Ask has its field in the titlebar; graph "Find a symbol" field still in facet/src/graph/view.rs:752; nudox:// appears in titlebar/jump/status code); the rest unverified; PR §0 says Ask (Cmd-K) works
real data: fixture
sources: W6/PLAN.md:13-81; PR:35

### PLN-2 · the wave-6 definition of done journeys: J1 first look, J3 find (type, world lights, open, back), J4 graph round trip (page, map, neighbour, page), J2 understand a symbol (hover, peek, pin, compare), J5 upgrade (comb, what changed, your code touched), J6 first run
surface: harness/journeys/lints
problem: Renumbered later into J0-J14 (JOURNEY-A); only J0, J1, J9, J10 exist as files (apps/desktop/journeys), J11 generated; J2-J8, J12-J14 unwritten (M-final M4).
evidence: W6/PLAN.md:75-81; W6/journey/JOURNEY-A.md
status: still open
real data: N/A
sources: W6/PLAN.md:75-81; M-final M4

### PLN-3 · W-Surfaces (wave4/surfaces/BRIEF.md minus Orbit: lenses, peeks, Settings, Ask actions, Source) queued, never launched
surface: shell/frame
problem: Settings (Agents page mapped to Appearance), Ask actions, Source view features have no lane report.
evidence: W6/PLAN.md:54-55; W6/surfaces/ (empty dir)
status: not built as a lane; Settings/Ask/Source got fragments elsewhere
real data: never looked at
sources: W6/PLAN.md:54-55

### PLN-4 · W-Green (every suite green, ROUTED.md, CI stand-in watch rounds) and W-Owner (non-blocking reads, typed progress channel, per-package publication, W-Owner.md brief) launched at 08:40 and died at the 08:55 rate limit; owner/ and green/ dirs hold logs only
surface: harness/journeys/lints
problem: The owner work was picked up by F-Data (reads during phase B only; progress deferred); green by F-Shell (suite counts 522/2 then 542/0 vs 538/4).
evidence: W6/PLAN.md:153-158; W6/owner/ (logs), W6/green/ (logs)
status: partly overtaken (M-final M33, M34, M48)
real data: N/A
sources: W6/PLAN.md:153-158; M-final M33,M34,M48

## SECTION 17: research/MOMENTS.md (2026-09-28 20:22, precedent for SCAN / JOIN / HOP; section 1 ranks 12 mechanisms, "my judgement, not measurements"; each names a "smallest test" nobody ran) and research/NAV.md (2026-09-28 18:13, sidebars, disclosure, dependencies, version diffs). Memory note: on 2026-09-29 the owner rejected "lab instruments" as too ambitious and asked for docs.rs simplicity with progressive disclosure; treat these as UNBUILT IDEAS, not commitments. Paths relative to W6/research (RS-M = MOMENTS, RS-N = NAV).

### RES-1 · mechanism 1 "Hover prices the closure and names what dies": hover a package to light its closure, price "+N new", show the exclusive set and removal delta (Path of Building / retained size)
surface: Find/browse
problem: Never built; the Library and package page show "N packages beneath - in your lock" (weight berg) but no hover pricing, no exclusive set, no lasso; the smallest test (PoB strings over a real lockfile, exclusive set equals dominator subtree) not run.
evidence: RS-M:29,44
status: unbuilt idea (U5 D4)
real data: never looked at
sources: RS-M:29,44

### RES-2 · mechanism 2 "A landing that says what went where and what came with it": Add-from-browsing landing sequence (room first 200-250 ms, arrival >= 330 ms, consequence flash, count ticks at landing, burst merges with 80 ms floor, first-ever add may carry delight, fortieth quiet)
surface: Find/browse
problem: The Add flow (G10) has only an offer -> seam progress -> added/failed component; the landing, count roll, dependency stagger and "12-dependency" test were never built or tried.
evidence: RS-M:30,46
status: unbuilt idea; ACQ-4 unverified
real data: never looked at
sources: RS-M:30,46; AQ/ACQUIRE-A.md:43

### RES-3 · mechanism 3 "Add-armed consequence lens": pressing Add repaints the user's project into five plate states (new, shared, duplicate, conflict, unaffected) with geometry frozen, KiCad ratsnest for unmet needs
surface: Find/browse
problem: Requires the registry resolver and duplicate detection the offline adapter does not have; nothing was mocked.
evidence: RS-M:31,48
status: unbuilt idea
real data: never looked at
sources: RS-M:31,48

### RES-4 · mechanism 4 "One variable painted over frozen geometry": a lens bar (transitive weight, exclusive weight, usage, releases behind, duplicate count) painting the Library list/graph, scale break for outliers, hold-a-key x-ray
surface: Library
problem: The sidebar has lenses (Contents/Versions/Rests on/Used by) but they list, not paint; the Library ring has no weight lens; smallest test (200 real packages, find three heaviest vs sorted list) not run.
evidence: RS-M:32,50
status: unbuilt idea; overlaps the graph "weather" journeys J6/J14
real data: never looked at (only 12-13 real packages exist)
sources: RS-M:32,50

### RES-5 · mechanism 5 "Fixed-position web where learning changes what a card says": Outer Wilds rumor mode for dependencies you have not opened (wears the requested range and the dependent that asked); three-state map (never opened / older / current)
surface: graph/world/hand
problem: Not built; the graph is the fixture world; SpaceTree study supports stable layout; no test of "no card moves across ten hops".
evidence: RS-M:33,52
status: unbuilt idea
real data: never looked at
sources: RS-M:33,52

### RES-6 · mechanism 6 "Surface fingerprint" (usage vs public surface as a fixed dot field 32-48 px) and mechanism 7 "Release history as a turning path with legend on the back" (Notabilia, plate flip)
surface: package page
problem: The package page has shingles (territory), a ticker and a berg; neither fingerprint nor turning path exists; smallest tests (five real crates, 30 real crates) not run.
evidence: RS-M:34-35,54,56
status: unbuilt idea (owner steered to simplicity)
real data: never looked at
sources: RS-M:34-35,54,56

### RES-7 · mechanism 8 "Hop grammar": direction as a key (R down, U up), one press one ring, undo peels the outermost, peek locks with a visible countdown, modifier-click promotes peek to page
surface: keys/focus
problem: The Library/sidebar have G C/V/R/U lens chords and Space peek, but there is no ring-by-press hop, no nested/locked peek chain, no promote; the smallest test (J4-style round trips, count key presses and lost-place events) not run.
evidence: RS-M:36,58
status: unbuilt idea; J4 (the map) unwritten (M-final M4)
real data: never looked at
sources: RS-M:36,58

### RES-8 · mechanism 9 "join sentence whose version range you can scrub" (Tangle), mechanism 10 "balance for choosing or replacing" (PhET), mechanism 11 "marker on the population + project histogram", mechanism 12 "cheap way home" (40 px spines, distance marker, faster return than way in)
surface: Find/browse
problem: None built; Compare (Held cards) exists but no balance; the sidebar has a Trail (places behind you) but no spines, no return speed rule, no distance marker.
evidence: RS-M:37-40,60-66
status: unbuilt ideas; SIDE-C Trail partially covers 12
real data: never looked at
sources: RS-M:37-40,60-66; SD/SIDE-C.md:18

### RES-9 · hover delay/dismissal numbers: two timers not one (Wikipedia 150 ms start, 500 ms floor, 300 ms grace; Radix 700; Gwern 750), the cost of a wrong open sets the delay; the app's Tip 350 (after FEEL F3), Peek 350, Quick rest 120, Menu 0
surface: overlays/popovers
problem: Contradiction 4 (hover delay 0 vs 500-750 ms) is unresolved for the Library, where no popover exists (FEEL-D9); the owner's bar of 400 ms rest and 120 ms dismissal was applied to tips only.
evidence: RS-M:817,836
status: owner decision open (FEEL-D9, FEEL-D13)
real data: fixture
sources: RS-M:817,836; review/feel/FEEL.md:36-42

### RES-10 · contradictions to decide before building: hop moves the world or holds it still (SpaceTree: stable layout); animate the hop 1 s vs 250 ms; read-first vs act-first at the join; enumerate transitive additions vs hide; gate vs after-line; full trail vs truncating stack; delight vs frequency
surface: motion/transitions
problem: 15 contradictions listed in section 8 with no ruling recorded for the app (the app's CARRY 0.28 s decided the hop duration case implicitly).
evidence: RS-M:831-847
status: owner decisions unrecorded
real data: N/A
sources: RS-M:831-847

### RES-11 · what the research could not verify (section 10): Apple zoom/Quick Look durations, Balatro Game:update constants, closed-engine games, Paradox lock timer, registry sites (npmjs, Socket, Maven, Snyk 403), lib.rs pages; web search budget exhausted (200/200) so no second pass
surface: other
problem: Any number in the research marked [E] is recalled, not fetched; "do not build on it".
evidence: RS-M:895-907
status: caveat, no action
real data: N/A
sources: RS-M:895-907

### RES-12 · NAV open question: no tool computes a version diff pre-filtered to one consumer's call sites - the app's Upgrade lens ("the 3 things that changed that your code touches") is an unsolved, uncopied design; real diffs (G14) do not exist so it was never built for real
surface: releases/time travel/upgrade
problem: The sidebar StateBook and package banner draw fixture diffs for toml/smallvec; the real per-consumer diff needs SurfaceCommand::Diff between indexed releases plus workspace usage (impact vector empty in untracked releases.rs).
evidence: RS-N:621,610
status: open design + data
real data: FIXTURE
sources: RS-N:610,621; M-final M8

### RES-13 · NAV open questions: Obsidian switches its quick-switcher algorithm past 10,000 items; the app's Ask/narrowing/Find run over 12,000+ declarations and 665 sidebar rows in debug at "a few seconds" - fuzzy matching, recents as the empty-query default, and scaling behaviour never designed or measured
surface: Ask/search
problem: NAV patterns "type-to-filter with recents as empty state", "narrow-then-widen staging" (Things), "in-file vs project-wide symbol search split (VS Code Ctrl+Shift+O vs workspace)"; Ask has none of recents/scoping; sidebar narrowing is the in-scope filter; M-final M21 search latency.
evidence: RS-N:251-257,620
status: open
real data: REAL latency (few seconds debug)
sources: RS-N:249-257,620; M-final M21

### RES-14 · NAV: hoist/zoom vs secondary panes (Logseq bug), sticky ancestors adoption, overview curation vs show-everything, "why is this here" partial vs complete answers: none signalled in the app's "Used by"/"Rests on" lenses (does a lens say it shows one hop or all paths?)
surface: sidebar
problem: The Used by / Rests on lenses do not say how complete they are; the ring on Library lists what projects use (F-Shell) without saying depth.
evidence: RS-N:622-625
status: open design question
real data: never looked at
sources: RS-N:622-625

## SECTION 18: older waves (wave2 controls/data/float/flow/harness/shell; wave3 anatomy/graph; briefs/next-*.md, w-journeys.md; wave4 GRAPH-HANDOFF, QUEUE, walk/FINDINGS, audit/LEDGER, surfaces/BRIEF, docsrs/PARITY, RULINGS of hand/browse/marks/motion/page2, browse-impl, marks-impl, facts; wave5 QUEUE, marks/CHECKPOINT-2, motion/PLAN, page/DESIGN). Most of this is superseded by v5/v6; only items whose current state I could establish by grep of the tree on 2026-09-30, or that no later file closes, are kept. Paths relative to .local/lanes (L).

### OLD-1 · Lenses (release / module / language) overlay: facet::overlay::lens is built and tested but has ZERO call sites in apps/desktop (wave4 audit: "an entire target board has no path to it")
surface: overlays/popovers
problem: The release, module and language lenses (Lenses.png board) do not exist in the shipped app; the sidebar lens strip is a different thing.
evidence: L/wave4/audit/LEDGER.md:86-98; L/wave4/surfaces/BRIEF.md:22-25; grep 2026-09-30: no overlay::lens use in apps/desktop/src
status: still open (W-Surfaces never launched)
real data: never looked at
sources: L/wave4/audit/LEDGER.md:29-31,86-98; L/wave4/surfaces/BRIEF.md

### OLD-2 · Peeks for package / location / version: PageKey has no Location or Version variant; Package peek is now wired (shell/peeks.rs:74-77, name and dossier) but Location and Version are not
surface: overlays/popovers
problem: Location and version peeks (from a comb tick or a file location) cannot be named in the model; Peek::Package content on the real install unverified.
evidence: L/wave4/audit/LEDGER.md:100-123; model/pages/key.rs:42-57
status: partly open (package peek exists; location/version missing)
real data: never looked at
sources: L/wave4/audit/LEDGER.md:100-123

### OLD-3 · peek "why care" fact ("14 uses in your code") missing at rest and under every modifier; Option x-ray does not enrich the card (compass bar, facts row, file comb); pin has no destination below 1900 px (the Cmd-P stack half of the pin contract is not built); chained peek unverified
surface: overlays/popovers
problem: gui-plan section 6.2 rule 7 broken for general peeks; SIDE-B's sidebar peek wraps a YOUR CODE section only for sidebar rows.
evidence: L/wave4/audit/LEDGER.md:199-248
status: partly open (sidebar peek has uses; page and symbol-page peeks unknown); pin stack still unbuilt
real data: fixture
sources: L/wave4/audit/LEDGER.md:199-248; SD/SIDE-B.md:32

### OLD-4 · hover never opens a peek: kit.rs HoverIntent only prefetches; float::open is called only from root.rs peek() on keyboard focus; on the package page's crest hovering opens plates but on the Library, sidebar rows and the symbol page's relation rows hover opens nothing
surface: overlays/popovers
problem: gui-plan section 6.1 says hovering shows the peek one rung up; the audit found it keyboard-only. Symbol page hoverables (SYM6 generic/error card) and the folio have their own hover cards; Library words and sidebar rows do not (FEEL-D9).
evidence: L/wave4/audit/LEDGER.md:205-212
status: partly open; owner decision for the Library (PR:189)
real data: fixture
sources: L/wave4/audit/LEDGER.md:205-212; FEEL-D9

### OLD-5 · Option x-ray inconsistent: it rewrites the symbol page's prose (Getting one collapses to code, as_str gains its return type) but the "can" capability chips and the code view (Source4) never read ctx.reveal.xray
surface: symbol page
problem: The x-ray contract (Ladder board) is only half-honoured; the SYM6 page has no can chips row in the old form, source view still ignores x-ray.
evidence: L/wave4/audit/LEDGER.md:250-290
status: unverified on the new pages; code view (bodies/source.rs, 190 lines) has no syntax colour or x-ray as of 2026-09-30
real data: fixture
sources: L/wave4/audit/LEDGER.md:250-290; L/wave4/walk/FINDINGS.md:5

### OLD-6 · code view (Source4): "a specialized GUI format instead of code views" (owner's words): neighbours folded to one line each, mint ticks on lines from the newest release, doc and callers in the margin, Option spells ages, syntax colour, second breadcrumb removed, wrapped signature continuation indent
surface: code view
problem: The desktop source view is a flat excerpt (bodies/source.rs 190 lines, no token colours); none of the Source4 features exist; 760/480 word-wrap loses continuation indent.
evidence: L/wave4/walk/FINDINGS.md (item 5); L/wave4/audit/LEDGER.md:270-290; L/wave4/surfaces/BRIEF.md:6 item 6
status: still open (W-Surfaces never launched; PR does not list it)
real data: never looked at on the real install
sources: L/wave4/walk/FINDINGS.md; L/wave4/audit/LEDGER.md:270-290

### OLD-7 · Settings pages: 6 of 10 pages are aliases (Editor, Agents, Privacy all show Appearance; Help/Legend show keys; Diagnostics/Connections show about; Registry shows index); Text-size row decided against ("the system sets it"); RetryIndex/CancelIndex/TestConnection controls on Index and Connections
surface: settings
problem: apps/desktop/src/shell/bodies/settings.rs:25-32 still maps the pages this way; nav lists pages that have no content of their own; F-Shell/F-Data added the Keys list and toolchain report.
evidence: L/wave4/surfaces/BRIEF.md:29-33; L/wave4/audit/LEDGER.md:125-143; settings.rs:21-30 on 2026-09-30
status: still open (L1 MCP; editor setting RSym-N3; privacy/agents pages)
real data: never looked at
sources: L/wave4/surfaces/BRIEF.md:29-33; W6/journey/GAPS.md:140-143; RSym-N3

### OLD-8 · Ask actions: Intent::Action and the action.rs catalog: Ask lists actions as rows like a command palette, or the catalog is deleted (a decision to write down); Overlay::CommandPalette falls through in bodies/mod.rs:249 (no palette exists)
surface: Ask/search
problem: Dead catalog; L2 documented shortcuts (cmd-shift-p, cmd-b ...) belong to it.
evidence: L/wave4/surfaces/BRIEF.md:34-38; bodies/mod.rs:249
status: still open (L2)
real data: N/A
sources: L/wave4/surfaces/BRIEF.md:34-38; W6/journey/GAPS.md:145-148

### OLD-10 · Orbit4 board: projects at the centre, two rings of names, one resume line (the hand), one "new release" line, Map / List toggle, OrbitRoute::Project(id) ignores its id
surface: Library
problem: The Library has a project tile and a ring; no resume line, no new-release line, no Map/List toggle found in orbit.rs; project route with id unresolved.
evidence: L/wave4/surfaces/BRIEF.md:39-43; L/wave4/walk/FINDINGS.md item 1; jump.rs:244, route.rs:374
status: partly built (resume() exists at orbit.rs; F-Shell reworked the ring); the rest unverified
real data: REAL project tile + ring only
sources: L/wave4/surfaces/BRIEF.md:39-43

### OLD-11 · the 760 px spine shows 25 identical bracket glyphs (depth-0 rows); target: ancestors then siblings as kind marks, hover label, never the flat module list; shelf overlay under 640 px had no scrim
surface: sidebar
problem: shelf.rs:685 was replaced by side/*; spine behaviour at 42 px under the new sidebar not reported in any file; scrim FIXED by FLUID-B.
evidence: L/wave4/audit/LEDGER.md:292-334; L/wave4/surfaces/BRIEF.md:44-48
status: scrim FIXED; spine content unverified after SIDE rewrite
real data: never looked at
sources: L/wave4/audit/LEDGER.md:292-334

### OLD-12 · systemic Abyss contrast failures across every checkpoint (Orbit 3.23:1, Library 3.25:1, runtime/toml_pin chips 2.7:1, grammar shelf row 1.10:1) and titlebar breadcrumb hit targets 16 px tall; effect of Contrast::High on them never checked
surface: contrast/a11y
problem: Partly fixed by ink3 sweeps (TIS-4, D1), FEEL contrast-high PASS on orbit/find only; breadcrumb hit target 16 px unrecorded.
evidence: L/wave4/audit/LEDGER.md:61-84,358-366
status: partly FIXED; breadcrumb segment hit target size unverified
real data: fixture
sources: L/wave4/audit/LEDGER.md:61-84

### OLD-13 · reduced motion verified only by one static capture and per-journey clean motion reports; a real film across a transition with motion off for each verb never made
surface: motion/transitions
problem: FEEL checked shelf toggle only.
evidence: L/wave4/audit/LEDGER.md:401-410; FEEL.md:49
status: partly verified (shelf; reduced arrival test motion_tests)
real data: fixture
sources: L/wave4/audit/LEDGER.md:401-410

### OLD-14 · docs.rs parity gaps (PARITY ranked 15), unverified after SYM6: required vs provided trait methods (Member.required), Arrival::Blanket/Auto never constructed, item-level deprecation payload, cfg/feature gates invisible (item can be missing from the index under one fixed feature policy), member/variant docs truncated to one line, no Errors/Panics/Safety section detection (SYM6 lifts errors out of docs), "In use" as code (done via UseLine), no release history in production (fixture only), Deref-derived methods absent, blanket impls onto &T/Cow/Box vanish, auto traits not computed, macro signatures near-empty, dyn-compatibility note, provenance/confidence never rendered (ReferenceSite.confidence unread by UI)
surface: symbol page
problem: The PARITY gap list is the standing "what docs.rs shows that Nudox does not" list; SYM6 adds "you write / you get" for traits (Owes) and errors; deprecation shows coral in the sidebar (DeclRef.facts); "83% derive" and "Elsewhere in the registry" explicitly deferred.
evidence: L/wave4/docsrs/PARITY.md:133-200; L/wave4/green/REPORT.md:303-312; S/SYM6-B.md:51-52
status: mostly still open; no lane re-scored PARITY after the symbol page rewrite
real data: never looked at (the real toml Value page is the only one the lead cites)
sources: L/wave4/docsrs/PARITY.md:133-200; L/wave4/page2/CHECKPOINT.md:81-125

### OLD-15 · Rust facts the engine lacks for the page (page2 named gaps 1-12): cfg predicate text per item/impl, feature unification ("on because of X"), impl blocks as first-class rows with trait args and associated items, type-level bounds/where-clauses, full member docs, #[deprecated(since, note)], required-vs-provided, Arrival for every capability, Deref target and its methods, macro-made impls, release history for every locked crate, the joint (exact span) per relation
surface: data-feed
problem: The index does not carry these facts; recipe engine drops producers whose gate is off ("const_new not in your build").
evidence: L/wave4/page2/CHECKPOINT.md:81-125
status: still open (W-Facts Phase 2 handed off and never run: L/wave4/QUEUE.md:handed off)
real data: never looked at
sources: L/wave4/page2/CHECKPOINT.md:81-125; L/wave4/QUEUE.md

### OLD-16 · Facts Phase 2 (blanket impls R1/R2, Variant to Constant, impl rows, gates R3, since) handed off; facts_polyglot.rs (desktop e2e across the seven languages, needs each toolchain) deferred; content_truth.rs Phase-1 additions (deprecated fn, required/provided trait, cfg item, two-paragraph member doc, derive, blanket impl) not added
surface: data-feed
problem: No test proves the seven-language fact merge in the desktop.
evidence: L/wave4/facts/CHECKPOINT-1.md:93-140
status: not done
real data: never looked at (only Rust ran through the desktop)
sources: L/wave4/facts/CHECKPOINT-1.md:93-140; IDX-3

### OLD-17 · package page tree context: duplicates (crates at two versions), direct-vs-transitive, tree licenses and dependency usage/purpose are not fed to the marks (DepFacts.in_tree None, tree_note "your tree is not read yet", uses/purpose empty); a package route has no way back to the referring project
surface: package page
problem: Dependency marks on the folio still render unknown/none for tree standing and usage; apps/desktop/src/shell/bodies/package.rs:308-313 sets in_tree: None.
evidence: L/wave5/marks/CHECKPOINT-2.md:141-155; package.rs:308-313
status: still open (2026-09-30)
real data: never looked at
sources: L/wave5/marks/CHECKPOINT-2.md:141-155

### OLD-18 · Add as "write to Cargo.toml": the browse ruling said Add writes the dependency (workspace.dependencies with workspace = true, else the member's table), shows the exact diff first, writes through the backend, never silent; nothing like it exists - the shipped "Add" indexes a crate into the library only
surface: Find/browse
problem: The dependency-adoption flow (Compare's adoption preview: "77 of 77 lines change only in name" false-equivalence rule; "only in name" only when every substituted item has the same shape) is unbuilt in the desktop; the false equivalence defect must not return.
evidence: L/wave4/browse/RULINGS.md:1-26
status: unbuilt spec; product meaning of "Add" unclear to a user (G10 = add to library)
real data: never looked at
sources: L/wave4/browse/RULINGS.md; W6/journey/GAPS.md:96-105

### OLD-19 · hand: one hand across projects (ruled), keys Cmd-1..5 with depth on Ctrl-1..4, Back history per session only, held trait filters the recipe ("roads through Deserialize"), first-card whisper once per install; the chain API convert/join/arrange in facet::semantics::recipes
surface: hand/holds
problem: The hand exists (5 cards, Cmd-D, fan) but is arranged on the fixture world (RO-D4, WLD-1); the trait-filter join and the recipe sentence with real data never verified; the whisper looped (FEEL-D4).
evidence: L/wave4/hand/RULINGS.md; L/wave4/hand/CHECKPOINT.md:100-140
status: partly built; real-data behaviour unverified
real data: FIXTURE world
sources: L/wave4/hand/RULINGS.md; RO/REVIEW.md:65-69

### OLD-20 · motion rules from W-Motion rulings: counts carry their tier ("at least N places, matched by path"); the reduced-motion fallback of the graph flight is still a 120 ms CROSSFADE (flight.rs:12,73) where MOTION.md asks for a cut plus a 1.2 s mark
surface: motion/transitions
problem: A crossfade remains in the graph camera reduced-motion path, and "no fades" is a binding rule.
evidence: L/wave4/GRAPH-HANDOFF.md; L/wave4/motion/RULINGS.md:13-14; apps/facet/src/motion/flight.rs:12,73
status: still open
real data: fixture
sources: L/wave4/GRAPH-HANDOFF.md; L/wave4/motion/RULINGS.md

### OLD-21 · graph handoffs never done: "T fly it" on Start here (the strip is gone), cross-package navigation from anatomy links (graph search-and-resolve as a service), symbol page door into the graph (page rows fly into the prism, focus=<item>), the prism-as-flow "octopus" lives only in the graph, chains ("in steps") in the find box and the road (chain bead, tour) built in facet (graph/road.rs, semantics::recipes::chains, semantics::tour) but unwired to the desktop package page/Find
surface: graph/world/hand
problem: The tour (Start here) and chains were removed from or never reached the desktop pages ("no fixture-ranked tour" is asserted by tests); they wait on real world data (W-World).
evidence: L/wave4/GRAPH-HANDOFF.md; L/briefs/next-graph.md; L/briefs/next-anatomy.md; anatomy_tests.rs:333, package/tests.rs:115
status: unbuilt (blocked on WLD-1)
real data: FIXTURE
sources: L/wave4/GRAPH-HANDOFF.md; L/briefs/next-graph.md; L/briefs/next-shell.md

### OLD-22 · TS/Go/Python/Java/C# fixtures: "no harness TS, Go or Python fixtures exist yet; the TS and Go producers were not exercised offline"; only Rust runs through the real desktop; Python and Java to follow
surface: data-feed
problem: DESIGN S10 fixtures (apps/desktop/tests/fixtures/lang/{ts,go}) exist but the real journeys and installs only add a Rust project.
evidence: L/wave5/page/DESIGN.md:521-535; IDX-3
status: still open
real data: never looked at
sources: L/wave5/page/DESIGN.md:521-535

### OLD-23 · lint coverage: mixed-face lines (most type lines) are not published so clip/contrast in mixed lines are checked by eye; button labels are not probe texts (D3)
surface: harness/journeys/lints
problem: The lints cannot see many texts on the symbol page.
evidence: L/wave3/anatomy/CHECKPOINT-2.md:96-99; W6/journey/GAPS.md:164-168
status: still open (D3 open)
real data: N/A
sources: L/wave3/anatomy/CHECKPOINT-2.md:96-99

### OLD-24 · Select menu wants alignment to its trigger's start (or trigger width) and current value active; float group opacity for cut plates (translucent bevel halves showing through) - compositing.rs now exists; pose.sx/sy squash-stretch not painted
surface: overlays/popovers
problem: Old requests from W-Controls/W-Flow; whether settled unrecorded.
evidence: L/wave2/controls/CHECKPOINT-2.md:82-89; L/wave2/flow/CHECKPOINT-1.md:88-95
status: unverified
real data: fixture
sources: L/wave2/controls/CHECKPOINT-2.md; L/wave2/flow/CHECKPOINT-1.md

### OLD-26 · wave2 shell queue: "second retarget bug (reader Presence on Tab)", comb driving at=, Start here strip, page routes hosting real views - overtaken; unknown if the Tab retarget bug still exists
surface: keys/focus
problem: Tab cycles zones not visual order (RFolio-D7) is the current form.
evidence: L/wave2/shell/CHECKPOINT-2.md:200-207
status: unverified
real data: fixture
sources: L/wave2/shell/CHECKPOINT-2.md:200-207; RFolio-D7

### OLD-27 · wave2 harness: matrix sheet run only for desktop-symbol; graph/world views judged as stubs; journeys only J1/J5 planned (W-Journeys brief J1,J5,J2,J4,J3,J6 with "BLOCKED (data)" rule); old graph cache in-process only
surface: harness/journeys/lints
problem: The journey order in the 2026 brief and the J0-J14 numbering of W6 differ; none of J2-J6 exists in current files.
evidence: L/wave2/harness/CHECKPOINT-3.md:150-155; L/briefs/w-journeys.md; L/wave3/graph/CHECKPOINT-1.md:110-116
status: superseded by JRN
real data: N/A
sources: L/briefs/w-journeys.md

## SECTION 19: phases that were specified in a lane brief (W6/lead/briefs/*.md) but never ran (no checkpoint file exists), plus corrections. Existing checkpoint files were verified with ls on 2026-09-30.

### PH-1 · SYM6-C never ran: symbol page phase C "motion and fit" (hover cards 120 ms in / stay while hovered / 160 ms grace out; folds unfold in place with a height spring (CARRY); hop to a sibling keeps the shared-element title flight; reflow checks at 800/1024/1440/2560; contrast >= 3:1 on every mark and >= 4.5:1 on text) and the promised harness captures at 1024x700, 1440x900 and 2560x1440 for Value, from_str (+ generic card open), one struct, one "or nothing" method, one Python or TS declaration (board-vs-native sheets)
surface: symbol page
problem: SYM6-B says "Harness, real content (HARNESS-REAL): see SYM6-C" but the file does not exist; every SYM6 visual is a gallery capture or rig test; RSym-D1/D2/D6/D8 are exactly phase C.
evidence: W6/lead/briefs/W-Sym6.md:134-163; W6/sym6/ (only SYM6-A.md, SYM6-B.md); S/SYM6-B.md:45
status: not done
real data: the lead cites "toml's Value looks right" (PR:34); no lane file has a harness capture of the real simple symbol page
sources: W6/lead/briefs/W-Sym6.md:134-163; S/SYM6-B.md:45

### PH-2 · FOLIO-C never ran: shingles fly to their cards' marks when a module opens (CARRY), reflow 800-2560 px, contrast >= 3:1 nothing lower; R-Folio's D1-D14 fix pass; harness captures of toml/tokio/serde_json/local project at three sizes
surface: package page
problem: See RFolio-* records; F-Shell's FINISH item 5 still lists "FOLIO-C" as work to finish.
evidence: W6/lead/briefs/W-Folio.md:C; W6/folio/ (only FOLIO-A.md, FOLIO-B.md); final/F-Shell-1.md:73
status: not done (code has unreported later edits: RFolio header)
real data: never looked at on the real 12-package install beyond toml intro (final/F-Shell-1.md:37 shot)
sources: W6/lead/briefs/W-Folio.md; final/FINISH.md:80

### PH-3 · INSTALL-B and INSTALL-C never ran: failure/retry with proofs, R7 (later done by F-Data), restart persistence of every field, state set aside note, multi-project, the wide net at every width and both themes, popover latency of door tips, keys sweep
surface: onboarding/install
problem: Only INSTALL-A exists; R-Install never reviewed anything.
evidence: W6/lead/briefs/W-Install.md:checkpoints; W6/install/ (only INSTALL-A.md)
status: partly overtaken by F-Data-1/2 (persistence, refusals) but multi-project, damaged-session note, state-set-aside note and width sweeps remain unverified
real data: single-project only
sources: W6/lead/briefs/W-Install.md; INS-4, INS-5

### PH-4 · JOURNEY-B and JOURNEY-C never ran: J2-J8, J13, J14, the crawler J12 run, films/INDEX.md, mutation proofs for J0/J7/J8/J11/J12, refusal-of-stand-in proofs
surface: harness/journeys/lints
problem: Only JOURNEY-A + GAPS.md exist; F-Shell wrote J9, J10, J11 (generated), J12 (apps/desktop/journeys/J12.journey, 54 lines, committed in 8766d6460, a corrected reading of M-final M3) and ran J10 to PASS; J12's crawl (harness/journey/crawl.rs is modified and uncommitted) has no recorded run; the part library has 6 parts (first-run, install, install-by-picker, indexed, install-all, relaunch) versus the brief's list (open-package, open-symbol, search, pick-result, view-release, set, chord, hover-sweep, back-to).
evidence: W6/lead/briefs/W-Journey.md:37-155; apps/desktop/journeys/, parts/; git status
status: still open
real data: N/A
sources: W6/lead/briefs/W-Journey.md; M-final M3, M4

### PH-5 · OWNER-A/B/C never written: non-blocking reads during index (partly delivered by F-Data-2: reads answer during compile phase B, not during scan/publish), typed progress channel with real package names (not delivered), per-package publication (not delivered, reason never written), runtime/owner.rs watch weakly (delivered D2), PROGRESS-API.md (never written), kill -9 recovery (F-Data-2 crash test delivered), R-Owner never launched
surface: foot/status/lifecycle
problem: The owner half of "a person can read toml while serde compiles" remains open (M-final M33-M34).
evidence: W6/lead/briefs/W-Owner.md; W6/owner/ (logs only)
status: deferred
real data: REAL
sources: W6/lead/briefs/W-Owner.md; M-final M33, M34

### PH-6 · FEEL-B not written (FEEL-A cites "the same list as FEEL-B"); W-Feel's brief items outside FEEL.md: storm 8 seeds x 300 acts on every scene, matrix on world/graph/symbol/package, key table in symbol/find/graph contexts, semantic colour audit; R-Feel never launched
surface: harness/journeys/lints
problem: See FEEL-N2, FEEL-N3.
evidence: W6/review/feel/FEEL-A.md:13; W6/lead/briefs/W-Feel.md
status: not done
real data: fixture
sources: W6/review/feel/FEEL-A.md

### PH-7 · reviewers never launched or never finished: R-Side (SIDE-A), R-Journey, R-Install, R-Acquire, R-Feel; R-Sym6 phase 2, R-Folio phase 2 (no report); R-Fit phase 2 (code landed, no REVIEW-DONE); R-Open3 phase 2 (MIGRATE.md only)
surface: harness/journeys/lints
problem: Every wave-6 lane's defects were found by a reviewer whose fix phase mostly did not run, so the review lists (RSym-*, RFolio-*, RFit-*, ROpen-*) are the authoritative unfixed backlog.
evidence: W6/PLAN.md:98-104,148; missing files listed in this section
status: not done
real data: N/A
sources: W6/PLAN.md:98-104,148

### PH-8 · W-Surfaces and W-World were queued (PLAN.md:52-55) and never launched; W6/surfaces is an empty directory
surface: shell/frame
problem: Lenses, peeks (location/version), Settings pages, Ask actions, Source4, the world projection: see OLD-1 to OLD-10, WLD-1.
evidence: W6/PLAN.md:52-55; W6/surfaces/
status: not built
real data: N/A
sources: W6/PLAN.md:52-55

### PH-9 · W-Green never produced ROUTED.md (every suite green, root-caused; CI stand-in watch rounds); suites were made green later by F-Shell (facet 762, desktop 522/2 then 542/0 vs 538/4, local-service 639, clang 24+9, engine 602/2) but no CI exists and no single-tree green run of all suites is recorded
surface: harness/journeys/lints
problem: See M-final M48; no ci files (memory: cross-platform and CI gate).
evidence: W6/lead/briefs/W-Green.md; W6/green/ (logs); final/F-Shell-1.md:5-19
status: open
real data: N/A
sources: W6/lead/briefs/W-Green.md; M-final M48

### PH-10 · W-Green outstanding (wave4): UI never reads ReferenceSite.confidence (the model can say "matched by path" but the render layer does not surface it; SYM6-B marks Resolved vs ByName on workspace rows only); structural-lane FileSpan byte offsets are excerpt-relative rather than file-relative (a latent "jump to source" hazard for OpenSource / Source links); RecordSource mislabelling for local packages
surface: symbol page
problem: The click-to-editor path uses UseLine spans read from the local file (workspace_lines.rs); whether structural-lane spans are right for all languages is unverified.
evidence: L/wave4/green/REPORT.md:303-312; SYM6-B.md:11,49
status: unverified
real data: never looked at (only Rust semantic spans)
sources: L/wave4/green/REPORT.md:303-312

### PH-11 · navigation/journey_specs.rs (the retired seven onboarding specs) is still in the tree and declared pub mod (navigation/mod.rs:7); JOURNEY-A says it "ran nothing and asserted no content" and was retired, but the file was never deleted
surface: harness/journeys/lints
problem: Dead metadata that misleads (ColdEmpty, PickerCancelled, Indexing, ReadyMultiProject, FailureRetry, PersistedRestart, McpSetup).
evidence: W6/journey/JOURNEY-A.md:88-96; apps/desktop/src/navigation/mod.rs:7
status: open cleanup
real data: N/A
sources: W6/journey/JOURNEY-A.md:88-96

### PH-12 · GUI is a separate workspace (memory): root-workspace green says nothing about lindsey; the lane files never mention that GUI workspace; the wave-6 suites are all root-workspace crates
surface: harness/journeys/lints
problem: Not a lane statement; noted so the lead does not read the suite counts as covering the separate GUI workspace.
evidence: MEMORY.md ("GUI is a separate workspace")
status: unverified for this GUI
real data: N/A
sources: MEMORY.md

### PH-13 · 42 modified and 2 untracked (runtime/releases.rs, docs/architecture/briefs/production-readiness.md) files in the working tree at snapshot time: milestone-3 work of F-Data and F-Shell (query/local.rs +193, view_build/structural.rs +163, harness/journey/look.rs +101, harness/install.rs +45, shell/acquire.rs, shell/ask.rs, shell/bodies/orbit.rs, package.rs, symbol/facts.rs, focus.rs, reader.rs, root.rs, side/view.rs, facet fluid.rs/measure.rs/flow.rs/float.rs/deps.rs/ticker.rs/rail.rs/compose.rs) is unreported and uncommitted
surface: harness/journeys/lints
problem: The most recent code has no lane statement of intent or verification.
evidence: git status at session start; M-final cross-file note (c)
status: unverified
real data: N/A
sources: M-final

### PH-14 · gui-plan section 3 item 10 budgets (page open <= 120 ms, search <= 50 ms, flight <= 1500 ms, p95 frame <= 8 ms in release) and the J1-J6 table are the binding definition; no lane recorded a release-mode journey
surface: performance
problem: See M-final M47.
evidence: W6/lead/briefs/W-Journey.md:24-25; PR:266
status: open
real data: never looked at in release
sources: W6/lead/briefs/W-Journey.md:24-25; PR:266

## SECTION 20: additional distinct items from wave4/5 checkpoints and briefs, added after the merge pass

### OLD-28 · cross-ecosystem cousins (npm, PyPI, Go, Java, .NET, C++ counterparts for 7 families) and "the same idea in another package" (page.js cousinsOf: Jaccard over public method names; conversion roads via recipes.rs/graph road.rs) exist only as hand-written prototype data or unwired facet logic
surface: package page
problem: Nothing from the other ecosystems is indexed on the machine; the "cousins" and "roads between two types" (docs.rs cannot do these) have no desktop surface.
evidence: L/wave4/browse/CHECKPOINT.md:96-99; L/wave4/docsrs/PARITY.md:125-131
status: unbuilt idea
real data: never looked at
sources: L/wave4/browse/CHECKPOINT.md:96-99; L/wave4/docsrs/PARITY.md:125-131

### OLD-29 · semver-slip notch and measured stability: semverSlip is false on every real diff (the honest-semver notch exists only in the legend), measured API churn exists only for toml and smallvec ("API diffs not measured yet" for 31 of 33 candidates), advisories checked for 92 of 1,189 lockfile crates, MSRV where undeclared, downloads only under Option
surface: releases/time travel/upgrade
problem: The stability and impact facts that made the marks board persuasive are prototype data; the desktop's replacement (runtime/releases.rs, untracked) has semver_slip: false and empty impact for every change (M-final M8).
evidence: L/wave5/marks/CHECKPOINT-2.md:141-152; L/wave4/marks/CHECKPOINT.md:100-150; L/wave4/browse/CHECKPOINT.md:84-95
status: still open (G14)
real data: FIXTURE
sources: L/wave4/marks/CHECKPOINT.md; M-final M8

### OLD-30 · license card: tree line ("1,189 packages: permissive throughout, plus MPL-2.0 in dwrote ..."), "adding it brings GPL-3.0 into a tree with no strong copyleft terms today", and the hollow "also" teeth for crates present at two versions (114 in the repo's lock, e.g. toml 0.8.23 + 1.1.5) need the project's tree; the package page has no route back to the referring project
surface: package page
problem: The owner's two "knows your tree" ideas were built on the board only (LicenseFacts.tree None on the desktop).
evidence: L/wave4/marks/CHECKPOINT.md:120-135; L/wave5/marks/CHECKPOINT-2.md:141-152
status: still open (OLD-17)
real data: never looked at
sources: L/wave4/marks/CHECKPOINT.md; L/wave5/marks/CHECKPOINT-2.md

### OLD-31 · marks rulings not confirmed built: deps on the marks line folded to one line with "and N more" (wrap under the comb only at <= 760; a package with 30 deps must never produce a paragraph of names), license hedge line "a plain-words summary, not legal advice", "assuming you take it under MIT" for OR licences with an unknown choice, purpose phrases falling back to top used items
surface: package page
problem: The dependency marks on the folio (marks/deps.rs, modified uncommitted) were never visually verified on a real many-dependency package (tokio, present) after the rewrite.
evidence: L/wave4/marks/RULINGS.md:5-20
status: unverified
real data: never looked at
sources: L/wave4/marks/RULINGS.md

### OLD-32 · adoption/Compare rules: a line changes "only in name" only when every substituted item has the same shape (params in words, output, receiver type); same-named items with different shape read "reads differently"; a substituted type is a rename only if its capabilities cover the uses; until shapes exist show "matched by name" never "only in name"; sticky column header must fully cover what scrolls under it
surface: Find/browse
problem: Compare (Held cards, facet/browse/compare.rs) exists; whether the desktop's Compare follows these rules with real data is unverified (the false equivalence was found on toml_edit vs toml).
evidence: L/wave4/browse/RULINGS.md:1-13
status: unverified
real data: fixture
sources: L/wave4/browse/RULINGS.md:1-13

### OLD-33 · Find lives at its own route nudox://find?q= with Ask's last row "all answers as a page"; measured churn lazy (last 12 releases when judged or compared); tree count "what builds for this host" with "and N for other platforms" on hover; role vocabulary derived not editable
surface: Find/browse
problem: Rulings for the browse implementation; the desktop Find, tree and roles exist in part (browse S1 tree, Find/Compare) but S2+ (judge card, role rename) is a handed-off brief with no lane.
evidence: L/wave4/browse/RULINGS.md:14-22; L/wave4/QUEUE.md (Handed off)
status: partly built (S1 Your tree, Find, Compare); S2+ not built
real data: fixture tree (the repo's own lockfile)
sources: L/wave4/browse/RULINGS.md; L/wave4/QUEUE.md

### OLD-34 · the first-card whisper "Value in hand" once per install, ruled a product note; the persisted hand (5 cards) restored with the order recomputed on load; held trait constrains the recipe
surface: hand/holds
problem: The whisper implementation looped (FEEL-D4) and was rewritten; whether the once-per-install flag persists across relaunch of the real app is not covered by J10 (settings) or any journey.
evidence: L/wave4/hand/RULINGS.md:5-13
status: unverified for real relaunch
real data: never looked at
sources: L/wave4/hand/RULINGS.md; FEEL.md:101-103

### OLD-35 · wave5 W-Motion S3 (semantic zoom), S4 and S5 ("the mark") were moved to W-Motion and never built; W-Seams' S7c shell wiring "one accessor short of connected"; F (hint mode) and S (peel) key caps must not show on the graph route (dead end #11: not built - the visible caps are per-row reveals in the symbol page body)
surface: motion/transitions
problem: The graph's semantic zoom and the reveal caps' route gating have no owner; hint mode and peel silently no-op on the graph.
evidence: L/wave4/shell/s15-w-seams-draft.md:192-208,269-274; L/wave5/QUEUE.md (W-Motion owns S3 and S5)
status: unverified (SYM6 rewrote the symbol body; graph key caps not re-checked)
real data: never looked at
sources: L/wave4/shell/s15-w-seams-draft.md:192-208,269-274

### OLD-36 · lead-reported keyboard defects in wave 4: Tab / J / Space on a settled symbol page ("investigated and disproved as a shell defect; flagged for confirmation against a settled bodies/symbol.rs") and "T inside the graph changed nothing" (fixed: tour_package falls back to the nearest territory); no-place links speak through the Notice on Enter (dead ends #14-#16)
surface: keys/focus
problem: The Tab/J/Space report was never re-confirmed on the rewritten symbol page in the real shell; RSym-K says R-Sym6 could not run keys either.
evidence: L/wave4/shell/s15-w-seams-draft.md:208-268
status: unverified for the simple symbol page; SYM6-B rig test covers stops and Enter only
real data: rig only
sources: L/wave4/shell/s15-w-seams-draft.md:208-268; RSym-K
