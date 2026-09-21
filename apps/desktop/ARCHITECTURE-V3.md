# Desktop architecture v3

The desktop is a UI-thread viewport over one immutable `AppSnapshot`. A
snapshot is keyed by the producer's `ViewStateRoot`, epoch, and generation;
the observation counter is diagnostic only. Its shelf, document, project,
settings, and session branches are `Arc` values, so a route change or one
engine delta copies a small shell while retaining untouched branches.

The data boundary is deliberately pure:

```text
typed Intent -> navigation::reduce -> (AppSnapshot, typed Effect)
                                            |
                         runtime::DesktopRuntime / EngineActor
                                            |
                                typed EngineEvent
                                            |
                     authority + stale check -> new AppSnapshot
```

`Route` is an enum with one content depth model: `Orbit -> Package -> Page ->
Source`. Coordinates, local projects, package coordinates, and service
projects have distinct types. Cmd-minus follows the typed parent route and
keeps the selected object. Settings and the command palette are an orthogonal
typed overlay, so opening or dismissing them does not mutate content history.
Back/forward use a bounded persistent route stack with an explicit 64-entry
cap. Widgets receive `SnapshotReadModel` and `IntentDispatcher` ports; engine
DTO mapping lives in `runtime::mapping`.
Palette channels, surface families, density, and semantic marks are closed
token enums in `core::tokens`; visual themes map those tokens to the design
system at the presentation edge.

The GPUI graph has one root entity plus snapshot, focus, modal, and palette
entities. It never owns a second copy of application data. A bounded,
coalescing mailbox runs on a dedicated engine/client actor. Root refreshes and
object reads carry producer generation and cancellation tokens. Replaced work
is cancelled, older producer generations are superseded, and the UI accepts a
result only when its request and basis still match the current snapshot.

Selectors and row geometry are keyed by stable object/delta IDs. Row heights
also include normalized available width and use bounded LRU memo tables. Source
and document viewports share the same stable-ID virtualization contract. A
single frame clock advances all retargetable tweens/springs; reduced motion
snaps tracks, while the capture clock makes frame timestamps deterministic.

Shelf, settings, and session state use a versioned JSON schema. Writes stage a
unique sibling file, `sync_all`, rename atomically, and sync the parent
directory. Cold reload restores durable session/settings choices; engine-owned
content is admitted again at the next producer root.

## Primary-source review

The implementation was informed by these primary repositories and their
current source/docs:

* [hummingbird-player/hummingbird](https://github.com/hummingbird-player/hummingbird)
  — explicit action namespaces and command palette, GPUI `Entity` observation
  edges, modal `FocusHandle` ownership, async resource states, resizable and
  virtualized controls, and one reduced-motion setting. The global
  `ui/models.rs` collection of dozens of independent entities was not copied;
  Nudox keeps one versioned immutable snapshot and keyed selectors. Reviewed
  at source commit `22335dd`.
* [zed-industries/zed GPUI contexts](https://github.com/zed-industries/zed/blob/main/crates/gpui/docs/contexts.md)
  and [GPUI's three registers](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md)
  — UI-thread `App`/`Entity` ownership, fallible async contexts, typed action
  dispatch, and low-level elements for hot collection surfaces. GPUI internals
  such as entity leases remain framework concerns. Reviewed at source commit
  `4ab9b90`.
* [zeronsh/zeron architecture](https://github.com/zeronsh/zeron/blob/main/ARCHITECTURE.md)
  — the engine stays off the UI thread, in-process and remote clients share a
  typed boundary, collections use stable row IDs, background work coalesces,
  and row-height caches bind to `(row, content/delta, width)`. Zeron's
  CRDT/session/agent domain model is outside the Nudox desktop contract and is
  not reproduced here. Reviewed at source commit `394f925`.

## Migration/deletion map

The v3 modules are the replacement target for the current `store`, `reducer`,
and transport-owned window state. The actor has bounded request and result
mailboxes; background producers apply backpressure while the UI is paused, and
`EngineActor::start` reports thread creation failures instead of returning a
dead handle. Explicit `shutdown` joins from a non-UI owner; UI teardown closes
both bounded channels and releases the handle without waiting for an arbitrary
client call. During the visual-lane transition, old views remain buildable so
visual work can migrate against the public ports. The final shell removes the
old `store/*`, `reducer/*`, `transport/*`, and `host/launch.rs` state owners
after the visual roots consume `UiEntityGraph`; no aliases or second state
owner should be retained in that cutover.
