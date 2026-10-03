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

### Recorder identity and permission preflight

The first Run19 Settings attempt did not capture a frame or post an action. Its verified build receipt admitted the QA executable, but `window.jsonl` recorded `ax_trusted:false` and frontmost PID 10250 instead of the target 14636. `SCStream.startCapture()` returned without delivering a first frame; that fact alone does not prove why. The revised recorder writes `preflight.jsonl` *before* ScreenCaptureKit selection, recording independent `CGPreflightScreenCaptureAccess()`, `AXIsProcessTrusted()`, and target-frontmost results. Screen Recording and Accessibility must pass for this AX-backed evidence tool. Target frontmost is required for any posting action; a passive, zero-input capture or read-only AX probe records foreground state without rejecting a visible background window unless the plan explicitly sets `require_frontmost:true`. If admission passes but ScreenCaptureKit still delivers no frame, `stream-state.jsonl` records a separate `NoFirstFrameAfterStartCapture` outcome and any stream-delegate error. The Python manifest preserves these sidecars and zero frame/action counts on failure; none becomes a motion pass.

Use one dedicated recorder app, separate from the product QA app. A team-signed app keeps its designated requirement across rebuilds. `security find-identity -v -p codesigning` currently reports **zero valid identities** on this host. Apple DTS confirms that a frozen ad-hoc signed app can receive Screen Recording permission, but a rebuild changes its identity and needs a new OS UI grant. The runner therefore admits `TeamSignedStableAcrossBuilds` or `AdHocFrozenContentAddressed`, with the latter requiring a path suffixed by its exact CodeDirectory hash. It verifies the app seal, bundle/Info, binary SHA, CDHash, designated requirement and path before and after capture. Do not borrow another app's identity, edit TCC databases, or grant privacy permissions programmatically.

For a team-signed recorder, with an available Apple Development or Developer ID identity:

```sh
QA_RECORDER_APP=/Applications/NudoxMotionRecorder.app
QA_SIGN_ID='Apple Development: YOUR NAME (YOURTEAMID)'
mkdir -p "$QA_RECORDER_APP/Contents/MacOS"
plutil -create xml1 "$QA_RECORDER_APP/Contents/Info.plist"
plutil -insert CFBundleIdentifier -string dev.nudox.audit.motion-recorder "$QA_RECORDER_APP/Contents/Info.plist"
plutil -insert CFBundleExecutable -string NudoxMotionRecorder "$QA_RECORDER_APP/Contents/Info.plist"
plutil -insert CFBundlePackageType -string APPL "$QA_RECORDER_APP/Contents/Info.plist"
plutil -insert NSScreenCaptureUsageDescription -string 'Capture the explicitly selected QA window for native motion testing.' "$QA_RECORDER_APP/Contents/Info.plist"
xcrun swiftc -parse-as-library -O tools/gui-harness/native_motion.swift -o "$QA_RECORDER_APP/Contents/MacOS/NudoxMotionRecorder"
codesign --force --options runtime --sign "$QA_SIGN_ID" "$QA_RECORDER_APP"
codesign --verify --strict --verbose=2 "$QA_RECORDER_APP"
codesign --display --verbose=4 "$QA_RECORDER_APP"
codesign --display --requirements - "$QA_RECORDER_APP"
```

For a single frozen QA recorder without a team identity, use a **new empty staging bundle**, replace `$QA_RECORDER_SOURCE` with the reviewed clean tool checkout, and sign this app with its own ad-hoc signature. Move it once to the CDHash-named final path, then do not rebuild or modify it. The exact final path is what the OS UI must grant:

```sh
QA_RECORDER_SOURCE=/path/to/clean/committed/tool-checkout
QA_RECORDER_STAGE=/private/tmp/NudoxMotionRecorder-staging.app
mkdir -p "$QA_RECORDER_STAGE/Contents/MacOS"
plutil -create xml1 "$QA_RECORDER_STAGE/Contents/Info.plist"
plutil -insert CFBundleIdentifier -string dev.nudox.audit.motion-recorder "$QA_RECORDER_STAGE/Contents/Info.plist"
plutil -insert CFBundleExecutable -string NudoxMotionRecorder "$QA_RECORDER_STAGE/Contents/Info.plist"
plutil -insert CFBundlePackageType -string APPL "$QA_RECORDER_STAGE/Contents/Info.plist"
plutil -insert NSScreenCaptureUsageDescription -string 'Capture the explicitly selected QA window for native motion testing.' "$QA_RECORDER_STAGE/Contents/Info.plist"
xcrun swiftc -parse-as-library -O "$QA_RECORDER_SOURCE/tools/gui-harness/native_motion.swift" -o "$QA_RECORDER_STAGE/Contents/MacOS/NudoxMotionRecorder"
codesign --force --sign - "$QA_RECORDER_STAGE"
codesign --verify --strict --verbose=2 "$QA_RECORDER_STAGE"
QA_RECORDER_CDHASH=$(codesign --display --verbose=4 "$QA_RECORDER_STAGE" 2>&1 | sed -n 's/^CDHash=//p')
QA_RECORDER_APP="${HOME}/Applications/NudoxMotionRecorder-${QA_RECORDER_CDHASH}.app"
mkdir -p "${HOME}/Applications"
test -n "$QA_RECORDER_CDHASH" && test ! -e "$QA_RECORDER_APP" && mv "$QA_RECORDER_STAGE" "$QA_RECORDER_APP"
codesign --verify --strict --verbose=2 "$QA_RECORDER_APP"
codesign --display --requirements - "$QA_RECORDER_APP"
shasum -a 256 "$QA_RECORDER_APP/Contents/MacOS/NudoxMotionRecorder" "$QA_RECORDER_APP/Contents/Info.plist"
```

Keep the resulting `.app` at that fixed path. A new binary, Info.plist, CDHash, or path is a **new recorder identity** and needs a fresh OS UI grant; the old permission is not a valid receipt for it. The Python runner requires `--recorder "$QA_RECORDER_APP/Contents/MacOS/NudoxMotionRecorder"`. Grant **NudoxMotionRecorder** in macOS **System Settings → Privacy & Security → Screen & System Audio Recording** (called **Screen Recording** on some versions) and **Accessibility** using the OS UI. Restart the recorder after the Screen Recording grant. If macOS requests **Input Monitoring** for this recorder, grant that through the same Privacy & Security UI. Then the sole GUI explorer brings the exact QA product PID/window to the front and explicitly cedes input ownership before any posting plan runs. A previous failed artifact remains untouched; the retry needs a new empty output directory and the root's current exact build/preservation receipt.

The first permission retry revealed a separate launch-ownership problem: macOS TCC logged the recorder as the **requesting/accessing** process but `com.openai.codex` as the **responsible** process for its direct Python-child launch. TCC used `com.openai.codex` as the Accessibility subject and returned denied, even though the frozen recorder's own app entry was enabled. The recorder's `Bundle.main.bundleIdentifier` alone cannot prove the TCC subject. The opt-in `--launch-bundle "$QA_RECORDER_APP"` route uses `/usr/bin/open -n -g -W -a` to launch the exact verified app through LaunchServices, with `--stdout` and `--stderr` into new regular output files. The runner records the **launcher** wait status separately from the recorder's unknown process exit; it requires a native preflight sidecar, exact recorder executable/bundle/target PID, and a native result sidecar before analyzing frames. A timeout stops only the `open` process group and reports that the recorder may still be running. For now this mode accepts **zero-input** plans only, so an orphaned recorder cannot post later timed input. It does not claim LaunchServices corrected TCC attribution until a new read-only TCC log shows `AUTHREQ_SUBJECT=dev.nudox.audit.motion-recorder` for that recorder PID.

The versioned recorder reads a regular, singly linked plan inode through a no-follow descriptor, at most 1,000,000 bytes plus one overflow byte. It hashes the captured `Data` and rejects a mismatched expected SHA before JSON decoding, ScreenCaptureKit admission, or input; a write-once receipt retains the actual bounded byte count and digest. A missing expected digest on an active plan is rejected after bounded decoding. This establishes the bytes that recorder decoded when the receipt is `MatchedV1`; Python's source/resolved snapshots alone cannot establish that across processes. The older signed recorder has no versioned receipt and must be treated as unverified.

The legacy LaunchServices route remains blocked for active capture. `open -W` exit status is launcher evidence, not recorder exit; killing its process group on timeout may leave a timed action in the recorder. Background launch (`-g`) can retain the product as the frontmost app, and the Swift sender already checks the exact selected PID/window, foreground PID, focused window for keys, and pointer hit target at dispatch. Those checks alone do not prevent a late event after the Python driver has timed out. The source-only supervised route below adds exact recorder identity, a live arm/cancel channel, a monotonic deadline, and separate stop/exit proof. Timeout without that proof remains a failed capture with input ownership unresolved; do not remove the legacy zero-input guard.

The source-only `--supervise-v1 --attest-plan-v1 --launch-bundle` route implements that additional protocol for a **future separately built and permitted recorder**. It has not been tested with a native signed build. Python creates a private mode-0700 Unix-socket directory and mode-0600 one-run nonce file, then starts the exact app with LaunchServices `-g`. A kernel `LOCAL_PEERPID` check, libproc executable path, frozen code-signing identity, native plan-consumption receipt, target PID, and digest must all match before Arm. The driver registers `kqueue` `EVFILT_PROC/NOTE_EXIT` on that peer before Arm, so a later PID reuse cannot supply process-exit proof. Its action protocol is `Armed → Permit → Done`; Cancel racing a granted Permit waits for Done and retains ownership. A held Down enters `HeldAwaitRelease`: cancellation skips every intervening action and permits only the plan's matched Up under the existing exact selected-window/foreground/hit checks. If that Up cannot be safely posted, the native stop receipt records held input and the capture remains unresolved. Per-message controller deadlines cannot be extended by slow bytes, and both sides recheck their deadlines after a reply. Swift checks again after blocking AX/window resolution, immediately before the actual posting group or AX mutation. Normal completion or cancellation requires a native `StopAck`, its write-once stop receipt, the separate kernel exit event, **and observed control-socket EOF** before `SUPERVISION.json` admits input ownership release. Kernel exit is bound to the accepted peer process; EOF confirms that no inherited transport endpoint remains open after it. The Python driver signals neither the product GUI nor an unverified recorder; it may terminate only its direct `open` child, which never counts as recorder-exit proof. Missing acknowledgments, timeouts, and exceptions remain `Unresolved` with raw evidence preserved. The legacy e976 bundle still only runs passive zero-input captures; this source is not a claim that it supports v1 supervision.

For a permission/transport preflight with no actions, omit `require_frontmost` (or set it false); ScreenCaptureKit can capture the selected visible window while another app is frontmost. A final film intended to show the active user interface should explicitly require the product frontmost and have the explorer establish that state first. Accessibility remains required here because the recorder collects and analyzes paired native AX evidence even during passive capture. Use a **new, clean committed Python tool checkout** for the LaunchServices source revision; the earlier e39 recorder build receipt proves the frozen Swift executable, while the later tool checkout is independently identified and hashed in each capture. Never replace the e39 build source fields with the later Python revision.

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
cd /path/to/reviewed-clean-recorder-checkout
python3 tools/gui-harness/native_motion.py \
  --pid <PID> \
  --binary /private/tmp/nudox-gui-user-audit-20261003/current-run19/NudoxAuditRun19.app/Contents/MacOS/Nudox \
  --source /private/tmp/nudox-native-candidate-source-f42191 \
  --compiler-receipt /private/tmp/nudox-native-candidate-f421-run19/build-receipt.json \
  --preservation-receipt <PRESERVATION_JSON> \
  --recorder "$QA_RECORDER_APP/Contents/MacOS/NudoxMotionRecorder" \
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
