# Nudox GUI lane orchestration

GUI evidence is generated from the pinned Nix closure, the live server/index,
and a real GPUI driver. A lane is not allowed to replace the driver, package
responses, compositor, or font input with a fixture. The control plane lives in
`.config/nix/control.nix`; the scenario catalog is `.config/gui/journeys.json`.

Start a lane from its worktree with an explicit shell closure:

```text
nix shell path:.#gui-harness path:.#gui-tools --command backend gui validate
```

The closure keeps final artifacts lane-local and routes intermediate builds
through the bounded warm-directory pool used by `luna-tools`. This avoids a
single Cargo build lock while sharing dependency work; saturated lanes use
their own overflow directory and still share `sccache`. Keep `NUDOX_GUI_LANE`
unique per worktree so nextest, native outputs, and evidence never cross GUI
slices. The harness exports fixed viewport, scale, locale, timezone, font,
display, color profile, virtual clock, and reduced-motion inputs. It emits
artifact manifests containing the revision, scenario hash, control-plane hash,
image-byte hashes, frame timestamps, and driver transcript hashes.

The driver must be an executable named by `NUDOX_GUI_DRIVER` (or `--driver`) and
implement `nudox-gui-driver-v1`:

```text
driver journey --protocol nudox-gui-driver-v1 --manifest <path> --journey <id> \
  --mode <mode> --locald-endpoint <path> --viewport <width>x<height> --scale <scale> \
  --capture-dir <directory> --report <path>
driver acceptance --protocol nudox-gui-driver-v1 --loop <name> --cases <count> --output <path>
```

The endpoint is the real locald framed socket used by the GUI, CLI, and MCP
adapters. The workspace flake builds `backend-locald`, `backend-cli`,
`backend-mcp`, `nudox-gui-service`, and `nudox-gui-probe` together. The shell
exports the service and readiness paths, so lifecycle does not depend on a
developer's PATH:

```text
nix shell path:.#gui-harness path:.#gui-tools path:.#gui-runtime --command \
  backend gui service --action start --endpoint "$PWD/.local/locald/backend.sock"
nix shell path:.#gui-harness path:.#gui-tools path:.#gui-runtime --command \
  backend gui service --action status --endpoint "$PWD/.local/locald/backend.sock"
```

`nudox-gui-service` starts and stops the real `backend-locald` binary. The
readiness command uses the real `backend-cli --format json health` framed
request, so the harness only admits a live endpoint and never invents an HTTP
health or ingest protocol. The supervisor owns process lifetime and stores its
lane-local workspace, pid, and log next to the endpoint.

The runtime also ships `nudox-gui-service-test`. It starts the real locald,
checks readiness, holds the lifecycle lock to exercise contention, injects a
stale/reused PID owner record, verifies that status refuses it, and then stops
the daemon. A lane should run it before any journey shard.

The driver owns GPUI offscreen capture and the visible-window journey. The
harness separately captures compositor frames with `backend gui capture`, so a
review can distinguish deterministic scene evidence from native titlebar,
clipboard, focus-routing, and resize evidence. Animation runs include start,
first-moving, midpoint, retarget, reversal, near-settled, settled, and
reduced-motion phases. A frame must pass PNG validation before it enters the
manifest; a missing or empty capture fails the run.

Before acceptance, `backend gui provenance` asks the real driver for the
revision-pinned GPUI and `gpui-ce-component` source digests, dependency-graph
digest, toolchain, and GPU backend. It independently probes the compositor's
actual backend/device and checks the realized Cargo metadata plus duplicate
dependency tree; a driver self-report cannot satisfy this gate by echoing an
environment variable. A mismatch against the Nix closure fails the lane.
If the locked graph has no resolved `gpui-ce-component` source hash, the
provenance gate stays failed closed until that dependency is pinned.
Acceptance also requires `NUDOX_GUI_HOLDOUT_MANIFEST` and an
independent `NUDOX_GUI_HOLDOUT_VERIFIER`; author fixtures alone cannot sign off
the result. Frame traces must report allocation, object, and timer leaks, and
the artifact manifest records the bundled font file-byte manifest, encoder version,
GPU backend, toolchain, source revision, deterministic clock, and redaction
policy. Raw driver transcripts are deliberately not persisted.

Run the complete selected shard against the live index:

```text
NUDOX_GUI_LANE=lane-a \
NUDOX_GUI_LOCALD_ENDPOINT=$PWD/.local/locald/backend.sock \
NUDOX_GUI_DRIVER=/absolute/path/to/nudox-gui-driver \
nix shell path:.#gui-harness path:.#gui-tools --command \
  backend gui journey --lane lane-a --shard-count 8 --shard-index 0
```

`backend gui matrix` prints the route, state mode, viewport, scale, and shard
cross-product before a driver is started. Use it to prove that all manifest
rows are assigned exactly once. `backend gui acceptance` invokes four
independent loops: structured property generation, weighted randomized event
generation, independently implemented GUI/CLI/MCP differential queries, and
metamorphic relations for no-op deltas, event batching, restart, theme, and
motion. The default case counts are deliberately large and shrinking is
required where the driver supports it. Failure injection and persistence modes
are part of the versioned control plane, not optional prose.

The testing policy follows Dan Luu's observations in
[`ai-coding`](https://danluu.com/ai-coding/) and
[`agentic-testing`](https://danluu.com/agentic-testing/): generated tests need
structured inputs that reach meaningful paths, independent reconstruction is
needed for high-risk behavior, artifacts reduce false positives, and a test
that found a bug remains in regression coverage. The harness therefore records
scenario inputs and artifacts, requires fresh GUI/CLI/MCP implementations for
differential checks, and rejects a driver that advertises synthetic operation.
The default checks are not a collection of shallow fixed-output unit tests.

The required matrix comes from the GUI implementation gate: nine viewport
sizes (including 640x480 and 800x600) at 1x and 2x, every route and geometry-changing loading/empty/partial/
offline/error/overlay/focus state, keyboard-only traversal, focus restoration,
pointer/wheel/resize and animation interruption, reduced motion, and live
package indexing, code search, semantic pipeline, settings, and MCP/CLI parity.
The driver is responsible for asserting GPUI semantic labels, hit targets,
contrast, graph provenance, and canonical view-root parity; the harness records
the evidence and fails closed when the driver or live index is absent.

Artifact cleanup is dry-run by default:

```text
nix shell path:.#gui-harness path:.#gui-tools --command backend gui clean --lane lane-a
nix shell path:.#gui-harness path:.#gui-tools --command backend gui clean --lane lane-a --apply
```

Only children below `.local/gui-artifacts` are eligible, active resource locks
are never removed, and an explicit `--apply` is required. Store garbage
collection remains a separate operator action; keep `gui-harness` and
`gui-tools` rooted before reclaiming unrelated store paths. Do not use `nix
develop` for GUI evidence because it does not make the lane closure explicit.

Each lane hands off exact commands, selected shard and count, pass/fail totals,
resource-lock manifests, artifact paths, image and perceptual comparisons,
startup/resource observations, external limitations, and its isolated commit.

## Design contract and rendered-evidence gate

The four unmodified HTML references under `Nudox-Design-System/artifacts` are
extracted into the checked-in machine-readable contract before a desktop run:

```text
nix shell path:.#gui-harness --command \
  backend-gui-harness design-contract extract \
  /absolute/path/to/Nudox-Design-System tools/gui-harness/design-contract.json
```

The contract records each declared artboard, exact source hash, palette,
typography, spacing, bevel, cut, glyph, focus, and motion declaration, plus
the wide/compact/narrow, 1x/2x, Ink/Glacier, reduced/full-motion, route,
overlay, interaction, and keyboard matrix. Captures attach a compact
`reference` identity to every manifest; it contains the contract hash and
per-artifact hashes/artboards. The harness never generates a reference image.

After a live run, the independent PNG/semantic gate reads the encoded bytes
and writes `conformance-report.json`:

```text
nix shell path:.#gui-harness --command \
  backend-gui-harness verify-conformance .artifacts/gui-harness \
  --contract tools/gui-harness/design-contract.json
```

It fails when logical dimensions multiplied by scale disagree with PNG and
frame metadata, when a large exact-color row/column indicates an upper-left
crop or unused viewport remainder, or when the route/root, live data
revision/coordinate, hashed semantic probe, action-tree focus order,
accessibility roles, keyboard journey, or external reference identity is
absent. Full-motion transitions
must have monotonic deterministic frame times with positive elapsed duration and
meaningful changed pixels;
static states may remain unchanged and reduced motion must settle. When an
external baseline is supplied, its full pixel metrics are retained and a
policy violation fails the color/comparison gate. The report keeps geometry,
color, typography, clipping, focus, and animation categories separate so a
stale `run-report.json` cannot turn malformed evidence into a pass.

## Desktop onboarding and shell journeys

The GPUI CE desktop adapter also exposes a production-input journey catalog in
`backend-desktop-gui-harness`. The onboarding slice covers the no-project
Get Started surface, the inline add flow and native folder-picker entry point,
indexing progress, shelf switching, restart persistence, MCP setup, titlebar
header menus, keyboard focus traversal, compact/wide resize, 1x/2x scale, and
Ink/Vellum appearance changes. Every journey captures the live
`WorkspaceSemanticProbe` beside its frames. The probe includes the onboarding
flag, header-menu id, shelf count, active language, data/revision identity, and
MCP health so a matching screenshot cannot hide a stale or mislabeled route.

Run the focused desktop catalog directly when the Nix shell is unavailable:

```text
cargo run -p backend-desktop --features visual-harness --bin backend-desktop-gui-harness -- capture --smoke
```

The MCP setup journey only displays and copies the discovered Claude Desktop
configuration path and exact JSON object. It never writes that external file;
the captured setup state tells the reader to save the object and restart Claude
Desktop before verifying the connection.
