# Wave application GPUI shell

This package is the thin GPUI consumer of the one in-process
`wave_application_core::ApplicationService`. The real GPUI entity owns that
service and accepts only typed `ApplicationInput` values. It projects typed
application replies into a stable Home, Libraries, Search, Connections, and
Settings frame with a status strip and command palette.

The default build keeps the projection headless and testable without a window,
accessibility driver, global search, timer, task, or polling loop. The
`real-gpui` feature enables the pinned GPUI-CE entity, keyboard command palette,
and `uniform_list` virtual rows with nearest-selection reveal.

## Focused gates

Run from the repository root with the pinned toolchain and inherited terminal
indicator removed for the offline build scripts:

```text
env -u PROMPT_MULTILINE_INDICATOR LC_ALL=C LANG=C \
  cargo +1.97.1-aarch64-apple-darwin test \
    --manifest-path workspace2/Cargo.toml -p wave-application-gpui-shell --offline

nix develop ./workspace2#quality --command \
  env -u PROMPT_MULTILINE_INDICATOR LC_ALL=C LANG=C \
  cargo +1.97.1-aarch64-apple-darwin test \
    --manifest-path workspace2/Cargo.toml -p wave-application-gpui-shell \
    --features real-gpui --offline
```

The first command checks the fixed headless projection. The second runs the
actual GPUI entity tests, including keyboard palette navigation, recover-local
→ pending → completed, and cancellation through the entity-owned service.

## Availability ledger

The GPUI dependency is pinned to the git revision recorded in `Cargo.lock`:
`d435891f47743d96bfc6d4ab74c9ecd05af2603e`. Its source is available in the
local Cargo git cache, so resolution is reproducible offline.

The application service intentionally reports unavailable compiler output and
absent index/graph/vector providers as typed degraded facts until accepted typed
compiler/publication and retrieval seams are supplied. This shell does not add a
fixture adapter, reinterpret backend validation, or claim lower-plane closure.

## Resource ledger

| Resource | Bound and owner |
| --- | --- |
| Service input | One borrowed `&ApplicationInput` per entity command. |
| Reply input | One copied `ApplicationReply` projected per command. |
| Retained shell state | Fixed enums, one six-fact health array, navigation route, bounded palette query, and one last-reply record; no task, timer, or polling owner. |
| Palette rows | GPUI `uniform_list` materializes only visible command rows and uses `ScrollStrategy::Nearest` after keyboard movement. |
| Render summary | `[SurfaceSummary; 7]` returned by value; labels and element IDs are `&'static str`. |
| Notification work | One `cx.notify()` per successful entity command. |

No unsafe code, global state, accessibility automation, or speculative backend
adapter is present in this slice.
