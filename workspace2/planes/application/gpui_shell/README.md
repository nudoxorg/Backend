# Wave application GPUI shell

This package is a presentation-only consumer of
`wave_application_core::ApplicationReply`. It does not define a second service
vocabulary or pull from a source trait. The core reply is borrowed at the UI
boundary, projected into fixed-capacity state, and rendered as six stable rows.

The default build is headless and keeps the projection testable without a
window, accessibility driver, global search, timer, task, or polling loop. The
`real-gpui` feature enables the pinned GPUI-CE entity and `Render`
implementation.

## Focused gates

Run from the repository root, with the inherited terminal indicator removed for
the offline build scripts:

```text
env -u PROMPT_MULTILINE_INDICATOR LC_ALL=C LANG=C \
  cargo test --manifest-path workspace2/planes/application/Cargo.toml \
    -p wave-application-gpui-shell --offline

nix develop ./workspace2#quality --command \
  env -u PROMPT_MULTILINE_INDICATOR LC_ALL=C LANG=C \
  cargo +1.97.1-aarch64-apple-darwin test \
    --manifest-path workspace2/planes/application/Cargo.toml \
    -p wave-application-gpui-shell --features real-gpui --offline
```

The first command is the deterministic state gate. The second is the required
actual-GPUI gate and runs the two `gpui::test` entity tests.

## Availability ledger

The GPUI dependency is pinned to the git revision recorded in `Cargo.lock`:
`d435891f47743d96bfc6d4ab74c9ecd05af2603e`. Its source is available in the
local Cargo git cache, so resolution is reproducible offline.

The initial non-Nix attempt exposed two toolchain/environment constraints:

1. `libm`'s build script rejected the inherited invalid UTF-8
   `PROMPT_MULTILINE_INDICATOR`; the commands above remove it.
2. The host environment then lacked `clang` while compiling `psm`; the pinned
   Nix quality shell supplies that compiler.

The actual-GPUI test gate passed in the pinned Nix shell after the parent core
API settled: two entity tests and nine headless projection tests pass. The
quality shell's default nightly toolchain does not include `cargo-clippy`; use
the explicit installed `1.97.1-aarch64-apple-darwin` toolchain shown above for
the strict GPUI lint gate. The earlier parent-core type/parse reds and the
host `clang` absence were in-flight/toolchain blockers, not GPUI resolution
failures.

## Resource ledger

| Resource | Bound and owner |
| --- | --- |
| Reply input | Borrowed `&[ApplicationReply]`; caller retains ownership. |
| UI batch | At most `MAX_BATCH_REPLIES = 8`; rejected before mutation. |
| Retained shell state | Fixed enums, one core health array, one core progress page, and one last-reply record; no `Vec`, `String`, `Arc`, task, or timer. |
| Render summary | `[SurfaceSummary; 6]` returned by value; labels and element IDs are `&'static str`. |
| Notification work | One monotonic epoch/`cx.notify()` per non-empty batch, independent of reply count. |
| Normal dependency graph | 8 unique packages in the `cargo tree --edges normal` output (including path crates, before test targets). |
| `real-gpui` dependency graph | 403 unique packages / 721 normal-edge tree entries with the pinned GPUI feature enabled. |
| Release artifact | `libwave_application_gpui_shell.rlib`: 97,024 bytes without GPUI; 372,000 bytes with GPUI (274,976-byte feature delta). The 903 MiB release target directory includes dependency intermediates and is not shipped. |

No unsafe code, global state, accessibility automation, or speculative backend
adapter is present in this slice.
