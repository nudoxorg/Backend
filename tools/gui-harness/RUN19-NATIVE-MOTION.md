# Run19 native motion capture brief (prepared; no capture executed)

The recorder checkout for these plans is an isolated source branch from `f6ea929ddb2ca8d0eb7fe306049ca644e002d0fa`. It adds honest `find` and `drawer` flow names, `settle` and `retarget` transitions, native paired mouse Down/drag/Up, and a typed admission path for the real Run19 build receipt. The original `f6ea929` checkout and producing receipt are untouched. The new matrix rows are independent of the original 16 and do not convert compositor pixels into live-owner proof.

## Admission before any native action

The producing receipt is `/private/tmp/nudox-native-candidate-f421-run19/build-receipt.json` (SHA-256 `dd5ab12f803726f7ff8b02a1c3e5aaa7a5ea2184b18b074582641cccae8327f9`). Its actual schema is `kind=desktop-bins-build`, not the legacy native-motion fixture. The frozen capture source is `/private/tmp/nudox-native-candidate-source-f42191`, HEAD `f42191f4ec7f4ed0c2eb63a5048351010a0b5059`, tree `5889e34cf83200a767a882838859d5625ab54a46`, Cargo.lock SHA-256 `2ab762dd5ecef4b5612aae3bab065578d1f4103e4c680acc62da7589043110c0`. The copied, regular, single-link executable is `/private/tmp/nudox-gui-user-audit-20261003/current-run19/NudoxAuditRun19.app/Contents/MacOS/Nudox`, SHA-256 `34ed2485a86e10d0e7ab29842902cac0652e40f70e3bd958014540bb5d7e20a0`; its Info.plist SHA-256 is `53b669f869cdd1a9cd57660b069bdda53af403fce01c3a95e7624e7984d7189a`, bundle ID `dev.nudox.audit.run19`. The root canary receipt is independently checked as an input to typed admission. These are historical read-only observations, not authorization to drive the app.

A separate preservation proof is still required. Its exact JSON fields and current values are:

```json
{
  "schema": 2,
  "kind": "run19-qa-preservation",
  "compiler_receipt_sha256": "dd5ab12f803726f7ff8b02a1c3e5aaa7a5ea2184b18b074582641cccae8327f9",
  "provenance_sha256": "0926d992cc0aea2f269a800f8b6b7744641a775b7df1e518551cb86a8612b28b",
  "compiled_artifact_path": "/private/tmp/nudox-gui-flow-integration-20261002/.local/target/debug/backend-desktop",
  "compiled_artifact_sha256": "34ed2485a86e10d0e7ab29842902cac0652e40f70e3bd958014540bb5d7e20a0",
  "capture_source_path": "/private/tmp/nudox-native-candidate-source-f42191",
  "source_head": "f42191f4ec7f4ed0c2eb63a5048351010a0b5059",
  "source_tree": "5889e34cf83200a767a882838859d5625ab54a46",
  "cargo_lock_sha256": "2ab762dd5ecef4b5612aae3bab065578d1f4103e4c680acc62da7589043110c0",
  "capture_artifact_path": "/private/tmp/nudox-gui-user-audit-20261003/current-run19/NudoxAuditRun19.app/Contents/MacOS/Nudox",
  "capture_artifact_sha256": "34ed2485a86e10d0e7ab29842902cac0652e40f70e3bd958014540bb5d7e20a0",
  "capture_bundle_info_path": "/private/tmp/nudox-gui-user-audit-20261003/current-run19/NudoxAuditRun19.app/Contents/Info.plist",
  "capture_bundle_info_sha256": "53b669f869cdd1a9cd57660b069bdda53af403fce01c3a95e7624e7984d7189a",
  "capture_bundle_identifier": "dev.nudox.audit.run19"
}
```

The root must verify and write this as a separate immutable file; it must never be represented as a field retroactively added to the producing receipt. The native driver rechecks both receipts and all source/tool/binary/plan hashes after capture. If source or QA bytes change, stop and re-admit rather than reuse this proof.

Only the sole GUI explorer may establish the live window, initial route, app foreground, requested viewport/text size, macOS Full/Reduced state, Screen Recording/Accessibility/Input Monitoring permissions, and input handoff. The native motion runner must remain idle until the explorer explicitly cedes input ownership and the root confirms the producing/preservation receipts and exact PID/executable. The runner uses `libproc` to reject a PID whose executable differs from the copied QA artifact. Every action is scoped to the selected visible PID/SCWindow. Paired Down/Up points are preflighted before the first press and checked again at dispatch; a failed scope check aborts without a synthetic Up/click in another window and records an unreleased-held-input finding.

## Planned captures and semantic oracle

- `settings-fast-open-close.json`: from a Reader or Orbit page without an overlay, ⌘, at 80 ms, native `Appearance` by 245 ms, Escape at 350 ms. Inspect actual early frames from 0–400 ms and post-exit AX/pixels. `Appearance` is a positive native oracle; there is no inferred animation pass from whole-window changed pixels.
- `find-fast-open-close.json`: begin with the Library shelf's **actual** native `Find packages and declarations` link visible. Native AX click at 80 ms, `Find query` focused and present by 245 ms, ⌘[ at 350 ms. If the link or route is absent, do not substitute a seeded Find state; report the precondition gap.
- `ask-interrupted-reopen.json`: ⌘K at 80 ms, native Ask editor by 245 ms, Escape during entry at 350 ms, ⌘K reopen at 530 ms, inspect settled editor/plate, then close. The plate itself is a GPUI paint/probe rectangle, not an OS AX Dialog; anchor the review to native Ask field plus full-window pixels and the paired GPUI probe if available.
- `settings-15s-settle.json`: after the explorer opens and leaves Appearance stable, capture 15,000 ms with **zero input actions**. The schema allows up to 30,000 ms. A content-driven ScreenCaptureKit stream can go idle; if its actual PTS gaps exceed 1,000 ms, record `uncovered` rather than claiming jank or a smoothness pass.
- `settings-resize-retarget.json`: open Settings; resize the exact selected AX window to 500, 360, and back to 500 points while motion is active. Inspect continuity of the content/edges and focus; geometry changes and GPUI Presented count alone do not pass visual quality.
- `shelf-hide-reveal.json`: begin with the wide inline Library shelf visible at 100% text. Probe the real native `Toggle the shelf` control; use four ⌘\ events to interrupt hide, reveal, hide, and reveal. Inspect the shelf/Reader seam and native toggle state through the measured PTS film, especially where direction reverses. The AX probe establishes the control, while actual pixel crops and the paired GPUI geometry probe establish the moving edge.
- `drawer-500-100-survey.json` and `drawer-360-200-survey.json`: the explorer first prepares **real** Settings Appearance at the stated width and text scale. These surveys positively probe the selected native `100%` or `200%` text choice, capture the initial native Motion `Full` radio and the opened native `Library shelf` dialog. Escape closes the drawer. The survey captures are prerequisites for exact gesture plans, not substitutes for the later Down→drag→Up refusal films.
- `derive_native_drawer_plan.py`: reads a passing survey's full native AX tree/window/display metadata; requires the expected selected text-size choice, one `Full` radio and one `Library shelf` dialog with positive bounds; derives right-strip Down and inside-drawer drag/Up points plus time-paired physical-pixel window/drawer/strip/radio crops. It rejects ambiguous/missing AX, a moved window, no exposed right strip, or out-of-frame crops. The generated plan binds the exact survey SCWindow ID and expected position/size, and the derivation sidecar records survey and plan hashes. Pass the survey `CAPTURE.json` as `--input` for the gesture capture. The expected native result is the drawer still present after the drag-off, the underlying Motion preference unchanged, no background radio focus/activation, then an Escape return to Settings. Native AX and pixels can establish cover/focus/geometry; the unchanged preference needs a separate app-side receipt or user-visible selected-state oracle.
- `derive_native_drawer_plan.py --scenario underlay_retirement`: from the same passing survey, requires the real native Motion `Full` radio to begin unselected. It derives one held Down at that radio, then only the Shell’s ⌘\ drawer-open key and Escape while Down is held, followed by Up at the exact original point. The plan forbids resize, click, or a second press while held. Fresh native AX probes must find one `Full` radio explicitly **unselected** before Down, after the drawer closes, and after Up; absence or a duplicate cannot pass. This tests retirement of the old press after the same keyed control remounts, with physical-pixel crops at each phase. The recorder rechecks the foreground PID/focused selected window before each held key; on scope loss it fails without posting an Up to a foreign window.

For every capture, review first, peak, reversal, and settled frames as individual physical-pixel crops **and** in the measured-PTS movie. Check the actual plate/Reader seam, clipping, interrupted retarget continuity, inert descendants, focus, and 500/360 point right strip. A whole-window changed-pixel fraction is only evidence that something changed within an attributable interval; it is not a beauty or smoothness verdict. The root visually inspects and signs the conclusion. Retain failed/uncovered raw `frames.jsonl`, PNGs, `actions.jsonl`, AX, and `CAPTURE.json` without recreating frames.

## Exact commands after handoff

Replace only `<PID>` and `<PRESERVATION_JSON>` after the root verifies them. Run these from the **clean committed tool checkout**; never from a dirty working tree, because tool identity is hash-bound. Use an empty unique output directory for each case.

```sh
cd /private/tmp/sol-native-motion-run19-plans-20261003
python3 tools/gui-harness/native_motion.py \
  --pid <PID> \
  --binary /private/tmp/nudox-gui-user-audit-20261003/current-run19/NudoxAuditRun19.app/Contents/MacOS/Nudox \
  --source /private/tmp/nudox-native-candidate-source-f42191 \
  --compiler-receipt /private/tmp/nudox-native-candidate-f421-run19/build-receipt.json \
  --preservation-receipt <PRESERVATION_JSON> \
  --plan tools/gui-harness/plans/settings-fast-open-close.json \
  --out /private/tmp/nudox-sol-native-motion-run19/settings-fast-open-close
```

For each other fixed plan, change only `--plan` and the unique `--out` suffix. Before a drawer gesture, capture the matching `drawer-*-survey.json` with the same admission flags, then derive its bounded plan:

```sh
python3 tools/gui-harness/derive_native_drawer_plan.py \
  --survey-capture /private/tmp/nudox-sol-native-motion-run19/drawer-500-100-survey/CAPTURE.json \
  --width 500 --percent 100 \
  --out-plan /private/tmp/nudox-sol-native-motion-run19/drawer-500-100-plan.json
```

Run `native_motion.py` on that generated plan with a unique gesture output, plus `--input /private/tmp/nudox-sol-native-motion-run19/drawer-500-100-survey/CAPTURE.json` and `--input /private/tmp/nudox-sol-native-motion-run19/drawer-500-100-plan.derivation.json`. For the held-underlay retirement capture, repeat the derivation with `--scenario underlay_retirement --out-plan /private/tmp/nudox-sol-native-motion-run19/drawer-500-100-retirement-plan.json`, then pass its distinct plan and derivation as inputs to a distinct empty output directory. Repeat both scenarios for `--width 360 --percent 200` with the matching survey. Keep capture outputs and resolved plans outside both source checkouts. The matrix report can inventory independent rows after review, but its `native_pass` still never grants live owner/index acceptance.
