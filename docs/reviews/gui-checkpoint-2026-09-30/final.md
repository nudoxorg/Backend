# RAW M-final (lead read: .local/lanes/final/{FINISH,COORD,F-Data-1,F-Data-2,F-Shell-1}.md, docs/architecture/briefs/production-readiness.md, working-tree peek at runtime/releases.rs)
Base path L = /Users/mileswirht/Downloads/backend/.local/lanes/final. Only F-Data-1, F-Data-2, F-Shell-1 milestone files exist: there is NO F-Data-3.md and NO F-Shell-2.md/-3.md, and no final DoD checklist file. COORD's last entry is 00:14 UTC (2026-09-30).

### M1 | harness/journeys/lints | no-final-dod-checklist
kind: not-verified
ids: FINISH DoD 1-9
problem: FINISH demands a final milestone that states the nine definition-of-done items PASS-with-evidence or not-done-with-reason; F-Data stopped after milestone 2 and F-Shell after milestone 1, so no file states the final checklist and no lane certified "production-ready".
evidence: L/FINISH.md:22-37, 124-131; L/ has only F-Data-1.md, F-Data-2.md, F-Shell-1.md
status: still open; COORD.md:105-109 (00:14) is the last entry (J1, J9, J11 "run now", no results recorded)
real-data: UNVERIFIED as a whole
sources: L/FINISH.md:22-37,124-131; L/COORD.md:105-109

### M2 | harness/journeys/lints | J11-every-key-not-run
kind: not-verified
ids: J11, DoD 6
problem: J11 (every binding in the key table, generated from an exhaustive match over KeyCommand) was written and "builds" but F-Shell-1 says "Not run yet"; COORD 00:14 says it "runs now" without reporting a verdict or a mutation proof.
evidence: L/F-Shell-1.md:65; L/COORD.md:106
status: unresolved in my files (no J11 REPORT quoted anywhere); mutation proof (marker-guarded product mutant makes J11 fail) demanded by FINISH.md:85 never quoted
real-data: UNVERIFIED
sources: L/F-Shell-1.md:65,72-73; L/FINISH.md:83-85; L/COORD.md:106

### M3 | harness/journeys/lints | J12-crawler-not-written
kind: next
ids: J12, DoD 8
problem: J12 (the crawler: hover and click every published target to depth N, popover timing, no fault plate or empty reader, back with focus restored, at 320/390/760/1440/2560 in both themes) is not mentioned as written or run in any final file; F-Shell-1 lists it only under later milestones.
evidence: L/FINISH.md:31,83; L/F-Shell-1.md:72-73
status: still open (F-Shell milestone 3 never reported). NOTE git status shows apps/desktop/src/harness/journey/crawl.rs modified, uncommitted
real-data: UNVERIFIED
sources: L/FINISH.md:31,83-85; L/F-Shell-1.md:72-73

### M4 | harness/journeys/lints | journeys-J2-J8-J13-J14-unwritten
kind: next
ids: J2..J8, J13, J14
problem: "Every other journey is PASS or BLOCKED naming a gap": J2 find by shape, J3 upgrade, J4 map, J5 failure pages, J6/J14 weather, J7 add from browsing, J8 earlier release, J13 failure and retry were never reported written or run in any final file.
evidence: L/FINISH.md:36,83
status: still open; production-readiness.md:291 also lists them as "also to run"
real-data: UNVERIFIED
sources: L/FINISH.md:36,83; docs/architecture/briefs/production-readiness.md:291

### M5 | harness/journeys/lints | J0-J1-J9-final-verdicts-missing
kind: not-verified
ids: J0, J1, J9
problem: J0 under env -i reached 8 of 11 checkpoints PASS at 19:53 with verdict FAIL on four points; after F-Data's 20:16 fixes no file records a J0 or J1 or J9 verdict. J1/J9/J11 "run now on a harness built at 00:08"; no result recorded.
evidence: L/F-Shell-1.md:47; L/COORD.md:64-69,71-78,105-106; run dir L/shell/runs/J0-finder-190840/REPORT.txt
status: unresolved; only J10 has a recorded PASS (L/F-Shell-1.md:41)
real-data: REAL for the 19:53 partial run; UNVERIFIED for post-fix state
sources: L/F-Shell-1.md:47; L/COORD.md:64-69,105-106

### M6 | harness/journeys/lints | mutation-proofs-for-J0-J11-J12
kind: not-verified
ids: none
problem: FINISH requires, for each of J0, J11 and J12, a marker-guarded mutation of PRODUCT code that makes the journey fail with the REPORT line quoted; none is quoted for J0 (a journey), J11 or J12 in the final files (mutation proofs quoted are for unit tests: whisper, shelf swap, inert page, reflow, ink4).
evidence: L/FINISH.md:85; L/F-Shell-1.md:52-57
status: still open
real-data: N/A
sources: L/FINISH.md:83-85; L/F-Shell-1.md:52-57

### M7 | releases/time travel/upgrade | earlier-release-read-not-delivered
kind: next
ids: G13, G14, DoD 9
problem: Reading an earlier release (toml 0.5.11 vs pinned 0.8.23) for real from the cargo cache was DoD item 9; COORD 17:28 says "Releases (G13/G14): not yet; I will post runtime/releases.rs here when it lands (milestone 3)" and F-Data-2 does not mention it. Working tree has an UNTRACKED apps/desktop/src/runtime/releases.rs (98 lines: versions from the cargo index cache, diffs from SurfaceCommand::Diff between releases the library holds) with no lane checkpoint that verifies it.
evidence: L/COORD.md:14; L/FINISH.md:32,58; apps/desktop/src/runtime/releases.rs:1-98 (untracked per git status)
status: in flight, unverified; not posted in COORD
real-data: UNVERIFIED (never run in any capture I can find)
sources: L/FINISH.md:32,58; L/COORD.md:14

### M8 | releases/time travel/upgrade | releases-rs-fields-empty
kind: hypothesis
ids: none
problem: In the in-flight releases.rs the Change records carry before: None, after: None, semver_slip: false, aliases empty, uses empty, impact empty, and DeclarationChange::Indeterminate is dropped: so the package banner, symbol history, upgrade lens and sidebar row glyphs (G14) would have no signature diffs, no "your uses" impact and no semver-slip flag even once real.
evidence: apps/desktop/src/runtime/releases.rs (Change {.. before: None, after: None}, Crate {aliases: HashMap::new(), uses: Vec::new(), impact: Vec::new()})
status: unresolved; observation of uncommitted code, not a lane statement
real-data: UNVERIFIED
sources: apps/desktop/src/runtime/releases.rs; L/FINISH.md:58

### M9 | releases/time travel/upgrade | fixture_releases-still-feeds-product
kind: next
ids: G14
problem: Real release diffs must replace runtime/fixture_releases.rs (serves only prototype toml and smallvec) behind its existing API; as of the last commit the fixture still feeds banner, history, upgrade lens and sidebar glyphs.
evidence: L/FINISH.md:58; docs/architecture/briefs/production-readiness.md:90
status: still open at HEAD; releases.rs uncommitted
real-data: FIXTURE
sources: L/FINISH.md:58; production-readiness.md:90

### M10 | package page / Find | add-affordances-not-shown-on-real-data
kind: not-verified
ids: G10, G12, FINISH F-Shell item 7
problem: The Add buttons in Find and the package header (shell::acquire::add_actions, bodies/browse.rs:76, bodies/package.rs:135, page_offer) exist in code per COORD 17:28, but F-Shell-1 never mentions drawing or seeing them; no final PNG shows an Add button, an "adding" seam or a release picker on the real library.
evidence: L/COORD.md:13; L/FINISH.md:86; L/F-Shell-1.md (absent)
status: unresolved in my files
real-data: UNVERIFIED
sources: L/COORD.md:13; L/FINISH.md:86

### M11 | Find | find-cannot-show-crate-only-in-cargo-cache
kind: next
ids: G12
problem: Find merges only index and owner catalogs (runtime/reads.rs compose_find), so a crate present only in the local cargo cache cannot be found or added from Find.
evidence: production-readiness.md:126 (reads.rs:1126-1168)
status: still open per lead brief; no final file changes it
real-data: UNVERIFIED
sources: production-readiness.md:126; L/FINISH.md:58

### M12 | onboarding/install | toolchain-discovery-other-languages
kind: not-verified
ids: none
problem: Toolchain discovery for a Finder launch was implemented and proven only for Rust (rustc+cargo directory order: PATH, rustup, Homebrew, Nix); Go's module-cache root, TypeScript, Python and Clang were never run through a Finder-like desktop launch.
evidence: L/COORD.md:22; L/F-Data-1.md:21; production-readiness.md:59
status: still open
real-data: REAL for Rust only
sources: L/COORD.md:22; L/F-Data-1.md:21-22

### M13 | onboarding/install | finder-launch-real-Finder-not-tested
kind: not-verified
ids: none
problem: The "Finder launch" is emulated with env -i HOME PATH=/usr/bin:/bin (test child and journey machine); no one launched the built Nudox.app from Finder/launchd, where PATH, TMPDIR, USER and the working directory differ.
evidence: L/COORD.md:36 (env lists no TMPDIR, USER, CARGO_NET_OFFLINE); L/COORD.md:23
status: still open
real-data: FIXTURE-ish (emulated env on the real machine)
sources: L/COORD.md:23,36; L/F-Data-1.md:22

### M14 | onboarding/install | first-run-shows-dev-shell-only-in-tests
kind: hypothesis
ids: none
problem: F-Shell 18:34 noted the env -i journey lacks TMPDIR/USER/CARGO_NET_OFFLINE and F-Data's own test child "may differ in what it compiles (toml_pin has registry deps) or in env"; the mismatch that produced the 18:34 Toolchain refusal (selected Rustc, configured None) was never root-caused: F-Data "could not reproduce", it went away after a fresh journey home.
evidence: L/COORD.md:36,50
status: unresolved: attributed to a stale attached owner holding the workspace (not proven)
real-data: REAL (env -i) but cause unproven
sources: L/COORD.md:34-37,49-50,63

### M15 | onboarding/install | progress-line-contrast-cause-unknown
kind: hypothesis
ids: D-progress
problem: The progress line 'indexing toml 0.8.23 (1 of 17)' painted at 1.01:1 contrast; F-Data says "Why that line painted at 1.01:1 I could not tell from the code" and removed the element; the underlying cause (a nearly transparent draw while waiting) may recur for the new headline whenever it is drawn during a fade.
evidence: L/COORD.md:66,73; PNG L/shell/runs/J0-finder-190840/05-installed-toml-pin.png
status: element removed (COORD 20:16); no lint re-run recorded
real-data: REAL (found on real install), fix UNVERIFIED
sources: L/COORD.md:66,73; L/F-Data-2.md:14,53

### M16 | onboarding/install | two-totals-17-vs-12
kind: defect
ids: none
problem: The package page for toml_pin says '17 packages beneath - in your lock' (source_facts reads every Cargo.lock entry) while the Library says 'All 12 packages toml_pin uses'; the five cfg(any()) pins appear only in the first number.
evidence: L/COORD.md:109; PNG L/shell/runs/J1-223330/03-your-module.png
status: still open (COORD 00:14 is last word)
real-data: REAL
sources: L/COORD.md:67,73,109; L/F-Data-1.md:12

### M17 | motion | library-ring-chip-overlap-during-install
kind: defect
ids: none
problem: When the package list grows from 8 to 12 the Library orbit chip ring is caught mid-glide and chip labels overlap (hashbrown over indexmap, serde_core over serde_spanned) for that frame; F-Shell also found chips jump 1.1 px when a package is added.
evidence: L/COORD.md:60,69; PNG L/f-data/cap/g15f/...-t400@1x.png
status: F-Shell said "being fixed" (COORD.md:69, 19:54); no later file confirms (F-Shell-1 update block line 47 lists it as "mine, next")
real-data: REAL
sources: L/COORD.md:60,69; L/F-Data-1.md:38; L/F-Shell-1.md:47

### M18 | motion | settle-differs-from-fresh-in-library-region
kind: defect
ids: none
problem: In J0 the Library region differs between a settled state and a fresh boot ("settle == fresh differs in the Library region"), a check that failed at 19:53.
evidence: L/COORD.md:69; L/F-Shell-1.md:47
status: "mine, next"; not confirmed fixed in any file
real-data: REAL
sources: L/COORD.md:69; L/F-Shell-1.md:47

### M19 | motion | package-seam-fills-live-past-budget
kind: defect
ids: none
problem: J1 (real install) reported 'settle: package-seam-<toml_pin>-fill-0..7: still live at 96 ms; budget ended at 240 ms' on the Library after the install (progress seam fills published live with an ended budget).
evidence: L/COORD.md:81; L/F-Data-2.md:18
status: F-Data 21:55 removed the relaunch seam (no seam at relaunch); during a fresh install the seam still animates and was not re-judged; no file says J1 now settles clean
real-data: REAL
sources: L/COORD.md:81,90; L/F-Data-2.md:18,47-49

### M20 | keys/focus | continuity-check-and-modality-switch
kind: decision
ids: D6
problem: D6 (focus ring jumped 1.000 to 0.000 in 0 ms) was traced to the Add-a-folder button's keyboard ring going off when the pointer took over; F-Data made the dialog close onto no focused control, but "a ring that goes off because the pointer took over is still a one-frame change anywhere else; if the continuity check should treat a modality switch as a retarget, that is yours".
evidence: L/COORD.md:68,74
status: dialog case FIXED (F-Data-2.md:25, test shell/onboard/tests.rs:167); the general continuity-check decision is open
real-data: REAL for the dialog case
sources: L/COORD.md:68,74; L/F-Data-2.md:25

### M21 | Ask/search | search-latency-few-seconds-in-debug
kind: known-failing
ids: DoD 4
problem: A search over the whole library takes "a few seconds in a debug build (corpus built once per view, then reused)"; the GUI budget is search <= 50 ms and release-mode was never judged.
evidence: L/COORD.md:103; production-readiness.md:266
status: still open
real-data: REAL (toml_pin install), release UNVERIFIED
sources: L/COORD.md:103; production-readiness.md:266

### M22 | Ask/search | search-coverage-gaps-not-said-in-the-gui
kind: defect
ids: none
problem: After the 22:59 fix a search no longer fails when rows and evidence do not pair: unpaired rows are left out, "counted in the lane coverage, and said on the owner's stderr" (query/local.rs LeftOut) - the GUI does not say what a search did not cover, although F-Shell asked for exactly that ("says what is not").
evidence: L/COORD.md:85,101
status: still open (no GUI text for LeftOut recorded)
real-data: REAL
sources: L/COORD.md:85,101

### M23 | Ask/search | search-ranking-verified-only-for-one-query
kind: not-verified
ids: none
problem: Ranking (row score, whole-name word first, type before use, declaration before link) was verified for ⌘K "toml Value" (and Deserializer found) only; no other query classes.
evidence: L/COORD.md:100-102; PNG L/f-data/cap/m3b/...-t1400@1x.png
status: partially verified
real-data: REAL (one query)
sources: L/COORD.md:99-103

### M24 | Ask/search | ask-shows-fault-in-words
kind: next
ids: none
problem: Ask used to show an empty plate and forever "searching" when the owner refused; it now says the fault in words (shell/ask.rs), and the veiled page under Ask is exempt from contrast like a dialog (Ask publishes field and plate bounds) - the exemption is a lint carve-out and the raw protocol text ("typed semantic evidence does not exactly cover...") is a fault sentence, not product copy.
evidence: L/COORD.md:86; apps/desktop/src/shell/ask.rs (modified, uncommitted)
status: fixed as a fallback; wording unreviewed
real-data: REAL
sources: L/COORD.md:85-86

### M25 | data-feed | serde_core-and-hashbrown-thin-packages
kind: known-failing
ids: none
problem: On the real install two packages are 'thin' (compiler could not finish): serde_core 1.0.229 (src/de/mod.rs; lowering gap RustGenericParameter) and hashbrown 0.17.1 (src/alloc.rs, Fragment(Prepare)); their names come from source alone, so their pages/symbol pages lack semantic data, and a person reads "the compiler could not finish 2".
evidence: L/F-Data-1.md:11,36; PNG L/f-data/cap/g15e/...-t400@1x.png
status: still open (frontend gaps; production-readiness.md:139-143 also lists zod, pflag, tokio)
real-data: REAL
sources: L/F-Data-1.md:11,36; L/F-Data-2.md:49; production-readiness.md:138-144

### M26 | data-feed | tokio-1.53.1-nothing-to-serve
kind: known-failing
ids: none
problem: tokio 1.53.1 in the fixture has nothing to serve: the intent queue's byte bound refuses its index (queue builtin intent: Queue(Bytes)); predates wave 6.
evidence: L/F-Data-2.md:102 (paths: wave6/**/run.log)
status: not investigated
real-data: REAL
sources: L/F-Data-2.md:102

### M27 | symbol page | false-place-on-as_str-page
kind: defect
ids: none
problem: On the as_str symbol page 'IN YOUR WORKSPACE 1 place' points at src/map.rs:4 in toml's license comment (a URL fragment ".../LICENSE-2.0>"), marked calls with the underline on "E-2.0>": a name match that should not exist or a span on the wrong bytes.
evidence: L/f-data/cap/all3/desktop-value-as-str-abyss-100pct-t1500@1x.png; L/F-Data-2.md:101
status: not investigated
real-data: REAL
sources: L/F-Data-2.md:51,101

### M28 | sidebar | record-rust-sidebar-repeats-Datetime
kind: defect
ids: none
problem: The desktop-record-rust sidebar lists Datetime and Date many times, once per impl block.
evidence: L/F-Data-2.md:104
status: unresolved in my files (F-Shell-1 fixed module duplicates and use-imports, not impl-block repeats)
real-data: REAL
sources: L/F-Data-2.md:104; L/F-Shell-1.md:31

### M29 | sidebar | contents-count-two-sources
kind: defect
ids: none
problem: The sidebar's Contents shows 88 on toml's intro while the page says '31 public names' (index outline vs source facts: two sources).
evidence: L/F-Shell-1.md:37
status: not done (F-Shell-1)
real-data: REAL
sources: L/F-Shell-1.md:37; production-readiness.md:166

### M30 | sidebar | drawer-instance-no-keyboard
kind: next
ids: SIDE-C 6
problem: The sidebar's drawer instance (phone widths) has no keyboard.
evidence: L/F-Shell-1.md:37; production-readiness.md:171
status: still open
real-data: N/A
sources: L/F-Shell-1.md:37

### M31 | sidebar | registry-versions-drawn-two-versions-of-one-crate
kind: not-verified
ids: none
problem: PackageRef::release_version() is drawn beside the name in sidebar rows, ring chips and jump bar (COORD 21:04) - not seen in any post-change PNG for the chip ring at width or for a library with two versions of one crate (the Library from a single install has one version per crate).
evidence: L/COORD.md:72,82
status: drawn; verification limited to the toml_pin install captures (F-Data-2.md:35 "toml 0.8.23 in page headers")
real-data: REAL (single version each)
sources: L/COORD.md:72,82; L/F-Data-2.md:35

### M32 | Library | project-list-and-package-list-counts
kind: not-verified
ids: none
problem: Library Contents count (projects plus packages) and beside_your_projects (ring lists what projects use, not the project) were fixed on real data only in the 1440 view; not swept at 320-760.
evidence: L/F-Shell-1.md:32-33
status: fixed; width sweep unverified
real-data: REAL at one width
sources: L/F-Shell-1.md:32-33

### M33 | status/lifecycle | reads-wait-in-scan-and-publish-phases
kind: deferred
ids: G3, W-Owner
problem: The owner is one writer/one &mut daemon: reads that arrive during an index's scan/frontier commit (phase A) or publication (phase C) still wait; only the compile (B) runs beside reads. A read-only second handle on the last published snapshot "is a change to the engine's owner". An owner with a compiler cluster is not deferred at all.
evidence: L/F-Data-2.md:13,103; crates/local-service/src/builtin/commands/index.rs:426
status: deferred (F-Data-2 What's left 4)
real-data: REAL (0.36 s read while a 39 s compile ran)
sources: L/F-Data-2.md:8-13,103

### M34 | status/lifecycle | no-progress-inside-one-package-compile
kind: deferred
ids: W-Owner
problem: The owner reports nothing inside one package's compile; progress is per package on the window side only, so a 30-40 s compile shows no movement; the Library lags the worker by a few packages during an install.
evidence: L/F-Data-2.md:14,103; L/F-Data-1.md:37,68
status: deferred
real-data: REAL
sources: L/F-Data-1.md:37,68; L/F-Data-2.md:14,103

### M35 | status/lifecycle | first-install-takes-a-quarter-hour
kind: known-failing
ids: none
problem: A clean install of toml_pin + 12 packages took 826-1032 s wall (load 20-26) and 636 s (load 18); F-Data-1 note "A first install takes a few minutes" (the copy in the Library) is not what was measured.
evidence: L/F-Data-1.md:36,38; L/F-Data-2.md:46; copy at COORD 17:40
status: still open (timing under machine load; no unloaded clean-install time recorded)
real-data: REAL
sources: L/F-Data-1.md:36,38; L/F-Data-2.md:46; L/COORD.md:25

### M36 | status/lifecycle | real-window-resize-reopen-not-seen
kind: not-verified
ids: G5
problem: Window size persistence was verified by desktop-state.json (window {1100,760}) and a unit test; a harness relaunch captures at the scene size, so the reopened window size was never seen in a picture.
evidence: L/F-Data-2.md:22
status: partially verified
real-data: REAL (state file), reopened size UNVERIFIED visually
sources: L/F-Data-2.md:22

### M37 | status/lifecycle | owner-start-flake-clang-sysroot-5s
kind: defect
ids: none
problem: Three real-owner tests failed at owner start with 'cannot establish selected Clang driver authority ... exceeded the 5s limit during sysroot' under load (351 s run vs 150 s); a 5 s probe failing the whole owner start is a flake source, and would refuse a real user's owner start on a slow/loaded Mac.
evidence: L/COORD.md:108
status: open; "yours to judge (crates/**)"; passes one at a time
real-data: REAL
sources: L/COORD.md:108

### M38 | status/lifecycle | R7-owner-failed-foot-verified
kind: not-verified
ids: R7, G7
problem: 'The index could not start. ... Try again' now shows in the status foot on every route (runtime/store.rs::owner_failed) - proven by lifecycle_tests with a fake failure; never seen with a real owner start failure in a PNG.
evidence: L/COORD.md:76; L/F-Data-2.md:21
status: fixed (test-proven)
real-data: FIXTURE (injected failure)
sources: L/COORD.md:76; L/F-Data-2.md:21

### M39 | status/lifecycle | D2-owner-watch
kind: defect
ids: D2
problem: runtime/owner.rs::watch held root and store strongly (leaked handles at quit); now weak and stops when either is gone.
evidence: L/F-Data-2.md:23
status: FIXED (F-Data-2.md:23, test host/lifecycle_tests.rs:352 mutation M14)
real-data: FIXTURE (test)
sources: L/F-Data-2.md:23; L/FINISH.md:57

### M40 | harness/journeys/lints | D3-button-labels-not-probe-texts
kind: next
ids: D3, L2
problem: Button labels are not probe texts so contrast/clip/overlap lints skip them; and documented shortcuts nothing binds (L2, navigation/action.rs:204-225: cmd-shift-p, cmd-b, cmd-n, cmd-shift-a, cmd-shift-y, cmd-shift-/, cmd-shift-m, cmd-left/right, cmd-0) are FINISH F-Shell item 8; F-Shell-1 does not report either.
evidence: L/FINISH.md:87; production-readiness.md:176,192-194
status: still open (no final file marks D3 or L2 done)
real-data: N/A
sources: L/FINISH.md:87; L/F-Shell-1.md (absent)

### M41 | keys/focus | J11-from-table-catches-unbound-keys
kind: hypothesis
ids: L2
problem: J11 is generated from the key table; keys that are only "documented metadata" in navigation/action.rs (L2) will either be pressed and do nothing or be absent; nobody said which. cmd-0 collides with ZoomReset.
evidence: L/F-Shell-1.md:65; production-readiness.md:176
status: unresolved
real-data: UNVERIFIED
sources: L/F-Shell-1.md:65

### M42 | overlays/popovers | add-dialog-lints-only-dialog
kind: defect
ids: D4
problem: D4 (add-folder dialog scrim not recognised; 13 false contrast lints) fixed - the dialog goes through facet::overlay::dialog and publishes a probe::veil; J0 run J0-finder-190840 'lints: none (5 texts, 4 targets linted)'.
evidence: L/F-Data-2.md:24; PNG L/shell/runs/J0-finder-190840/02-add-dialog.png
status: FIXED, verified on the real journey
real-data: REAL
sources: L/F-Data-2.md:24,52

### M43 | contrast/a11y | D1-type-to-narrow-fixed
kind: defect
ids: D1
problem: "Type to narrow" et al. set in ink4 (2.59:1); all sidebar words moved to ink3; guard test fit_tests::no_words_in_the_shell_are_set_in_ink4 scans src/shell; J10 Library frames pass zero-lint in both themes.
evidence: L/F-Shell-1.md:30
status: FIXED (F-Shell-1.md:30)
real-data: REAL (J10 on real owner)
sources: L/F-Shell-1.md:30

### M44 | keys/focus | D5-esc-closes-settings-fixed
kind: defect
ids: D5
problem: Esc does not close Settings and the relaunch reopens Settings: fixed, rig test plus J10 PASS on the real owner.
evidence: L/F-Shell-1.md:41; run L/shell/runs/J10-174347/REPORT.txt
status: FIXED (verified)
real-data: REAL
sources: L/F-Shell-1.md:39-43

### M45 | motion | j10-found-two-defects
kind: defect
ids: none
problem: J10 found (1) the headless display had a new UUID per launch so per-display zoom was lost on relaunch in the harness; (2) the reader published plate edges as springs with target=value velocity 0, so every transition failed continuity/overshoot/lockstep (45 findings per journey); both fixed (vendor display.rs; reader.rs Course).
evidence: L/F-Shell-1.md:42
status: FIXED for J10; whether the other journeys' motion is now clean is unreported
real-data: REAL
sources: L/F-Shell-1.md:42

### M46 | harness/journeys/lints | vendor-scheduler-patch-uncommitted-then-committed
kind: defect
ids: none
problem: gpui headless TestScheduler took two locks in opposite orders (deadlock, 0 % CPU hang); fix is a vendored gpui_ce_scheduler 0.2.2 with a [patch.crates-io]; the 8766d6460 commit missed the Cargo.toml line and vendor/ dir; HEAD 525355f68 'fix(vendor)' appears to carry it.
evidence: L/COORD.md:80,96; git log 525355f68
status: FIXED per git log (not verified by me by build); harness-only, product scheduler untouched
real-data: N/A
sources: L/COORD.md:80,96

### M47 | performance | release-mode-budgets
kind: not-verified
ids: gui-plan 3.10
problem: Release-mode budgets (page open <= 120 ms, search <= 50 ms, flight <= 1500 ms, p95 frame <= 8 ms at 1440x900) were never judged in any final run; all final measurements are debug builds under load.
evidence: production-readiness.md:266; L/COORD.md:103
status: still open
real-data: UNVERIFIED
sources: production-readiness.md:266; L/COORD.md:103

### M48 | harness/journeys/lints | suite-green-not-final
kind: known-failing
ids: none
problem: Last suite counts in final files: desktop lib 542 passed 0 failed at 21:50 (F-Data-2.md:92) while F-Shell at the same time reported 538 passed 4 failed (COORD 22:07); 00:01 three real-owner tests failed under load; the git working tree has ~40 uncommitted modified files; no run of all listed suites on one final tree is recorded.
evidence: L/COORD.md:97,108; L/F-Data-2.md:92
status: unresolved
real-data: N/A
sources: L/COORD.md:97,108; L/F-Data-2.md:92

### M49 | harness/journeys/lints | 25-scene-capture-all-not-visually-reviewed
kind: not-verified
ids: none
problem: capture --scene all now runs all 25 scenes (exit 0, 9 min, warm fixture state); only desktop-record-rust and desktop-value-as-str were opened and read; no one reviewed the other 23 PNGs at that run or the lint result of capture --scene all.
evidence: L/F-Data-2.md:27,50-51; dir L/f-data/cap/all3/
status: partially verified
real-data: FIXTURE (warm fixture state)
sources: L/F-Data-2.md:27-29,50-51; L/COORD.md:89

### M50 | data-feed | multi-module-real-crate-pinned-by-test-only
kind: not-verified
ids: none
problem: The 'present' package page read '155 public names in 1 module' (no module path on semantic rows); after the fix it is pinned by owner tests (toml 0.8.23, toml_datetime) - the package page's module structure on the real install was not re-shown for a many-module crate (present has ~30 modules) in a final PNG.
evidence: L/F-Data-1.md:25-27; L/FINISH.md:51
status: fixed in data (rows carry file); the page's behaviour when names carry no module ("say so, don't draw one lib block") is F-Shell's item 5 - not reported
real-data: REAL for owner rows; page UNVERIFIED
sources: L/F-Data-1.md:25-27; L/FINISH.md:51,80

### M51 | package page | lib-block-when-no-module-and-heads-up-tile
kind: next
ids: FOLIO-C
problem: F-Shell item 5: when names carry no module, say so rather than draw one 'lib' block as truth; fix the empty HEADS-UP tile; finish FOLIO-C. F-Shell-1 lists these under "Left for milestone 2" and no milestone-2 file exists.
evidence: L/FINISH.md:80; L/F-Shell-1.md:73
status: still open
real-data: UNVERIFIED
sources: L/FINISH.md:80; L/F-Shell-1.md:73

### M52 | symbol page | SYM6-B-not-finished
kind: next
ids: SYM6-B
problem: F-Shell item 5 'symbol page: finish SYM6-B' - listed left for milestone 2, no report.
evidence: L/FINISH.md:81; L/F-Shell-1.md:73
status: still open (unless a wave6 file says otherwise; see other agents)
real-data: UNVERIFIED
sources: L/FINISH.md:81; L/F-Shell-1.md:73

### M53 | fluid/phone widths | FLUID-C-real-content-sweeps-not-done
kind: next
ids: FLUID-C
problem: F-Shell item 5: 'finish FLUID-C (real-content sweeps 320-2560, no cliffs)' - left for milestone 2; never reported. DoD 8 (no crash/fault plate/empty reader at 320/390/760/1440/2560 in both themes on any page reachable from the Library) was never demonstrated on the real 13-package library.
evidence: L/FINISH.md:31,82; L/F-Shell-1.md:73
status: still open
real-data: UNVERIFIED on the real install (sweeps were on fixtures)
sources: L/FINISH.md:31,82; L/F-Shell-1.md:73

### M54 | sidebar | sidebar-peek-on-float-layer-lens-chords
kind: not-verified
ids: SIDE-A..C
problem: FINISH sidebar list included 'Scope, lenses and chords, narrowing (N of M), and the peek on the float layer'; F-Shell-1 evidences narrowing (25 of 665), duplicates, toml_pin once, Keys list; it does not report lens chords (G C/V/R/U, G G), scope switching or the sidebar peek on real data.
evidence: L/FINISH.md:77; L/F-Shell-1.md:29-37
status: partially verified
real-data: REAL for narrowing/modules; lens/chord/peek UNVERIFIED
sources: L/FINISH.md:73-77; L/F-Shell-1.md:29-37

### M55 | data-feed | 11-vs-13-refused-package-appears-on-same-boot
kind: not-verified
ids: INDEX 11 vs 13
problem: FINISH F-Data item 3 (a refused package appears in the Library on the same boot with its refusal named, ~8-line republish in CommandAdapter::add): not mentioned as done in F-Data-1 or F-Data-2 (they cover refusals of dependency packages only).
evidence: L/FINISH.md:54; L/F-Data-1.md, L/F-Data-2.md (absent)
status: unresolved in my files
real-data: UNVERIFIED
sources: L/FINISH.md:54

### M56 | data-feed | view-dto-version-set-aside
kind: defect
ids: none
problem: 'unsupported view DTO version' (view.journal from an older wire) was refused whole; now a set-aside (embedded_host.rs:64 journal_refusal, view_journal.rs:179 written_by_another_build) with a unit test.
evidence: L/F-Data-2.md:26
status: FIXED (test); never seen as a GUI state (what the person sees when their view journal is set aside is not described)
real-data: FIXTURE
sources: L/F-Data-2.md:26

### M57 | onboarding/install | try-again-on-refused-project-and-refusal-persist
kind: not-verified
ids: none
problem: Refusal words for a package the owner lists but could not compile are persisted (data/registry-sources/refusals.json) and shown on relaunch; a project refusal card exists; nothing describes what a person can DO about a thin package (retry, report) beyond reading the reason.
evidence: L/F-Data-2.md:17
status: fixed for display
real-data: REAL
sources: L/F-Data-2.md:17-18

### M58 | typing | display-name-cached-per-tree
kind: hypothesis
ids: none
problem: PackageRef::display_name() reads a registry package's name from its tree's Cargo.toml (cached per tree); an unreadable or renamed manifest, a git dependency, or a workspace member with version.workspace not tested.
evidence: L/F-Data-2.md:35; L/COORD.md:72
status: unresolved
real-data: REAL for the 12 toml_pin packages only
sources: L/F-Data-2.md:35

### M59 | data-feed | cargo-metadata-needs-rustc-beside-cargo
kind: defect
ids: none
problem: Under env -i the owner's cargo metadata could not find rustc and fell back to the bare lockfile (17 packages); fixed by giving cargo the rustc beside it (builtin/browse.rs); the fallback path (lockfile only) still exists, so when metadata fails the count silently changes.
evidence: L/COORD.md:73; L/F-Data-2.md:14
status: FIXED for the Finder env; fallback behaviour not surfaced
real-data: REAL
sources: L/COORD.md:73; L/F-Data-2.md:14

### M60 | data-feed | is_older_authority-cursor-fix-core
kind: decision
ids: none
problem: F-Data edited core/ids.rs (VersionedRoot::is_older_authority compared cursors by derived Ord so about half of later root reads were dropped as stale); core/ had no owner in FINISH.md; the same class (ordering by hash before sequence) may exist for other cursors (actor newest-basis filter fixed the same way; others not audited).
evidence: L/F-Data-1.md:17; L/COORD.md:41
status: fixed for two sites; audit of other Cursor comparisons not recorded
real-data: REAL
sources: L/F-Data-1.md:17; L/COORD.md:41

### M61 | data-feed | store-twice-linked-object-crash-reopen
kind: defect
ids: none
problem: A process killed between linking an object into objects/ and unlinking its stage left it twice-linked; every read refused UnsafePath; FileStore::open now reaps dead sessions (intermittent red).
evidence: L/F-Data-2.md:37-43
status: FIXED (M20 mutation), desktop crash test 4 of 4
real-data: REAL (kill -9 test)
sources: L/F-Data-2.md:37-43

### M62 | onboarding/install | crate-without-edition-refused
kind: defect
ids: none
problem: Crates without package.edition (equivalent 1.0.2, many old crates) were refused whole; now Cargo's 2015 rule.
evidence: L/F-Data-1.md:15; L/COORD.md:43
status: FIXED
real-data: REAL
sources: L/F-Data-1.md:15

### M63 | data-feed | doc-link-to-std-had-no-row
kind: defect
ids: none
problem: A doc link to std ([`f64::NAN`] in toml_write) had no view row, so the owner wrote a view journal it could not reopen and a relaunch after installing toml_pin refused to start (fault plate); fixed with an external row. Other link kinds (to other crates not in the library, macros, intra-doc paths) not enumerated.
evidence: L/F-Data-1.md:16; PNG L/f-data/cap/g15b/...-t100@1x.png
status: FIXED for doc links to external declarations; general class unverified
real-data: REAL
sources: L/F-Data-1.md:16; L/COORD.md:42

### M64 | onboarding/install | cargo-cache-offline-only-no-network-adapter-decision
kind: decision
ids: G11
problem: The registry source is offline (~/.cargo/registry/{src,cache}); "a network adapter may exist but never runs". The product decision (when may the app download a crate, and how is consent shown) is undecided; "a dependency not on this machine is named, never silently skipped" (hashbrown 0.17.1: needs a download).
evidence: L/FINISH.md:49; L/COORD.md:12; production-readiness.md:119,361
status: owner decision open
real-data: REAL
sources: L/FINISH.md:49; L/COORD.md:12; production-readiness.md:119

### M65 | overlays/popovers | R-J-motion-in-the-toml-install-frames
kind: not-verified
ids: none
problem: Motion (overlap, jump, continuity) on install-phase frames was judged only by lints on the last frames of a few captures; the film of the whole install (arrival of 12 chips, headline changes, seam steps) was never watched.
evidence: L/F-Data-1.md:35-39
status: unresolved
real-data: REAL captures, motion unjudged
sources: L/F-Data-1.md:35-39; L/COORD.md:60

### M66 | Library | ring-lists-what-projects-use-but-no-hover
kind: decision
ids: FEEL D9
problem: The Library shows no hover popover on package words or shelf rows; the ring now lists dependencies of the project; whether Library chips need popovers is a design call.
evidence: production-readiness.md:189; L/F-Shell-1.md:32
status: owner decision open
real-data: N/A
sources: production-readiness.md:189

## Cross-file notes
(a) Duplicates likely in other agents' files: G15/G10-G14 (acquire, journey GAPS), D1-D6 (GAPS.md), R7, W-Owner progress, sidebar SIDE-A..C, FOLIO-C, SYM6-B, FLUID-C, J-series journeys.
(b) Id schemes: G1-G15 product gaps, L1-L2 lint/limits, D1-D6 dev defects in GAPS.md; F-Data mutations M1..M24 in F-Data-1/2; FEEL D1..D17; R-Fit C1..C10; R-Folio D1..D14; R-Sym6 D1..D10; R-Open3 D1..D6.
(c) Working tree at snapshot: 42 files modified uncommitted incl. crates/local-service view_build/structural.rs (+163), query/local.rs (+193), harness/journey/look.rs (+101), harness/install.rs (+45), runtime/releases.rs untracked: i.e. milestone-3 work of both finishers is in flight and unreported.
