# Wave C.7 chief research journal

Only findings that change the candidate boundary, falsifier, dependency budget, or rollback decision
are retained here.

| Source or experiment | Decision informed | Finding | Decision or falsifier changed |
| --- | --- | --- | --- |
| Baseline source at `f2565a9f`: compile vocab/registry, index vocab, operation, observe, root/locality | accepted application inputs | Compiler has two concrete language rows; index owns only typed identities; graph/vector/application/GUI do not exist; operation and observe already own terminal/probe patterns. | The application service consumes typed existing facts and reports absent backends honestly. It does not invent compatibility or claim backend completion. |
| `GUI_REVAMP_PLAN.md` sections 4–7, 10, 12–15 | GPUI state and visible terminal | Stable IDs, event-driven stores, coalesced notifications, fixed resource states, no polling/shimmer, and visible package/search/graph/health/progress areas are accepted behavior; the plan contains no runtime implementation. | GPUI gets a separate optional adapter crate and real entity/view tests. Its projection cannot own business state. |
| GPUI crates.io `0.2.2` metadata and official Zed GPUI docs/examples (`gpui::test`, `TestAppContext`) | real GPUI dependency and tests | A released Apache-2.0 package and deterministic state/view test surface exist. Default Linux features are broad; feature and release-text cost must be measured rather than assumed. | Reject a local stand-in. Pin `0.2.2`, keep GPUI out of the base service/CLI graph, and measure selected features before closure. |
| MCP final `2026-07-28` specification/blog and official TypeScript SDK migration guide | MCP protocol era | The final revision is stateless: no initialize/initialized session, optional `server/discover`, version/client capabilities in per-request `_meta`, server identity on response `_meta`, and explicit application handles for cross-call state. | Target `2026-07-28`, test `server/discover`, reconnect/replay without protocol shadow state, exact malformed metadata, and concurrent independent clients. Do not implement the superseded 2025 task/session lifecycle. |
| MCP 2026 Tasks extension | long-running command representation | Tasks are an extension with independently evolving cancellation semantics, not required core protocol behavior. | Keep the first MCP slice on ordinary tools plus the service's explicit generation/progress handles. Do not add the Tasks extension until a current client requires it. |
| Local `cargo info gpui@0.2.2` inside the pinned Nix quality shell | GPUI dependency budget | Default features include font-kit, Wayland, X11, and the Windows manifest; platform feature cost is material. | Require resolved target-specific tree and release artifact measurements. Do not put GPUI in the shared root workspace dependency table by convenience. |

## Saturation and remaining uncertainty

For the protocol-era choice, the final MCP release announcement and official SDK migration guide
independently add no competing version or session model: use `2026-07-28`. For the GPUI test surface,
the released crate documentation and upstream examples independently agree on `gpui::test` and
`TestAppContext`; no stand-in is needed.

Uncertainty remains around the smallest GPUI feature set that builds on all declared platforms, the
actual release-text/RSS cost on this Apple Silicon host, and whether the released package's headless
test support is sufficient for every visible-state assertion without opening a platform window.
Those are measured adapter rows, not reasons to weaken the public terminal.
