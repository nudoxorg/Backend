# Desktop GUI architecture

The desktop window has one navigation owner: the production document and
shell stores. Views render projections of those stores and send typed GPUI
actions back to the workspace. The visual layer does not maintain a second
route, overlay, or focus reducer, so the route shown in a screenshot and the
route reported by the workspace semantics always come from the same state.

The canonical Facet boards are visual fixtures in `views/design.rs`. They use
the same palette, surface, glyph, text, and CE action primitives as product
routes. The harness mounts each board through the live workspace host, then
asserts its route, overlay, focus, action tree, geometry, and virtual motion
clock. They do not stand in for package or project data.

Header disclosures, settings, onboarding, the folder chooser, MCP setup, and
reader sheets are owned by the workspace transient stack. Escape dismisses the
topmost transient and restores the store-selected focus owner. CE action
metadata is published from the rendered controls and is the source used by the
harness for keyboard order, enabled state, bounds, and focus assertions.

Viewport classification remains a shell concern: the titlebar, shelf, reader,
and context rail share the same compact breakpoint. Appearance and motion are
also read from preferences; the harness can set Abyss or Glacier and reduced
motion through the same production preference paths before capture.
