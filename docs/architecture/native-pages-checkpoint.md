# Native symbol and browse pages — integration checkpoint

Implementation: `/Users/mileswirht/Downloads/backend`, branch `canonical`, merge commit `4ba9034e6`. The separate `/Users/mileswirht/Downloads/backend-symbol-browse` worktree was removed after the merge. Original graph integration was separately audited in `/private/tmp/w-pages-codex/graph-integration-audit.md`; its newer shell, graph, hand and marks changes were preserved through the merge. The inherited dirty baseline is recorded in `/private/tmp/w-pages-codex/baseline/manifest.json`.

## Native startup incident

Recorded review-app crashes were GCD worker stack overflows during fixture recovery. Preparation now runs on a named 16 MiB thread, with asynchronous progress/completion and a deferred GPUI handoff. Four startup regressions passed. Two build9 launches and one build12 launch survived; build9's second launch and build12 displayed Value automatically without input or resizing. The launcher and harness are ARM64. No repaired launch requested Rosetta; the original prompt's cause remains unconfirmed.

A separate native idle sample found a continuously polling backend owner. The listener now sleeps between empty polls while preserving connected-client liveness. Deliberately restoring the old branch caused 1,784,456 owner polls in 600 ms and failed the regression. A single Bash trap restored and touched the source; two identical restored runs passed 1/1. Build12 observed 0.41 CPU seconds over 12 wall seconds and 0.34 over 11 (~3% of one CPU for the whole debug app); 766/773 sampled owner stacks were sleeping and the main thread waited in AppKit. This confirms the hot loop is gone in this scenario, not a release performance pass.

## Implemented native direction

Code is a separate destination. Semantic pages use typed callable projections, progressive disclosure, bounded per-symbol reader state, and cached immutable world/page readings. Recipe connectors align with their actual inputs and outcomes; authored prose uses shared inline formatting. Unsupported syntax is presented conservatively rather than inventing semantics.

Find uses immutable worker-prepared models, a persistent input entity, a generation-guarded debounce, explicit coverage, and disambiguating source locations. Compare uses distinct exact package identities, bounded name/overload views, and distinguishes recorded, proven absent, partial, and unread evidence. Name alignment does not imply compatibility. Package pages use a recorded outline instead of prototype-ranked entry recommendations.

Build14 includes: complete-outline exact symbol navigation (ranked top-K search cannot prove uniqueness), held comparison choices surviving Back, bounded relations pagination, occurrence-specific relation targets, and authored teaser/reference/singular-copy repairs.

## Evidence so far

Build12 default-dev harness SHA-256: `fb2b9bea986587a837e45fde98b704e2c6e5f336c248415a0854960c6017eed3`.

- Wide capture produced 15/16 scenes. Root inspected package, Find home/query/empty, Compare, Tree, Value, from_str, Serialize and separate Error. Core default structures are provisionally accepted; responsive/motion evidence remains open.
- Error-only capture passed but Error failed again in the all-scene batch. That inconsistency exposed the ranked-search resolver defect; the standalone pass is not accepted as a complete result.
- Native CUA: Ask opened, eight quick answers and All answers footer worked, Find held two exact packages, Compare loaded, and immediate replacement query plus Enter settled correctly. Build12 Compare→Back lost the held tray. Build14 native replay now preserves both choices, and removing one restores the correct disabled one-package state.
- Native Code opened separately but could not recover Value's shed registry source. The source-identity helper passed, but native build14 replay still failed. The adapter wrongly required a package URL where the fixture admitted a canonical local project directory. The corrected admitted-project-record lookup awaits build15 and native replay.
- Read-only Luna audit found pinned/synthetic exact-value fixtures, bounded presentation caches (3 worlds/48 packets), and family LRU bounds. It found the missing type-specific MadeBy regression and duplicate maker foot, both assigned for repair and mutation proof.

## Remaining acceptance gates

The final integrated source compiles and its desktop library suite passed twice. Native replay now covers held→Compare→Back, current-query Enter, and separate Code. Late symbol completion, the complete disclosure accessibility journey, and the remaining responsive/motion cases still need a final pass. Run two consecutive release perf runs at each target size with actual input scripts and a memory soak that discards frame history.

Debug timings under system contention cannot satisfy release gates. The production host still performs blocking initialization before entering its application event loop; that is separate from the repaired review harness. Fixture anatomy remains a documented enhancement, not a compiler authority for every language. Later browse shape-search, adoption, compatibility, and release-judgment capabilities are not claimed as shipped by this work.

## Latest native session

Build14 opened and mounted Value automatically without injected input, resize, or Rosetta. It stayed responsive through Page/Code, Ask, Find, hold, Compare, Back, and removal. Root closed the app and verified process exit. The initial page appeared scrolled mid-body; no intentional startup scroll exists in the scene or Reader, and this remains an unconfirmed anomaly pending the next launch. Cold debug fixture recovery is still expensive and is not a production startup benchmark.

## Review follow-up

A code audit confirmed offscreen keyboard selection in Find and Compare. The shared fix is under native mounted testing; no acceptance is claimed until selected-row bounds are verified after repeated Down/Up traversal. The timed disclosure film uses a probe-resolved control center and reverses at 40 ms without settling between clicks.

## Enlarged-text review

Build14 six-scene sheet at logical 1440×900, Abyss, 200% text, 1× raster, t=1600ms and 16ms frame cadence: `/private/tmp/w-pages-codex/captures/build14-1440-abyss-200/six-scene.png` (RGBA SHA-256 `245089b21737868a3ba0e4e6a062d6d054c8c6bfb6317b69cf6fcade2df465f9`). Root and both Sol GUI reviewers inspected the actual artifact. Value/from_str/Error heroes and authored prose wrap cleanly; operation input/outcome/failure paths remain legible. Find and package outline have no visible clipping in the first viewport. Compare stacks its three package heads at this effective width; the matrix continues below the viewport. This is a first-viewport layout pass, not proof of all scrolled states or performance. Error succeeded in the combined batch. The initial native scroll anomaly did not appear in these captures.

Focused source tests passed 1/1 for admitted local project root, 1/1 for exact confined excerpt recovery, and 1/1 for coverage page combination. Native source replay remains required.

Build14 760×900 Glacier 100% six-scene sheet: `/private/tmp/w-pages-codex/captures/build14-760-glacier-100/six-scene.png`, RGBA SHA-256 `6b73e896f4303c82f3d4b348ac6a1a789b240302a7517b8695acca25d21c9d38`. Static capture uses an 80ms sampling cadence at t=1600ms; this is not motion evidence. Root inspected the actual image: symbol ports, relation rows, wrapped Error prose, Find inline inspector, package outline and dependencies remain legible without apparent collisions. Compare remains usable, but stacked headers spend excessive vertical space; a bounded responsive header improvement is pending.

## Final repair and verification

Find and Compare now use one GPUI keyboard-reveal helper that measures the selected row in prepaint and moves only the necessary scroll distance. Review caught and removed a double scroll-offset application. Mounted tests walk Down and Up across an initially offscreen list and assert painted selected-row bounds inside the real viewport; both pass. The facet Browse suite passes 13 tests. Compare owns a focus handle on active page entry and does not claim focus while an overlay is open; full-shell native replay is still required. Compact Compare headers use actual measured gutters to choose parallel columns or a wrapped 2+2 layout.

Glacier tertiary ink changes from `#66758f` to `#555d79`, raising contrast from 3.68:1 to 5.14:1 on its darkest base ground and retaining a stronger ink2. Abyss is unchanged. A palette regression is added.

The final page build passed, the facet Browse suite passed 13/13, and the page-worktree desktop library suite passed 219/219. The merged source passed its full desktop suite **twice in succession, 232/232 each time**. Two initially failing desktop assertions had described the old raw-symbol and library pages; the test fixture for the tree now uses pinned `frontends/rust/fixtures/toml_pin`. A deliberate MadeBy type mutation failed as intended, was restored and touched by the same trapped Bash command, then passed in two consecutive identical runs. Compare's isolated mounted tests pass 9/9, including painted keyboard reveal and the measured two-header row.

The browse-cache regression separately plants a sentinel in a synthetic pinned Cargo project. It proves an untouched project is served from cache, a lockfile-only edit invalidates it, and a manifest-only edit invalidates it. Removing `Cargo.lock` from the cache witness inside one trapped Bash command failed specifically at the lockfile-only assertion. The trap restored and touched the source; two consecutive restored runs passed 1/1 each.

Build15 native CUA opened a real registry source in the separate Code destination from a semantic page, and traversed Find, held packages, Compare and Back without the earlier tray loss. The actual shell review also found two Compare defects that isolated component tests missed: the two package headers wrapped at 1440 logical pixels, and shell traversal consumed arrows before Compare. Build16 gives the header row a definite measured width with rounding slack; its fresh 1440×900 native GPUI capture shows all three candidate headers in one row (RGBA SHA-256 `76d653e2dcdf6e122adc8fdd0df5a98b73aa863f55b8e19b0c9449b07f4e2ce7`). Compare now owns a focus context for arrow/J/K traversal, leaving Ask available. A merged-source ARM64 native replay opened Value, searched `toml`, held exact `toml-0.8.23` and `toml_edit-0.22.27`, rendered both Compare headers in parallel, moved selection from `add_key` to `as_array` by Down, and returned to Find with both holds intact. The final shell binding regression passed in the integrated 232-test desktop suite twice.

Direct 1440×900 comparisons against the supplied stills are saved as [Value](figures/native-pages/value-target-side-by-side.png) and [Compare](figures/native-pages/compare-target-side-by-side.png). The native Value view preserves the readable semantic structure while moving raw source to Code, as requested. The native Compare view groups recorded declaration names and per-package inputs/exits; the prototype's usage counts, replacement costs and equivalence claims require additional compiler evidence and are deliberately not copied from the web mockup.

The Value Uses disclosure was driven through the real fixture at 1440×900, with a control hitbox resolved by a journey rather than guessed coordinates. The 40 ms open→reverse film is `/private/tmp/w-pages-codex/films/symbol-reversal/desktop-value-abyss-100pct-in48d8d7b8@1x-film.png` (decoded RGBA SHA-256 `723e275aa0c66c6c300488c9670f29450f82057760352ecad769dea1b4a0bcfe`); its t=1880 and later frames equal the pre-open t=1760 frame exactly. The single-open journey motion report has two segments, continuity worst 0.65 of budget, overshoot 0 failures, and zero requested/ambient frames after idle. The full journey verdict remains **FAIL** because its accessibility lint reports 25 issues, including two 16 px title-bar segment hit targets and offscreen focusables whose scroll reach the checker could not establish. The film is transition evidence, not accessibility acceptance. Debug draw p95 in that journey was 102.80 ms under fixture work and is not a release perf pass.

Build16's review app had stalled on two retries while its launcher used the removed worktree as its current directory. Sampling pinned the main thread in CoreText font registration (`fonts::install` → `add_fonts` → `CTFontCreateWithGraphicsFont` → `getcwd`) at 0% CPU. Changing only the temporary launcher directory to `/private/tmp` made a window open in about six seconds; that old binary then correctly reported its compile-time repository root was missing. Replacing it with the merged-source ARM64 harness mounted the real Value page after cold fixture indexing (about six minutes), and the Find/Compare/Back replay above completed without Rosetta or a native hang. This strongly implicates the invalid launch directory in the two stalls, but does not prove every production startup path. The integrated source passed a fresh `cargo check -p backend-desktop --features visual-harness` and the two full desktop test runs above. The review window was closed and its process exited.

Release page perf at 1440×900 and 2560×1440, a memory soak, two identical post-restore release runs, and a clean accessibility journey remain open. The graph's earlier release evidence is separate and cannot satisfy those page gates. Until these run, this checkpoint does not claim full production acceptance.
