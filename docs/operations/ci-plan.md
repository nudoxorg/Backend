# Backend CI and release workflow

Backend owns its lane scripts in `.config/ci/`. MachineConfigurations owns the
Concourse scheduling, worker configuration, Forgejo webhooks, and deployments.

## Developer checks

Open a PR against `canonical`. Forgejo webhooks wake the PR-head resource; a
five-minute heartbeat recovers missed events and bounded infrastructure failures.
Checks resolve and verify the current head before posting a verdict. New commits
need their own checks; a successful result on an older commit cannot authorize merging.

`concourse/backend-fast` is the required merge check. It runs structural checks and
`cargo check --locked --workspace --all-targets` for Linux x64, Windows GNU, and
Linux ARM64, including compilation of tests and benches. It does not execute them.
Branch protection also requires the PR to be up to date with canonical.

Two independently serialized fast jobs divide PR ownership by PR number. Each
has a separate worker-local cache and stable checkout path. At most two platform
compilations run at once per worker, each with four Cargo jobs. This reduces
queueing but PRs assigned to the same worker can still wait behind each other.

Unchanged tracked files recover their cached mtimes. Changed files keep their new
mtimes, and Cargo checks their fingerprints normally. Each platform has a separate
Cargo graph, including host build scripts. Caches are capped at 24 GiB after each
run; missing or evicted caches require a clean build. Low disk is reported as an
infrastructure problem, rather than a code failure.

There is no guaranteed completion time. Before these cache changes, required
checks took about 14–23 minutes once started; measure current warm and cold runs
in Concourse and the `CI-TIMING`/`CI-CACHE` log lines.

## Runtime results

After the current head passes fast checks, the PR runtime job runs:

| Status | Coverage |
|---|---|
| `concourse/backend-windows-wine` | Windows platform tests under Wine |
| `concourse/backend-arm64-emu` | ARM64 Linux platform tests under QEMU |
| `concourse/backend-linux` | Root flake checks and the native `backend test pr` selection |

These are currently advisory merge statuses. Main runs fast checks followed by
all runtime lanes, with an aggregate `concourse/backend` result. The PR aggregate
is `concourse/backend-lanes`. Heavy jobs still use host-wide disk admission;
fast jobs run independently of that lock.

The emulated lanes cover six platform crates, not the entire workspace. Exclusions
are explicit TOML entries with category/reason fields; `unclassified` entries are
unresolved coverage gaps. Wine/QEMU results do not replace native platform acceptance.
The native PR selection also excludes the declared GUI quarantine and deep-accuracy
suites. Those limitations must remain visible when assessing release readiness.

The completed `cec2fc288` run exposed 69 Wine test failures and 29 ARM64 failures;
a parallel PR run reported 31 ARM64 failures. Those are test verdicts, not successes.
Its native lane separately hit a compiler configure failure; another run could not
obtain disk admission. Diagnose the exact current revision rather than assuming
those older counts describe later canonical code. Infrastructure fixes do not
silently remove failures or broaden exclusions.

## Diagnosis and retry

Use the commit's Forgejo statuses to find the Concourse build. `CI-TIMING` identifies
slow steps; test summaries and captured stderr identify failures. Timeouts, failed
disk admission, and other infrastructure errors are reported separately and receive
limited retries. A deterministic code failure is not rebuilt on every heartbeat.
After fixing infrastructure on an unchanged commit, an operator can set its aggregate
status back to pending and trigger the owning job; inspect the exact head first.

Worker caches are accelerators. Source, evidence, and published release archives are
not cleanup targets. Completed PR tasks collect unrooted private Nix-store paths;
the host store and Cargo caches remain separate. Build logs retain their configured
history even after an expired container can no longer be inspected interactively.

## Release handoff

Follow [desktop-release-pipeline.md](desktop-release-pipeline.md). Select accepted
canonical source, pass the required source/platform checks, build the native artifact,
and record QA against its exact bytes. Submit a completed platform prerelease for
Concourse validation; publication/promotion is manual and does not rebuild it.

Pipeline definitions alone do not establish readiness: dedicated publishing and
promotion credentials, production routing, and end-to-end validation must be in
place before enabling release delivery. Apple signing/notarization stays on the Mac.
