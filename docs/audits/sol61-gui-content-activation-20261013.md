# GUI content activation and history audit — 2026-10-13

Baseline: `97895b5cb2a113c0eb122e548935cfeb1f0e1ba8`.
Owned branch: `codex/sol61-gui-lifecycle-models-20261013`.

## Evidence boundary

The computer-use inventory on this laptop contained no running Nudox GUI. A
process census found existing MCP endpoints, with no Nudox GUI, Cargo, or rustc.
No GUI, owner, compiler, or index was launched here. There is no new screenshot,
native acceptance result, or binary/source match from this audit.

Report 21's two Linux cold starts remain historical failures: bootstrap
cancellation and immediate re-entry, then request 2 stalls in ReadingHeader,
followed by an initial-root failure roughly 67–69 seconds later. The baseline
already contains the pending startup cancellation/socket and definitive
initial-root failure repairs. This source audit does not validate those repairs
on an actual cold launch.

## Source findings and repair

`GraphFocus` and `Notice` described themselves as visit-scoped, but their
identity was only a route and producer root. Back restores a historical
`ReadingVisit`, including its original `VisitId`, by design. Consequently a
departed route/root/reading identity could match a later activation. Old
notices could replace newer words; an old empty graph publication could clear
newer state. The titlebar, local editor, keyboard handoff, hint session, queued
reading edits, and graph callbacks independently checked overlapping parts of
that same identity.

The repair gives `ReadingSession` a fresh `ReadingActivation`, excluded from
`ReadingVisit` and `RouteHistory`. Its retained allocation token cannot alias
another live token. Fresh navigation, history restore, preview cancellation,
and same-place route replacements renew it. Covers and scalar reading edits
preserve it. History therefore retains scroll, lens, selection intent, and
focus choices while renewed input/publication ownership rejects old callbacks.

`ContentVisit` combines this activation, the exact route, and producer root.
Producer comparison ignores diagnostic observation counters. Local reading
uses the same destination identity without requiring producer authority; owner
failure or a root update does not by itself remove local navigation/editing.

Graph selection and notices, Page/Code requests, deferred graph lookups,
titlebar actions, local editor/native keyboard claims, hints, and delayed
reading deltas now share this identity. The store retires abandoned transient
state before announcing a new snapshot and rejects stale graph publications,
including empty ones, before they can replace current state.

The retained map publishes selection only after explicit presentation under
the current activation. Graph → selected symbol Page/Code → Back retains the
chosen node and camera and republishes it under fresh ownership. Fresh
navigation to the same graph route consumes its route entry again: World
clears the old selected node, and a declaration graph resolves its entry
declaration instead of briefly presenting the departed selection.

A separate lifecycle gap lost the footer's owner recovery guidance during
local navigation after failure. The store now derives a new scoped Retry
notice from the current failed-owner state. This does not restart the owner or
reuse a departed notice.

## Checks actually run

- Rustfmt parse-only output for every changed Rust source file:
  `--edition 2024 --emit stdout --config skip_children=true`, output discarded.
  Exit 0. No compiler or Cargo process was used.
- `git diff --check`: exit 0.
- Source review of all changed identity constructors and graph request/notice
  publishers, including their empty-publication and same-authority paths.

Rust type checking, executable tests, mounted TestAppContext tests, real native
input, and pixel/motion evidence are **UNRUN**. Parse success is not type-check
or behavioral acceptance.

## Brokered remote validation required

Only the existing native execution broker may run these after authenticating
the selected source, binary, X11/test-support prerequisites, fresh managed
permit, and resource budget. No local execution is authorized by this audit.
Use the candidate worktree and retain full command exit status/output.

After the permitted remote build, focused tests can use:

```text
cargo test --locked -p backend-desktop --lib -j 4 runtime::content_visit::tests -- --test-threads=1
cargo test --locked -p backend-desktop --lib -j 4 shell::keyboard::tests -- --test-threads=1
cargo test --locked -p backend-desktop --lib -j 4 queued_local_reading_edits_cannot_land_after_a_same_route_history_return -- --test-threads=1
cargo test --locked -p backend-desktop --lib -j 4 abandoned_graph_publications_cannot_replace_a_returned_visits_current_notice -- --test-threads=1
cargo test --locked -p backend-desktop --lib -j 4 current_owner_failure_republishes_retry_for_local_navigation_without_reviving_old_words -- --test-threads=1
cargo test --locked -p backend-desktop --lib -j 4 deferred_graph_callback_uses_exact_activation_and_ignores_observation -- --test-threads=1
cargo test --locked -p backend-desktop --lib -j 4 an_open_rejects_changed_focus_route_root_visibility_or_generation -- --test-threads=1
cargo test --locked -p backend-desktop --lib -j 4 graph_header_requests_the_exact_selection_at_native_activation_time -- --test-threads=1
cargo test --locked -p backend-desktop --lib -j 4 fresh_same_route_graph_reentry_consumes_entry_instead_of_reviving_departed_selection -- --test-threads=1
```

Required mounted/native follow-up: two fresh cold launches; symbol selection
followed by both topbar Page and Code; keyboard equivalents; deliberate Back
restoring selection/camera/reading offset; fresh same-route re-entry; stale
queued actions during preview/back transitions; owner death and replacement at
the same root; publication/root movement while detail stays open; a cover
open/dismiss cycle preserving local context; and recovery guidance after local
navigation while the owner is failed. Capture actual source/binary hashes,
process identity, current semantic probes, and new rendered frames separately.

The missing package-add entry, live detail refresh, animation quality, and cold
bootstrap behavior still require the actual current native GUI acceptance
matrix. They are not declared fixed by these source changes.
