# Zeron GUI comparison

The comparison used the read-only checkout at
`.local/luna-gui-brutal-target/zeron`, commit
`0e3239d9739a1f230879b15514fe51ea3936aa4d`. The checkout was inspected for
interaction and layout techniques; no Zeron code was copied into Nudox.

## Techniques adopted

- `crates/ui/src/shell.rs:4-11` keeps a sidebar within a bounded drag range,
  uses an empty drag ghost, and animates the panel over a short ease-out. The
  Nudox shell already owns panel springs, so the GUI slice keeps that owner and
  applies the same clamp-and-retarget rule in `apps/desktop/src/store/shell.rs`.
- `crates/ui/src/shell.rs:91-127,145-201` defers focus restoration by one
  mounted frame and preserves the interpolated size when a transition reverses.
  Nudox's focus and panel state remain store-owned; the harness now asserts the
  resulting focus route after every state application.
- `crates/ui/src/motion.rs:12-20,126-137,822-823` leases animation frames to
  mounted consumers and snaps under reduced motion. This is reflected in the
  production capture clock and the existing `motion` module; the capture slice
  does not add a timer or an allocation per frame.
- `crates/ui/src/transcript.rs:1-26,82-134,2286-2302` uses bounded virtual
  rows, edge-scroll only during an active drag, and stable animation identity
  across row remounts. The graph canvas follows the same bounded projection and
  stable `ElementId` approach in `apps/desktop/src/graph.rs` and
  `apps/desktop/src/views/page.rs`.
- `crates/ui/src/shell/command_palette.rs:10-17,108-130,210-223` makes focus,
  previous focus, and scroll ownership explicit. The desktop route adapter
  uses the existing CE command/omnibar focus owner and checks the exact settings
  page and route in `WorkspaceSemanticProbe`.
- `crates/ui/src/files/tree.rs:21-27,98-161` derives scrollbar metrics from
  virtual-list state and keeps ancestor guides inside a row. Nudox keeps its
  scroll handles in the document/workspace owners rather than copying the tree
  implementation.
- `docs/mobile-polish.md:70-75,108-117,130-164` calls for a real narrow and
  landscape matrix, stable anchors during close, and frame-sampled keyboard
  movement. Those constraints informed the effective viewport preflight in
  `tools/gui-harness/src/gpui_driver.rs` and `tools/gui-harness/src/lib.rs`.

## Techniques rejected

Zeron's transcript-specific row cache and Swift `TimelineView` loaders
(`crates/ui/src/transcript.rs` and `apps/ios/Zeron/Views/Loaders.swift:36-107`)
were not transplanted: Nudox's graph, registry, and document data have a
different revision and ownership model. Likewise, Zeron's shell wrapper IDs
(`crates/ui/src/shell.rs:973-974`) are a warning for Nudox's existing CE
component tree, not a reason to introduce another route or state tree. The
Nudox graph continues to consume the single rich projection in
`apps/desktop/src/graph.rs`, with route identity stored by `DocumentStore`.
