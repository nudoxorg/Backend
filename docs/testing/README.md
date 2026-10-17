# Running the test tiers

The testing rework starts with a small Linux smoke runner. It builds the CLI, MCP
and desktop binaries for the host once, then checks each public surface:

- CLI: `--help` exits successfully and prints usage.
- MCP: stdio `initialize` and `tools/list` succeed, followed by clean EOF exit.
- GUI: a visible window paints under Xvfb/software Vulkan and exits successfully
  through the public Quit shortcut.

From the Backend checkout:

```sh
python3 .config/ci/test.py smoke
```

The Nix shell supplies the pinned Rust toolchain and Linux graphics dependencies.
The runtime checks use temporary HOME/XDG directories and an empty project, so
startup does not index the repository or reuse your normal workspace. Correctness
of indexing, resumption, health, search and paging is checked separately by T2.

For the small driver regression suite, without compiling the product:

```sh
python3 .config/ci/test_smoke.py
```

The warm target is under one minute. A cold build is substantially longer. CI owns
its dedicated, serialized build caches; local edits can use the default target
directory. Do not point a local run at another worktree's mutable cache. Keep
build caches outside the flake source tree to avoid copying them during Nix setup.

## CI selection during rollout

Backend PRs changing only the explicit documentation paths (`docs/`, `README.md`,
`LICENSE`) receive `T0: docs-only; no build required` on the existing CI contexts.
The dispatcher verifies the complete changed-file list for the exact head first.
Mixed changes, source-to-doc renames and unknown or incomplete diffs run normal CI.
Markdown outside that explicit list does not automatically skip CI.

T1/T2 are live advisory checks; required fast remains unchanged under D11.
Both retired Backend pipelines were removed under D13; the required fast,
advisory T1/T2 and scheduled deep pipelines remain active.

T1 can publish its successful build for a later task by setting
`CI_T1_ARTIFACT_ROOT` to an output directory outside the checkout. The runner
creates `<output>/<full-source-sha>/` only after all three smoke checks pass.
The Linux artifact contains those three binaries, the smoke driver, a Nix
runtime export and a manifest recording the source SHA, host target, Rust
version, payload sizes and SHA-256 hashes. It contains no nextest archive:
T1 does not compile the old test binaries.

A consumer can verify the identity and bytes without executing anything:

```sh
python3 .config/ci/t1_artifacts.py verify /path/to/artifact \
  --source-sha FULL_COMMIT_SHA --target x86_64-unknown-linux-gnu
```

Add `--smoke` in an isolated Linux Nix task to import the bundled runtime and
run CLI/MCP/GUI checks against the published binaries, with no Cargo invocation.
Only explicit runtime paths are retained; credentials and workspace settings
are excluded. The artifact budget is 8 GiB. Publication refuses an existing
destination, a dirty tracked checkout or unresolved libraries. Build-once producer/consumer wiring is deployed; required-check cutover remains
pending (W3.2).

## Preview selection for a diff

```sh
python3 .config/ci/test.py select --base origin/canonical
python3 .config/ci/test.py select --base origin/canonical --worktree
```

This read-only JSON preview resolves exact base/head commits and uses their
merge base. It keeps deleted paths and both sides of renames; `--worktree` also
includes staged, unstaged and untracked files. It never enters Nix, builds,
executes tests, fetches refs or accepts snapshots. Fetch the intended base first.

The current canonical-PR policy only skips explicit `docs/`, `README.md` and
`LICENSE` changes. Empty, unavailable or incomplete diffs run full checks. All
product/configuration/unknown paths retain full T1/T2 and the current optional
legacy platform lanes; the JSON lists the actual local integration catalog.
Required fast and advisory T1/T2 contexts remain as deployed under D11. Scheduled
T3 is independent. This previews policy rather than queue state, host health or
a CI verdict. It does not yet narrow product tests by symbols/reverse dependencies
or replace the dispatcher's API completeness and exact-head checks. The reusable
selector is the starting point for W6; later CI wiring must keep those checks.

## Integration: reuse the smoke binaries

```sh
python3 .config/ci/test.py integration --bin-dir .local/target/debug
python3 .config/ci/test.py integration --bin-dir /path/to/artifact/bin --case cli-usage
python3 .config/ci/test.py integration --bin-dir /path/to/artifact/bin --case python-blinker
python3 .config/ci/test.py list
```

Integration enters the pinned surface shell and never invokes Cargo. Use the
actual directory selected by your smoke run if you set `CARGO_TARGET_DIR`.
Both smoke and integration return a failing exit code when their own tier fails;
smoke no longer runs an integration check whose failure it ignores. In CI the
artifact manifest must be verified before selecting its binaries.

Integration defaults to two concurrent isolated cases. Use `--jobs 1` for a
serial diagnostic or explicitly select up to `--jobs 4`;
this changes scheduling while preserving per-case comparisons and aggregate
failure. Cases reuse the supplied binaries and never build the product.

Use `--report /path/report.json` for machine-readable performance evidence.
`--case` is repeatable; unknown cases fail. `--plan` prints the exact command
as JSON without entering Nix, running binaries or rewriting snapshots. CI can
use `--in-shell` after entering the corresponding declared Nix shell.

## Source-bound CI summary evidence

```sh
python3 .config/ci/test.py integration --bin-dir /path/to/artifact/bin \
  --ci-report /path/case-evidence.json --source-sha FULL_COMMIT_SHA
```

This optional report verifies the artifact inventory, payload hashes, Linux target
and source SHA, and requires a clean tracked harness checkout at that commit before any
case runs. It records the manifest digest, selection scope, every selected case's
verdict, actual operation timings/CPU/memory, index delta from the reviewed
baseline and bounded readable snapshot differences/failure details. Setup errors
remain failed cases without invented measurements. Failed peers do not disappear
from the report. Publication is atomic, including when the tier fails.

The existing `--report` remains a performance map for compatibility. `--ci-report`
requires ordinary comparison and cannot be combined with `--update`. These files
are evidence inputs for the Internal PR summary tool; producing a file neither
posts a comment nor changes a required status. The CI adapter must retain the
report and source identity together. Full differences/metrics remain in CI logs.

## Review an intentional snapshot change

```sh
python3 .config/ci/test.py integration --bin-dir /path/to/bin --case polyglot --update
git diff -- tests/surface/snapshots/
python3 .config/ci/test.py integration --bin-dir /path/to/bin --case polyglot
```

Explain every semantic change and review performance baselines in that diff.
The ordinary integration command compares snapshots and never accepts changes.

## Deep checks: retained laws outside the PR path

```sh
python3 .config/ci/test.py deep
```

This bounded T3 bootstrap compiles the CLI test binary and runs the four retained
`process::index_wait_tests::` laws with nextest: public grammar, later failure, saved identity
and disconnect/immediate failure. Cargo and nextest concurrency are both two;
the step has a 30-minute cold-build bound and the same memory/disk admission
floors as T1. It does not run native compilation of fixture projects.

This is not the complete release gate. Full deep corpora, release builds,
packaging and macOS/Wine/QEMU placement remain pending. No old tests are deleted
or silently quarantined by this command. These tier commands currently target
Linux with Nix; native macOS execution remains a separate workstream.

## Source-bound catalog for CI consumers

`python3 .config/ci/test.py list` keeps the simple local tier/name list.
For automation, `python3 .config/ci/test.py list --source-sha FULL_HEAD_SHA`
returns schema1, exact `source_sha`, the committed cases file's `catalog_sha256`
and `tiers`. This requires the matching clean tracked checkout and reads the
cases blob from that commit, so an untracked replacement cannot acquire its
identity. No build, test, network request or PR comment is launched. Missing or
malformed names, duplicates and source/checkout mismatch fail explicitly.
The deep list currently names the implemented CLI laws; it does not claim to
list every nightly platform/release test. A bot must bind its resolver to the
same PR head before suggesting or scheduling these names. Dispatch authorization,
replay protection and actual comment-triggered execution remain separate work.


CI also retains `CI-ARTIFACT` byte inventories and `CI-PERF-CONTEXT` observations.
The former binds each product binary, runtime export and smoke driver hash to
its source and manifest, even after task payloads expire. It does not retain the
payload itself. Source-verified T2 report mode records bounded CPU model/affinity,
observed clock ranges, cgroup limits/pressure/counters, host CPU/I/O pressure and
a top-ten live owner-thread CPU sample at case end. Cgroup values are read only
when `/proc/self/cgroup` maps unambiguously into the visible cgroup2 mount and
the resolved `cgroup.procs` lists this process; `cgroup_scope` records the path
and mount root, otherwise those values are reported unavailable. Missing data
is explicit; exited threads are absent.
Context events are capped at64KiB and never alter a
snapshot, baseline, tolerance or case verdict. Use them to investigate failures,
not to automatically exempt a slow result. Physical CPU frequency samples do
not establish the clock rate throughout an operation.

For a retained payload, inspect the same verified inventory without importing or
launching it:
`python3 .config/ci/t1_artifacts.py inventory ARTIFACT_DIR --source-sha FULL_SHA`.

Integration performance runs select the highest static CPU-capacity group within
Linux's existing allowed CPU set, before starting case threads or product children.
They prefer complete kernel `cpu_capacity` metadata, then complete advertised
`cpuinfo_max_freq` metadata. Sampled clock speeds and machine names are not inputs.
Homogeneous or incomplete topology preserves the existing grant; platforms without
an affinity API keep their normal scheduling. Placement failures are errors, and the
parent's original affinity is restored after the run. `SURFACE-CPU-PLACEMENT` records
the metric, allowed/selected CPUs and reason. Baselines and tolerances do not change.

This controls a measured source of variability on hybrid workers: a fixed P/E/E/P
same-artifact experiment indexed in39.34/63.57/63.57/39.40s, with owner CPU
19.82/32.95/33.15/19.89s. The experiment used one logical CPU per condition; ordinary
integration uses every allowed CPU in its selected capacity group. It does not
prove the cause of earlier unrestricted failures or eliminate I/O contention.
T3/platform execution continues to exercise its declared worker scheduling.
