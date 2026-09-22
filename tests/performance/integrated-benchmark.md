# Integrated benchmark runner

`src/bin/integrated.rs` drives the production Tantivy, Turso, CAS, library,
CLI, MCP, and optional desktop GUI contracts over real source files. It emits
`nudox.integrated-benchmark.v4` JSON and a concise Markdown report beside it;
timings are descriptive measurements and correctness assertions are recorded
beside every result. The v4 lanes add ignore-aware discovery, fresh/graceful/
SIGKILL/offline Turso lifecycle checks, CLI/MCP parity for every admitted
language, bounded RSS/file-descriptor sampling, throughput/reuse rows, and an
explicit backend_1 comparison row.

Run the checked-in smoke profile with the pinned Rust shell. This runner uses nix shell, never nix develop, so the exact environment remains reviewable:

```console
CARGO_TARGET_DIR=.local/integrated-target \
nix shell '.#luna-tools' --command cargo run --locked --offline \
  -p backend-performance-tests --bin integrated -- \
  --profile smoke --output tests/performance/results/integrated-smoke.json
```

To include a live desktop harness, pass its executable explicitly. The GUI
slice requests the package route at the exact `1440x1000@1` viewport and only
reports success when the harness writes a verified seven-frame package
manifest:

```console
CARGO_TARGET_DIR=.local/integrated-target \
nix shell '.#luna-tools' --command cargo run --locked --offline \
  -p backend-performance-tests --bin integrated -- \
  --profile smoke --output tests/performance/results/integrated-smoke.json \
  --gui-bin /absolute/path/to/backend-desktop-gui-harness
```

Use `--profile full --require-complete` for 15 measured repetitions. Full
implies the promotion gate and requires a release build, seven-lane configured
corpus, authenticated Unix locald CLI/MCP transport, and
verified GUI artifacts. Smoke may report status=partial and exit successfully;
require-complete writes its artifact before returning nonzero for missing or
failed cells. Tail p95/p99 values are null until 20 samples are available and
carry an insufficient-tail-samples marker. Build metadata records profile,
target directory, and dirty state. Ingest and search phases are named for
their actual boundaries, including durable_publish_and_reopen,
warm_in_memory_build, and first_in_memory_search.
The production Tantivy API currently exposes exact, prefix, and full-text
queries; fuzzy construction is unavailable and is recorded as unsupported.
Ingest emits explicit one-file add, no-op, and delete rows. Delta rows include
the measured source bytes read and durable bytes written, whether the path hit
an immutable cache, and the number of semantic rows avoided by the delta-local
path; a delete must advance to a distinct root and remove exactly one document.
The discovery lane uses the shared package/source policy against a fixture with
root and nested `.gitignore` files, and reports each generated root separately
(`.next`, `.angular/cache`, `dist`, `build`, `bin`, `obj`, `coverage`, and the
other default tool roots). Claimed oversized source is retained as a typed
`TooLarge` row rather than failing the rest of the project.

For a promotion run, use the release profile explicitly:

```console
CARGO_TARGET_DIR=.local/integrated-target \
nix shell '.#luna-tools' --command cargo run --locked --offline --release \
  -p backend-performance-tests --bin integrated -- \
  --profile full --require-complete --output tests/performance/results/integrated-full.json
```

When Nix corpus variables (`NUDOX_*_CORPUS_DIR`) are set, the runner uses those
real multilingual trees. Otherwise it uses this workspace's checked-in source
tree, filters files above the canonical 32 KiB relation-row bound, and caps the
large class at 256 files or 4 MiB for a deterministic smoke duration. The
artifact names that source kind explicitly; the fallback is a real source-tree
corpus, not a mock.

Acquisition runs synchronized 1/8/32-caller rounds through the production
RegistryOwner and loopback HTTP transport, then records warm-cache and offline
restart rows with receipt, delta, target-root, and archive identities. Each
round requires one metadata response, one archive response, one leader, the
remaining synchronized callers as followers, exact response-byte accounting,
and an immutable artifact whose bytes exactly match the fixture archive. The
acquisition rows also report measured request count, response/downloaded and
reused bytes, durable bytes written, delta-row count, signed registry storage
growth, and whether the phase performed no remote/archive work. Warm and
restart rows therefore expose no-op reuse separately from any journal bytes
written during receipt accounting. `requests`, `response_bytes`,
`written_bytes`, `delta_rows`, `storage_growth_bytes`, and `no_op_work` are
null for the CAS-only rows because those values are outside the registry
transport boundary. GUI phase fields are populated only from explicit harness
phase timings; the full child-process wall is kept separate. GUI promotion
requires captured_frames, verified_frames, and verified PNG artifacts all equal
seven. null allocation fields mean the public Rust boundary does not expose an
allocation counter; GUI child-process resources remain outside host totals. The
fallback source tree is marked incomplete for promotion.

The runner records the full command in `build.command`, the Nix invocation in
`build.nix_shell`, host CPU/RSS/FD data in `hardware`, and storage bytes on
each Turso row. It writes `integrated-smoke.md` when the JSON output path is
`integrated-smoke.json`; keep both files together when comparing runs. The
backend_1 row is `unavailable` unless that checkout has the same performance
manifest and command, so no cross-branch numbers are inferred.
