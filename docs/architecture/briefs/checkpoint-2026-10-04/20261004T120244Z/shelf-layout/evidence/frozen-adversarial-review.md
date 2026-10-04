# Frozen measured Shelf review

Read-only review by Sol GUI worker, 2026-10-04. Worktree `/private/tmp/nudox-shelf-measured-layout-20261004`, HEAD `c70bc0ec690dfad51c625177e5852c727c53985e`. Saved full.diff and freshly read `git diff` both SHA256 `31a651dad197132404916345713205ff1d8a945b14cfc326a07b668f62954af9`. No source edits, compiler, tests, native app, recorder or TCC actions. The frozen 81 Run19 captures describe old source f42191f/binary34ed; they cannot accept this patch.

## Actionable remaining issues

1. **P1: pruned restoration callback leaves a permanent scheduled latch.** side/mod.rs:182 sets `Restore::Measuring.scheduled=true`, and :1490 queues `on_next_frame`. GPUI window.rs:5167 discards callbacks from inert/unmounted owners without invoking their closures. Root root.rs:2521 wraps a departing drawer in `a11y_inert`; a fully closed drawer is omitted. Closing the drawer or mounting Ask/Add before delivery can discard the callback, retaining scheduled=true. Same-visit reopening cannot rearm it, while `restoring=true` suppresses ShelfScroll publication (:1543). Add a mounted restore→inert commit→discarded delivery→reopen oracle, and explicit bounded restoration ownership/cancellation/rearm.

2. **P2: deferred restoration can override newer same-visit input.** finish_restore (:190) unconditionally returns to its saved semantic anchor. walk/reveal (:920/:224) and native List wheel change scroll without retiring Restore::Measuring. Callback checks only VisitId. Deterministic sequence: queue_restore(A), update/paint, then reveal B or wheel, then finish_restore; the newer position is overwritten or restoration remains pending if A is before logical top. Preserve latest input through explicit restoration ownership invalidation. Test actual user-event supersession, not only direct successful completion.

3. **P2: sticky overlay uses the previous viewport during a resize frame.** visible_sticky_ancestors (:270) caps using ListState.viewport_bounds; side/view.rs:145 builds the overlay during render, before List::prepaint updates bounds. Old viewport400→new96, depth5, row32 allows a160px overlay in a96px viewport for that frame. The depth5 test paints only bare ListView and calculates the chain after paint; it cannot see the product overlay's old-bound calculation. Add a mounted actual-overlay shrink/zoom assertion. Persistent no-input failure is not established.

## Prior findings and meaningful coverage

- Tall unmeasured Note Back is source-addressed by semantic anchor plus measuring completion. `back_restores_the_same_row_past_an_unmeasured_tall_note` proves measured RowId20 and inset8 after replacing the whole list. It directly calls finish_restore, so callback lifecycle remains uncovered.
- Combined broad replacement+zoom is source-addressed. `broad_fold_and_zoom_preserve_the_measured_items_fraction` measures the old row and asserts index62 plus8/32→16/64 after fresh measurement. This is a meaningful geometry oracle.
- Depth5→depth1 reveal now recomputes a capped ancestor chain. Its test checks final target below final computed inset. It does not mount actual sticky controls or verify native focus/hitbox ownership.
- Focus continuation remains unproved: Shelf uses plain splice, which registers no GPUI ListItem focus handles, though rows track native focus. GPUI offscreen focused-row continuation requires splice_focusable handles. Add a mounted focus→wheel offscreen→keyboard return/reveal oracle; do not declare a product failure without that intended-policy discriminator.
- Probe qualification remains open: probe.rs:298 claims full reachable content extent, while :689 uses measured heights plus offscreen baseline hints. The splice fixture accurately proves estimated range growth; it does not prove complete measured extent. Distinguish estimated coverage from actual content measurement.
- Native guidance captures002–004 motivate the measured Note change. Old027 detached outline and039 compact-header persistence are separate acceptance scenarios, not fixed by these tests alone.

## Exact reviewed source SHA256

```text
16933213316572b7e876a914c2eedc740557920a18c99305f31d7b33493d5965 apps/desktop/src/navigation/presentation.rs
903b5048492aa85dab8a9f94046a0408547aa2d427e7057956b3c8d00a29ca47 apps/desktop/src/shell/side/mod.rs
1e78f4659515029cd821b57f003c252148102214a88c9eeaff9614fe7a1ac6d2 apps/desktop/src/shell/side/view.rs
a7e6eb7dbcca65e6e0d4cc6891bf23d2a6853c149f484e9b139597a52191d3d7 apps/desktop/src/shell/shelf_tests.rs
0112c846d3c8539c796465e2cda6cfc6fcade35853fe8a4a6601bcbfa63e6c3b apps/facet/src/probe.rs
5fad3838f95769200558b9d148c2b7b4440e220890c06d50473d318ddebe6991 vendor/gpui-ce/NUDOX-PATCHES.md
e2fa5952fcc7f054f64b92afc3d0edf493bc1aafd0f7ae5d21554cb5643f5106 vendor/gpui-ce/src/elements/list.rs
```

## Next native scenarios after exact new-build handoff

1. Library UsedBy/RestsOn guidance at390/360px and150/200%; inspect recovery suffix, following row and actual scroll reach.
2. Scroll beyond wrapped guidance, navigate away/Back/Forward, compare exact row/inset; immediately wheel or press Down during restoration.
3. Close/reopen drawer and open/close Ask/Add during restoration; verify later wheel positions persist across Back.
4. Deep subtree→shallow sibling upward keyboard reveal, then height shrink and200% zoom; inspect actual sticky controls, target focus and hitboxes.
5. Focus real row, wheel it offscreen, then navigate by keyboard; test hide/reveal without stale glow or focus transfer.

All newly added Rust/GPUI fixtures remain UNRUN here. Still images cannot establish animation quality.
