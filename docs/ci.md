# Backend CI: execution, caches, and measurements

Backend's trunk is `canonical`. MachineConfigurations schedules the pipeline;
Backend owns the commands that it executes under `.config/ci/`.

| Stage | Execution | Evidence |
| --- | --- | --- |
| Fast PR gate | Structural flake checks; workspace/all-targets compilation for Linux x86, Windows GNU, Linux ARM64 | `concourse/backend-fast` on the exact PR head |
| Linux runtime | Root flake checks and `backend test pr` | Linux lane status and nextest results; deep-accuracy/quarantined tests are excluded |
| Windows runtime | Selected platform packages under Wine | Windows lane status; named exclusions in `emulated-windows-exclusions.toml` |
| ARM64 runtime | Selected platform packages under QEMU user emulation | ARM64 lane status; named exclusions in `emulated-arm64-exclusions.toml` |

Compilation is not a runtime test. Emulation does not prove every native OS
behavior. Release publishing/promotion is configured separately in
MachineConfigurations and needs credentials and candidate acceptance.

## Where to read the implementation

1. MachineConfigurations: `system/services/concourse/pipelines/apps.nix` defines
   pipelines, lanes, workers, and caches.
2. MachineConfigurations: `system/services/concourse/dsl/recipes.nix` renders the
   Concourse jobs, selects PR heads, posts statuses, and acquires heavy-job locks.
3. Backend: `.config/ci/fast.nu`, `cross-check.nu`, `linux.nu`, and `emulated.nu`
   execute the checks. `.config/ci/lib.nu` owns timeouts and lane cleanup.
4. MachineConfigurations: `system/services/concourse/build-storage.nix` and
   `build-storage-cleanup.py` manage host cleanup. `docs/backend-release.md`
   describes the storage policy and release setup.

Concourse's overview draws jobs and resources, not every command inside a task.
Our few job boxes contain multiple stages; a larger example graph does not by
itself establish better speed or coverage.

## Cache reuse and cleanup

Two serial PR workers have separate bounded fast caches. Main has its own fast
cache; the configured limit is 24 GiB for each. They retain Cargo targets,
registry inputs, and Zig outputs. `restore-mtimes.py` restores timestamps for
unchanged git blobs so fresh clones do not automatically invalidate Cargo's
source freshness checks. Cache loss is allowed and produces a cold build.

Main runtime builds also have Cargo/sccache support. Nix can realize existing
build outputs from its store/binary cache. These are different cache layers.

Fast compilation checks free space and may evict its own cache when over budget
or under disk pressure. The dev host's cleanup timer runs approximately every
five minutes, removes eligible idle managed Cargo graphs, and protects active
work. It retains sources, worktrees, corpora, and evidence. These policies do
not guarantee that arbitrary manual builds obey the reserve.

## Measure actual work, not polling jobs

The PR workers process multiple open PRs and skip heads that already have a
verdict. Their job duration includes setup and scanning and can contain zero
compilations. A green polling job can report a red individual PR. Never use its
green color or its duration as proof of a successful PR run.

`CI-TIMING` lines report stage durations. Each cross-target compilation now also
emits a `CI-METRIC` JSON record containing the head SHA, target, start time,
elapsed seconds, exit code, fresh artifacts, and rebuilt artifacts. Cargo's
artifact `fresh` flag measures reuse directly; it is not an sccache hit rate,
a count of passing tests, or a byte-saving estimate. Failed attempts have
partial artifact counts and must be analyzed separately.

Export the relevant build logs with `fly watch -b BUILD_ID`, then run:

```sh
python3 .config/ci/report.py run-1.log run-2.log
```

The report deduplicates exported attempts and separates passed and failed
checks. It reports compilation means, maximums, and artifact reuse, plus
separate named-step summaries for structural checks and runtime tests. It omits
p99 below 100 attempts; even larger samples need a representative workload and
time window. A few repeated warm runs are cache experiments, not a reliable
developer-latency p99.

Compilation metrics exclude queueing, clone time, shell realization, and runtime
testing. Named-step metrics include that step's compilation and execution, but
still exclude scheduler queueing and dev-shell preparation. Failed and timed-out
steps have separate summaries. End-to-end latency requires a recorded push/webhook timestamp joined
to the exact head's final status. A commit's authored timestamp is not a push
timestamp. Until that link is measured, do not advertise a push-to-green average
or p99. Report full-runtime and fast-gate results separately.

### Git source pins and emulated build capacity

`ci-cargo-git-hashes` checks that every Git package in `Cargo.lock` has a
SHA-256 source pin in `.config/nix/tools.nix`. The Linux packaging checks use
that map to vendor the whole lockfile, even for a restricted package build.
Adding a Git dependency requires a hash for each package supplied by that
repository. Coverage is checked in the fast lane; Nix verifies fetched source
contents against the pins during packaging.

Emulated CI compilation defaults to eight Cargo jobs on the 62 GiB worker;
an explicit `CARGO_BUILD_JOBS` overrides that CI setting. The repository's
two-job developer default stays unchanged. QEMU runtime execution uses four
test threads to avoid fsync and short-deadline contention. `compile-and-list`
timing separates compilation from the subsequent runtime tests.

Embedding protocol fixtures allow ten seconds for supervised Python startup
and positive inference under QEMU. Explicit deadline tests retain their short
budgets. The blocked-stdin test gives process creation a separate five-second
lifetime while preserving its 100 ms write deadline and two-second completion
assertion; previously a 100 ms process lifetime could expire during startup
before the blocked write was tested. The microbatch deadline fixture warms its
persistent worker and then requires two three-second responses to share one
five-second request budget. Each fits alone; both cannot. This retains the
exact two-call, deadline-error, no-partial-cache and recovery assertions while
leaving scheduling headroom. Production process limits are unchanged.

### Wine private-storage limitation

Runtime build 2558 on PR131 ran 588 Windows cases: 519 passed and 69 failed.
For 52 failures, captured diagnostics explicitly reported that a workspace
object lacked a protected DACL. An independent MinGW Win32 probe in the same
container created a directory, applied an owner-only ACL through its handle
with `SetSecurityInfo(DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION)`,
and read it back using `GetSecurityInfo` and `GetSecurityDescriptorControl`.
The standalone source is `.config/ci/probes/windows-dacl.c`. It creates a new
unique temporary directory and refuses to change an existing one.
Setting and reading both returned success, but the control word was `0x8004`: the
`SE_DACL_PROTECTED` bit (`0x1000`) was absent. Wine 11.0's `server/file.c` maps
file ACLs to Unix mode bits and reconstructs a descriptor with `SE_DACL_PRESENT`;
it does not preserve protected DACL semantics. This is an emulation limitation,
not a reason to weaken Backend's private-directory admission rules.

A second run on head `aeef7ea2e` exposed the typed cause for the remaining
17 failures: every one also reported a missing protected DACL. All 69 cases
are individually classified on that evidence.

The Windows emulation exclusion file lists each confirmed affected case
individually. These tests remain enabled on native Windows and Linux where
applicable. Other failures without captured causes still require diagnosis.
Wine coverage cannot certify the native Windows access-control boundary.

### Controlled fast-check timing comparison

On October 10, 2026 (EDT), PR131 head `2bad57dbe` was checked with the same
24 GiB cache budget and four Cargo jobs per target, before and after the CI
infrastructure change. This is one completed run per configuration, not an
average or p99.

| Phase | Two concurrent targets | Three concurrent targets |
| --- | ---: | ---: |
| Structural checks | 65 s | 55 s |
| Three-target compilation | 160 s | 84 s |
| Entire fast lane, including shell preparation | 299 s | 159 s |
| Fresh Cargo artifacts | 85–87% | about 87% |

For the second run, the required Forgejo verdict arrived 208 seconds after
the queue worker started. The worker job finished later, after cleanup/cache
finalization. Neither number includes time waiting for a worker. The first
actual push took about 402 seconds from push completion to the fast verdict.
A rerun requested through commit status is not a new push and cannot be used
as a push-to-result sample.

A verified-head heartbeat in the new pipeline finished in 36 seconds, with
`verification_needed=false` and no cached Cargo task. A recorded old heartbeat
that also skipped all heads took 289 seconds. Avoiding cache copies matters
even when no Rust compilation would have happened.

### Observed push-to-fast-verdict samples

Three subsequent pushes on October 10 (EDT) provide actual developer-latency
observations, including queueing:

| Head | Push to successful fast verdict | Queue context |
| --- | ---: | --- |
| `aeef7ea2e` | 230 s (3m50s) | Worker started about 10 s after push |
| `65152f80f` | 219 s (3m39s) | Worker started about 16 s after push |
| `116dee8c2` | 405 s (6m45s) | About 187 s waiting behind the previous cached task |

The mean of these three observations is about 285 s (4m45s); this small,
warm-cache sample is not a representative service-level average or p99.
The third run makes the remaining queue cost visible: the preceding worker
kept its serial slot for about 121 s after posting its fast verdict while
finalizing the approximately 15 GiB Cargo cache. The cache is reused, but
Concourse's cache-copy/finalization cost can delay a closely following push.
These timings cover the required fast gate, not full runtime or release delivery.
