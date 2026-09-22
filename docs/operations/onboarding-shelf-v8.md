# Onboarding and project shelf capture plan

The onboarding and shelf surfaces are intended to be captured through the
existing canonical GPUI harness extension seam. This slice deliberately does
not own `apps/desktop/src/harness.rs` or `tools/gui-harness`; the parent
integration should register these state names against the live host and the
same semantic/action tree used by the desktop. A capture adapter must seed only
typed workspace lifecycle state. It must never walk the filesystem or fabricate
index progress.

This slice is intended to land after live-index commit `127604a67`. The
canonical request chain is `UiRootEntity::request_index` →
`EngineCommand::IndexProject { project, basis, request }` →
`EngineRequest::IndexProject` → `Session::index(path)`. The service returns the
authoritative root projection; the coordinator admits one root refresh and the
shelf becomes `Ready` from that response. Until the service exposes progress,
`Indexing` stays indeterminate. The shelf UI never supplies a file count or
progress value of its own.

## State matrix

| State | Typed seed | What it proves |
| --- | --- | --- |
| `onboarding` | EmptyFirstLaunch | First launch has no shelf row and offers folder, Claude/MCP, and help actions. |
| `onboarding--folder-picker` | FolderPicker | Picker handoff and invalid-path recovery remain readable. |
| `onboarding--indexing` | Indexing | The shelf uses an indeterminate `Indexing…` status while `Session::index` is in flight. |
| `onboarding--failed` | Failed | A bounded error and retry/reveal/remove affordances survive a service failure. |
| `onboarding--ready` | Ready | Completion is shown only after authoritative service success. |
| `shelf--multiple-projects` | MultipleProjects | Recent projects are explicit rows with per-row activate, reveal, and remove actions. |
| `shelf--mismatch` | Mismatch | The selected shelf project and the live service workspace are both named before a query can proceed. |
| `shell--collapsed` | Ready + collapsed shelf | Rail geometry preserves project status and keyboard targets. |
| `shell--text-135` | Ready + 135% text | Long labels wrap without clipping at a large reading size. |
| `shell--text-200` | Ready + 200% text | The largest supported type setting stress-tests the narrow shell. |
| `focus--omnibar`, `focus--library`, `focus--reader` | Ready + focus owner | Focus rings and traversal regions remain discoverable. |
| `browse--settings-connections` | Ready + Connections page | Dynamic Claude/MCP command/config, local daemon mode, connection test, and CLI help. |

Each state is run at every canonical harness viewport at 1x and 2x. The narrow
640×480 and 800×600 cases are required for the collapsed shelf and settings
sheet; the 1440×900 and 2560×1440 cases are required for the full shelf and
large type review.

## Crop review matrix

Full-window screenshots are insufficient for small regressions. Every capture
review should also inspect these crops at native scale:

| Crop | Bounds in logical pixels | Review questions |
| --- | --- | --- |
| `header-actions` | x=0..viewport width, y=0..96 | Do Add project, shelf/context toggles, settings, and help retain labels and focus rings? |
| `shelf-rows` | x=0..320, y=72..viewport height−28 | Are paths elided safely, is `Indexing…` indeterminate, and are activate/reveal/remove targets reachable? |
| `onboarding-actions` | x=content left, y=140..360 | Does the first-run hierarchy survive 135% and 200% text without button overlap? |
| `mismatch-banner` | x=content left, y=120..300 | Are selected and live paths both visible and is the switch action explicit? |
| `settings-navigation` | x=viewport right−220..viewport right, y=72..viewport height−44 | Does the page list remain scrollable and keyboard-addressable at narrow widths? |
| `settings-body` | remaining settings sheet body | Do command/config blocks wrap, and are copy buttons still adjacent to their payload? |
| `status-bar` | x=0..viewport width, y=viewport height−40..viewport height | Does connection/index state remain legible after large-type scaling? |

The visual review should inspect the full crop, then a 2× magnified crop of the
same bounds. The canonical harness owns crop writing and semantic manifests;
the parent integration should register these bounds there rather than adding a
second driver. The onboarding and shelf surfaces use ordinary GPUI elements,
with no canvas or new SVG primitives added to the product path.

## Process journeys

1. Launch with an ambient empty host; verify the shelf is empty and the native
   folder action is the only admission path.
2. Choose a valid directory. The reducer admits an indeterminate row and
   `UiRootEntity::request_index` submits a typed
   `EngineRequest::IndexProject`; the row becomes Ready only after the real
   `Session::index(path)` response and one authoritative root refresh.
3. Cancel, retry, and fail the same path. Cancellation retires the request and
   keeps the row; retry coalesces by canonical path; failure retains a bounded
   diagnostic and retry action.
4. Add a second directory, activate each row, and check the live service label
   after each completion. The mismatch banner prevents a stale shelf choice
   from looking like the queried workspace.
5. Remove the active row. The parent runtime adapter should call the canonical
   local-service removal command, refresh the root, and index the next active
   row when one remains. The reducer keeps the shelf transition pure and
   persists the selected fallback.
6. Close and reopen the desktop. The durable shelf and active project restore;
   missing folders are marked `Folder missing`, while the service path remains
   visible as runtime evidence.

## Validation status

The source-side matrix and journeys are prepared. Compiler, GPUI runtime,
screenshots, crop inspection, and restart evidence remain queued until the
host build lane is assigned; this worktree has intentionally not run Cargo,
Nix, rustc, or a test binary under the current resource ceiling. The parent
integration must consume canonical `ResponsiveLayout` geometry (wide shell:
titlebar 50, rail 54, shelf 264, context 300, status 26) rather than adding
panel widths to the shelf component.
