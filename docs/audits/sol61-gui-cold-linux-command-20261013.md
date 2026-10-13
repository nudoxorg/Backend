# Exact Linux G1 collector handoff — 2026-10-13

This is a source-only handoff. No laptop Cargo, GUI, owner, index, X11 server, or
native capture was launched. Report 22 ran canonical `7f1f672f3`, where PR 202
was still a review branch: one cold run certified at 8.72 seconds, and the
other stalled in ReadingHeader at 7.58 seconds then reported the initial-root
timeout at 67.23 seconds. That result does not test PR 202's fail-fast path.

The frozen `97895b5cb2a113c0eb122e548935cfeb1f0e1ba8` source includes PR 202:
each Root submit captures the ready gate epoch; only a current, definitive
`EngineFault::Failed` can create `InitialRootFailed`; UI delivery rechecks the
current Root request; the gate atomically refuses old attachments and failures
after it has admitted a publication. Its trace category is
`initial-root-failed`, separate from the observer's sixty-second `initial-root`
deadline. The current cancellation/socket repair also retires a cancelled
bootstrap transport before another Root uses it. Neither source property is a
new native acceptance result.

## Minimal targets and prerequisite

The sole native execution broker selects and authenticates the current merged
candidate, managed permit, memory/disk/Cargo limits, and existing private X11
display. It may reuse a matching product binary. If a build is needed, the
minimal normal-GUI target is:

```text
cargo build --locked -p backend-desktop --bin backend-desktop -j 2
```

GPUI mounted/event controls are a separate test-support target. Desktop
dev-dependencies already enable `gpui/test-support`,
`gpui_platform/test-support`, and component headless text hooks:

```text
cargo test --locked -p backend-desktop --lib --no-run -j 2
```

Current `vendor/gpui_ce_platform/src/gpui_platform.rs` explicitly returns
`HeadlessRendererError::Unsupported` on Linux from
`try_current_headless_renderer`. The existing offscreen capture driver cannot
produce Linux PNG acceptance. Do not relabel a mounted TestAppContext or
synthetic projection as a Linux native screenshot. Actual product X11 windows
on the broker's private Xvfb display provide the pixel layer here; no renderer
stub is added.

Required existing X11/capture tools: `xdotool`, `xprop`, `xwd`, ImageMagick
`convert`/`identify`, and `xdpyinfo`. The collector does not create a display or
download tools. It refuses non-Linux live collection, a mismatched source/build
receipt, unavailable display/tools, and missing window-to-PID proof.

## Ten cold starts

The build receipt must be the broker's actual build provenance, with these
fields added or projected from that receipt:

```json
{"source_sha":"<exact 40-character candidate commit>","binary_sha256":"<actual backend-desktop SHA-256>"}
```

The managed receipt is the existing execution admission, not one manufactured
by the collector. The collector retains its hash as a reference; the broker
remains responsible for validating its scope and resource limits.

From the authenticated candidate tree, substitute the broker's actual paths,
candidate commit, and existing display in this exact collector command:

```text
python3 tools/gui-harness/cold_linux_product.py \
  --binary /absolute/path/to/target/debug/backend-desktop \
  --source-sha <exact-candidate-commit> \
  --build-receipt /absolute/path/to/actual-build-receipt.json \
  --managed-receipt /absolute/path/to/existing-managed-admission.json \
  --display :<existing-private-display> \
  --output /absolute/path/to/new-owned-evidence/g1-ten-cold \
  --count 10 --deadline 80
```

Each launch uses a new private empty ambient directory and private
`XDG_DATA_HOME`, config and cache roots. Inherited project/workspace/endpoint/
credential overrides are removed. The ordinary product therefore takes its
actual ambient starter/workspace path; there is no fixture indexing or fake
owner. Ten launches are serial and their data is retained.

The collector verifies the running `/proc/<pid>/exe` hash, waits for actual
`observation.certified` or `observation.failed`, keeps short post-terminal
activity, binds the visible X11 window by `_NET_WM_PID`, captures that window,
and closes only its own window/process group. It preserves bootstrap event
order, ReadingHeader faults, fault-to-observer latency, frame-lifecycle records,
stderr/stdout, executable/tool identities, XWD and PNG bytes, and per-run JSON.
On a private Xvfb display with no window manager, it performs actual native
`windowraise` and `windowfocus` operations after proving `_NET_WM_PID`, then
checks `getwindowfocus -f` against that same window. The exact commands,
results and monotonic intervals are retained as `native_focus`; this is the
native focus needed to display the mapped product. It supplies no fixture,
semantic route, application redraw call, or root admission.
`summary.json` returns success only when all requested runs certify and have a
nonblank native capture. A failed case remains a failed case; the collector
continues to collect the remaining cold starts.

The `pass` field is limited to cold-root certification plus an actual nonblank
window capture. Semantic route acceptance, navigation, sidebar actions, top
buttons, native keyboard/pointer behavior, and watcher publication are explicitly
UNRUN in this collector. After G1's ten actual starts, those checks use the
matched test-support executable and actual native windows, with their own
semantic and pixel proof.

For offline analysis of collected traces, without any native process:

```text
python3 tools/gui-harness/cold_linux_product.py --analyze-only /absolute/path/to/cold-01/trace.jsonl
```

At the original source handoff, verification covered CPython source parse/help, pure
classification controls using explicitly synthetic trace records, and
`git diff --check`. These controls verify the collector only. Product builds,
ten cold starts, native event/capture proof, and G1 acceptance were **UNRUN**.

## Subsequent actual evidence and focus prerequisite

The original no-input collector was run on exact candidate
`710e39d8a7221961e6fc9326fe6b99f8863ea91b`, normal executable SHA-256
`f5e581ffb42c76ad1d3537954bfe97dfd096a19f2d02b06ee92e61f5853cc00e`.
All ten launches proved their running executable and X11 window PID. Three
certified their initial root; seven failed in request 2 ReadingHeader. Each
failure reached the definitive observer failure within 0.07–0.63 ms. All ten
native PNGs were one color, so the preserved cohort passed 0/10 cases.

A separate actual native diagnostic retained that black baseline, then proved
ordinary window raise/focus changed the same product window into a nonblank
Library failure view with real accessibility nodes. Native resize remained
nonblank, and keyboard actions opened Settings, toggled the shelf, and opened
and closed Ask. Initial-root admission still failed. Those diagnostics are
separate from the failed ten-start cohort and do not certify a semantic project
journey, an exact presented-frame bracket, or a macOS bundle.

The focus operations above implement this measured private-Xvfb prerequisite.
The amended ten-start collector has not yet been run. Its next run must use the
reviewed bootstrap-release repair and an exact source-matched build receipt;
the original failed cohort remains unchanged.
