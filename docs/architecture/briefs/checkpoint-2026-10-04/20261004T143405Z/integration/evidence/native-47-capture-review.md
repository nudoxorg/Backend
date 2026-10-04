# Old Run19 native flow review

Recorded 2026-10-04T13:40:19.010Z. Actual running PID 61935, native bundle dev.nudox.audit.run19, executable /private/tmp/nudox-gui-user-audit-20261003/current-run19/NudoxAuditRun19.app/Contents/MacOS/Nudox. SHA256 34ed2485a86e10d0e7ab29842902cac0652e40f70e3bd958014540bb5d7e20a0; producing source f42191f4ec7f4ed0c2eb63a5048351010a0b5059. This is exploratory old-binary evidence, never current-source acceptance. Window 764×510 logical, screenshots 1528×1020. Existing indexed local Rust canary; embedded local owner reported Connected before and after tree failure. No new Index, Add submission, Retry, owner termination, app launch, Cargo/build/check, recorder, TCC or source changes. Settings preferences/geometry unchanged.

## Findings

### P1 Shelf selected declaration cannot be activated with Return

Reproduction: package Page022 → Toggle Shelf023 → Expand Functions024 → Down025 (selects first row Types) → Down twice026 (advance_signal visibly outlined) → Return027 → Escape028. Actual: Shelf stays open after Return, header remains real-rust-canary; after dismissal package intro still displayed. Expected: open selected real declaration. Positive control: reopen029 → click exact advance_signal AX19 →030 opens advance_signal Reader. Native AX continues to report window focused, so visible selection is proven, exact native row focus is not. Owning abstraction candidate: Shell zone/Targets activation authority and selected versus native focus; do not patch individual declarations. Current source includes substantial focus/lease reconciliation; exact new-binary replay remains required.

Raw027 label contains 'opens-function-reader', which describes the intended test, NOT its observed outcome. The raw file is preserved and this paragraph corrects interpretation. Crops026 and030 preserve selected row and detached outline.

### P2 Settings reference lacks keyboard scrolling

Settings→Keys013; PageDown/Next014 no movement; Tab015 no visible focus/AX focus change; wheel8 pages016 reaches actual last reference rows; Home017 leaves bottom position unchanged. Expected keyboard access to long reference. Pointer wheel positive control proves content is reachable. Owning abstraction: Reader scroll keyboard capability, independently from semantic target walking. Current integration shell/reader.rs has overflow_y_scroll/track_scroll and reveal; targeted searches of keys/root/reader found no PageDown/PageUp/Home/End binding, so no identified current fix. This is source inspection, not current runtime validation.

### P2 stale Shelf selection outline survives Reader/Code/Inbox

After pointer activation030, former approximately528px-wide Shelf selection rectangle remains crossing function signature;031 Code and032–034 Reader/history retain rectangle;035 empty Inbox also retains it. It is visually disconnected from current controls. Exact screenshot crop030. Known earlier stale-focus class, not a newly discovered independent renderer cause. Owning abstraction: one mounted focus registration/canvas geometry lifetime; current-source focus fixes require native replay.

### Navigation-state evidence, cause unproven

Find local package Inspect004 expands real declarations; scroll005 exposes Explore package; click006 lands package already scrolled below intro with cadence module selected. No-input007 unchanged; scroll-up008 exposes existing intro. Could be intentional historical presentation restoration or stale reused view; do not call fresh-visit failure without owning route contract. Package Explore works.

### Accessibility disclosure gap

License009 expands visible terms with correct row reflow and no overlap. AX exposes only unchanged Toggle licence details Help; neither expanded state nor terms appears. Native generic text reporting may be broader limitation. Settings roundtrip010–022 collapses disclosure; view-lifetime policy unproven.

### Known resource failure, not compiler or owner death

Library037 → real-rust-canary project click038 (focus only) → Right039 (no step; project action contract unproven) → Dependency tree040 Waiting → no-input041 terminal READ-PROTOCOL: cargo metadata cannot create Cargo.lock because --locked. Long raw diagnostic fits window. No Retry. Settings046 and Diagnostics047 still open and report embedded local service Connected. This is old lockless metadata behavior, not compilation failure; no current-source failure claim.

## Positive paths and qualifications

Agents011/Diagnostics012/Connections018 report local embedded owner connected and exact owned project/workspace/socket. Index019–020 reports16 total declarations,2/2 files,Rust8 declarations; structural Ready versus compiler Active/readiness not checked. No remote availability established. About021 and Escape022 work. Direct Cmd.031 opens actual cadence.rs source1–10; source MorningSignal AX8 click032 opens Reader; Cmd[033 returns Code; Cmd]034 returns MorningSignal. Existing whitespace defect incidentally visible, not repeated as independent finding. Inbox035 shows empty followed-release feed, NOT compile status (raw filename current-publication is only intent). Shelf fold024 works, Down025–026 visibly walks. File042 menu has Add Folder/Close Window. Escape+CmdO043 returned stale native-menu AX context; explicit exposed Cancel044 reveals already-open Add dialog; do not infer CmdO failed. Cancel045 closes dialog without submitting and keeps read error. Settings046 remains available.

## Coverage boundaries

This session inventories a running app; cold startup/owner-disconnected/remote disconnected/actual new compilation/progress/cancel/restart were not induced. No safe new compiler capacity handoff was provided; preserved published workspace/source was not changed. Earlier frozen81 packet already covers200%/geometry restore and broad settings; those known cases were not repeated solely for counts. Screenshots prove endpoints only; no animation timing acceptance. Screen capture recorder not launched.

## Next exact-source native gates

1. Package Shelf → expand Functions → Down to actual advance_signal → Return; compare actual native focus, typed target authority, route and pointer control.
2. Settings Keys long content PageDown/Home/Tab, and mounted scroll/focus ownership.
3. New measured Shelf: cold history Back with tall unmeasured guidance;99px viewport/32px row depth5→1, actual sticky target hitbox; combined replacement+zoom; wheel supersedes pending reveal; Loading→Ready retains target intent; focused row offscreen continuation; drawer shrink/reveal.
4. Fresh owned fixture lockless tree plus current local publication; distinguish metadata refusal from owner failure and compiler terminal.
5. Cold no-index + failed-owner capability, real compile/cancel/restart only under coordinated exact-binary/capacity authorization.

All47 numbered endpoint AX records and any emitted PNGs were inspected; menus042/043 returned AX-only, so no PNG is fabricated. Exact per-capture time/provenance is in matching JSON. This report adds qualifications without rewriting raw records.
