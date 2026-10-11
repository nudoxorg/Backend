# Helpers shared by the CI lane runners in this directory. Each lane is
# invoked as `nu .config/ci/<lane>.nu` from the checkout root inside its dev
# shell; `use lib.nu *` resolves against this directory.

# The crates a platform difference lives in (B7 `platform`): process
# supervision, durable storage, the cluster transport and the runtime. The
# emulated lanes test only these.
#
# Not backend-flow: it pulls in backend-engine and the language frontends
# (rust-analyzer, oxc), which take over an hour to build for Windows on their
# own, for tests that exercise no platform code.
export def platform-packages []: nothing -> list<string> {
    [
        "backend-version"
        "backend-platform"
        "backend-store"
        "backend-runtime"
        "backend-cluster-transport"
        "backend-compile"
    ]
}

const STEP_TIMEOUT = "CI step timed out"

# Runs one named step, streaming its output, and returns whether it passed.
# The `CI-TIMING` line is for comparing runs: grep a build log for it. Its
# result is `passed`, `FAILED`, or `TIMEOUT` when run-bounded stopped the step;
# a TIMEOUT is also recorded for finish-lane.
export def --env ci-step [lane: string, name: string, body: closure]: nothing -> bool {
    print $"== ($lane): ($name) =="
    let started = (date now)
    let verdict = (try {
        do $body
        "passed"
    } catch {|error|
        print $error.msg
        if ($error.msg | str starts-with $STEP_TIMEOUT) { "TIMEOUT" } else { "FAILED" }
    })
    let elapsed = (date now) - $started
    print $"== ($lane): ($name) ($verdict) in ($elapsed) =="
    print $"CI-TIMING lane=($lane) step=\"($name)\" seconds=($elapsed / 1sec | math round) result=($verdict)"
    if (in-ci) {
        let metric = {
            kind: "ci-step"
            head_sha: $env.CI_HEAD_SHA
            lane: $lane
            step: $name
            started_at: ($started | format date "%+")
            seconds: ($elapsed / 1sec | math round --precision 3)
            outcome: $verdict
        }
        print $"CI-METRIC ($metric | to json --raw)"
    }
    if $verdict == "TIMEOUT" {
        $env.CI_TIMED_OUT = ($env.CI_TIMED_OUT? | default [] | append $name)
    }
    $verdict == "passed"
}

# Ends a lane from its steps' results. When every failed step ran out of
# time, it exits 75: the scheduler then reports a CI problem and retries the
# commit, instead of a failure that blames the code.
export def finish-lane [lane: string, results: list<bool>]: nothing -> nothing {
    let failed = $results | where {|passed| not $passed } | length
    let timed_out = $env.CI_TIMED_OUT? | default []
    if $failed > 0 and ($timed_out | length) >= $failed {
        print $"== ($lane): only time limits failed \(($timed_out | str join ', ')\); exit 75 =="
        exit 75
    }
    if $failed > 0 { error make {msg: $"($lane) lane failed"} }
}

# Runs an external command, and stops it, with everything it started, if it
# is still running after `limit`. Without a limit a hung step held the PR
# gate until Concourse's 180-minute task timeout, and every PR queued behind
# it went unchecked that round (nudox-backend-lanes-pr build 1656). Callers
# pass several times the step's warm time from the CI-TIMING lines, plus room
# for a cold cache, so only a hang reaches the limit.
#
# `timeout` signals the command's whole process group: TERM, then KILL two
# minutes later. Stdin is an empty pipe, not the task's terminal, so nothing
# in the background group can stop on a terminal read.
export def run-bounded [limit: duration, command: string, ...args: string]: nothing -> nothing {
    try {
        "" | run-external "timeout" "--kill-after=120s" $"($limit / 1sec | math round)s" $command ...$args
    } catch {|error|
        if ($error.exit_code? | default 0) == 75 {
            print $"CI problem: transient infrastructure failure in ($command); exit 75"
            exit 75
        }
        if ($error.exit_code? | default 0) in [124 137] {
            error make {msg: $"($STEP_TIMEOUT): ($command) was still running after ($limit)"}
        }
        error make {msg: $error.msg}
    }
}

# Whether this is a CI run: the scheduler exports CI_HEAD_SHA for every
# lane. The helpers below change the machine they run on, so they act only
# inside CI's throwaway task containers.
def in-ci []: nothing -> bool {
    ($env.CI_HEAD_SHA? | default "") | is-not-empty
}

# Process tests start children with PATH=/usr/bin:/bin and absolute
# /bin/true, /bin/sleep, /usr/bin/python3 and the like, as a Mac or an
# ordinary Linux machine provides them. The CI task image (nixos/nix) has
# only /bin/sh and /usr/bin/env, so those children could not start
# (Terminal(Exit), Process(Io), "grandchild PID was not published").
# Link the dev shell's coreutils and python3 into /bin and /usr/bin. Nix's
# coreutils dispatches on the name it is run by, and each link keeps the
# tool's own name. Never replaces an existing file, and does nothing on a
# machine that already has /bin/true.
export def provide-fhs-tools []: nothing -> nothing {
    if not (in-ci) or ("/bin/true" | path exists) { return }
    let sleep = which --all sleep | where type == "external" | get --optional 0.path
    if $sleep == null { return }
    let tools = ls ($sleep | path dirname) | get name
    for dir in ["/bin" "/usr/bin"] {
        mkdir $dir
        for tool in $tools {
            let link = $dir | path join ($tool | path basename)
            if not ($link | path exists) { ^ln -s $tool $link }
        }
    }
    let python = (
        $env.NUDOX_PYTHON?
        | default (
            which --all python3
            | where type == "external"
            | get --optional 0.path
            | default ""
        )
    )
    if ($python | is-not-empty) and not ("/usr/bin/python3" | path exists) {
        ^ln -s $python /usr/bin/python3
    }
    print $"ci: linked ($tools | length) coreutils tools and python3 into /bin and /usr/bin"
}

# Concourse keeps the container of a job's most recent failed build, and a
# lane's build output (over 100 GB for the Linux lane) with it, until that
# job's next build finishes. That next build then found too little free disk
# for the heavy-job lock and failed every lane (2026-10-04, builds 50 and
# 51). Delete the lane's build output when it ends, pass or fail. A managed
# cache (NUDOX_BUILD_CACHE_ROOT) is left alone.
export def reclaim-build-output []: nothing -> nothing {
    if not (in-ci) { return }
    let outputs = [
        ($env.CARGO_TARGET_DIR? | default ".local/target")
        ".local/zig-cache"
    ]
    | append (
        if ($env.NUDOX_BUILD_CACHE_ROOT? | default "" | is-empty) {
            [($env.HOME? | default "/root" | path join ".cache" "nudox")]
        } else { [] }
    )
    for output in $outputs {
        if ($output | path exists) { rm --recursive --force $output }
    }
}

# The Linux lane's persistent build cache (NUDOX_BUILD_CACHE_ROOT, set by the
# scheduler from a Concourse task cache): Cargo's build directories and
# sccache, kept from one run to the next. Empty when this run has none.
def build-cache-root []: nothing -> string {
    $env.NUDOX_BUILD_CACHE_ROOT? | default ""
}

# restore-mtimes.py's manifest, beside the cache it describes.
def source-mtimes-manifest [root: string]: nothing -> string {
    $root | path dirname | path join "source-mtimes.json"
}

# Bounds the build cache. It grows with every Cargo graph change and is
# never pruned by Cargo, and the disk is shared with Forgejo, Postgres and
# the other lanes. Past `max_gib` it is deleted, and the next build is cold.
export def cap-build-cache [max_gib: int = 80]: nothing -> nothing {
    let root = build-cache-root
    if not (in-ci) or ($root | is-empty) or not ($root | path exists) { return }
    let gib = ^du -s --block-size=1G $root | split row "	" | first | into int
    if $gib > $max_gib {
        print $"ci: build cache is ($gib) GiB, over ($max_gib); starting cold"
        rm --recursive --force $root (source-mtimes-manifest $root)
    } else {
        print $"ci: build cache is ($gib) GiB"
    }
}

const RESTORE_MTIMES = path self "restore-mtimes.py"

# Prints how much disk this lane's build output and the build cache hold,
# as a CI-DISK line, before reclaim-build-output deletes the former. The
# heavy lock's per-lane free-disk requests and the cache cap are sized from
# these numbers. CI only.
export def report-build-size []: nothing -> nothing {
    if not (in-ci) { return }
    let measured = [
        {
            name: "target"
            path: ($env.CARGO_TARGET_DIR? | default ".local/target")
        }
        {
            name: "cache"
            path: (build-cache-root)
        }
        {
            name: "home-cache"
            path: ($env.HOME? | default "/root" | path join ".cache" "nudox")
        }
    ]
    | where {|entry| ($entry.path | is-not-empty) and ($entry.path | path exists) }
    | each {|entry|
        let gib = ^du -s --block-size=1G $entry.path | split row "\t" | first | into int
        $"($entry.name)=($gib)GiB"
    }
    print $"CI-DISK lane=linux ($measured | str join ' ')"
}

# Restores unchanged sources' mtimes from the cache's manifest, so Cargo
# reuses the cached build output instead of seeing a fresh clone as all new
# (restore-mtimes.py). Only with a build cache, only in CI.
export def restore-source-mtimes []: nothing -> nothing {
    let root = build-cache-root
    if not (in-ci) or ($root | is-empty) { return }
    let python = $env.NUDOX_PYTHON? | default "python3"
    run-external $python $RESTORE_MTIMES (source-mtimes-manifest $root)
}

# Runs `body` with the temporary directory at plain /tmp. `nix develop` points
# TMPDIR at a nested directory: a check derivation's sandbox cannot see it
# ("$env.PWD points to a non-existent directory"), and test socket paths under
# it outgrow sun_path's 107 bytes (EndpointTooLong).
export def with-plain-tmp [body: closure]: nothing -> any {
    with-env {TMPDIR: "/tmp", TMP: "/tmp", TEMP: "/tmp", TEMPDIR: "/tmp"} { do $body }
}

# This directory's emulated test runner, located from this file rather than
# the caller's working directory.
const EMULATED_RUNNER = path self "emulated.nu"

# One emulated lane (`windows-wine.nu`, `arm64-emu.nu`): skipped, and passing,
# when the change cannot reach the platform crates; otherwise the platform's
# test selection through `emulated.nu`.
export def emulated-lane [lane: string, platform: string]: nothing -> nothing {
    if not (emulated-lanes-needed) {
        print $"== ($lane): skipped, the change reaches no platform crate =="
        return
    }
    stop-if-superseded $lane
    provide-fhs-tools
    let emulated = $EMULATED_RUNNER
    let passed = ci-step $lane $"($platform) platform tests" {||
        with-plain-tmp {|| run-bounded 30min "nu" "--no-config-file" $emulated $platform }
    }
    reclaim-build-output
    finish-lane $lane [$passed]
}

# Whether the PR this run is verifying has moved on to a newer commit, so the
# rest of the run would verify a commit nobody will merge. The scheduler
# exports CI_PR_NUMBER and CI_HEAD_SHA beside its Forgejo credentials; when any
# is missing, or the API does not answer, the answer is "no" and the lane runs
# to the end, which is never wrong, only slower.
export def superseded []: nothing -> bool {
    let needed = [
        CI_PR_NUMBER
        CI_HEAD_SHA
        FORGEJO_URL
        FORGEJO_TOKEN
        FORGEJO_OWNER
        FORGEJO_REPO
    ]
    if ($needed | any {|name| ($env | get --optional $name | default "") | is-empty }) {
        return false
    }
    let url = $"($env.FORGEJO_URL)/api/v1/repos/($env.FORGEJO_OWNER)/($env.FORGEJO_REPO)/pulls/($env.CI_PR_NUMBER)"
    let head = (try {
        http get --headers [Authorization $"token ($env.FORGEJO_TOKEN)"] $url | get head.sha
    } catch { "" })
    ($head | is-not-empty) and $head != $env.CI_HEAD_SHA
}

# Stops the lane when a newer commit has been pushed to the PR.
export def stop-if-superseded [lane: string]: nothing -> nothing {
    if (superseded) {
        error make {msg: $"($lane): superseded, the PR head is no longer ($env.CI_HEAD_SHA)"}
    }
}

# Paths this change touches, against the scheduler's CI_DIFF_BASE
# (`origin/<base>...HEAD`), or null when that cannot be worked out: no base,
# or a shallow clone without the merge base.
export def changed-files []: nothing -> any {
    let spec = $env.CI_DIFF_BASE? | default ""
    if ($spec | is-empty) { return null }
    try {
        ^git diff --name-only $spec | lines | where {|path| $path | is-not-empty }
    } catch { null }
}

# Whether the emulated (Wine, QEMU) lanes have anything to test in this
# change. Fails closed: they run unless every changed path is inside a
# workspace package that the platform packages neither are nor depend on
# (normal, build or dev). A path in no workspace package (Cargo.lock, .config,
# the flake, vendor/), an unknown diff, or any doubt runs them.
export def emulated-lanes-needed []: nothing -> bool {
    emulated-lanes-needed-for (changed-files)
}

export def emulated-lanes-needed-for [files]: nothing -> bool {
    if $files == null or ($files | is-empty) { return true }
    let metadata = (try {
        ^cargo metadata --no-deps --format-version 1 --locked | from json
    } catch { null })
    if $metadata == null { return true }
    let root = $metadata.workspace_root
    let packages = $metadata.packages | each {|package| {
        name: $package.name
        dir: ($package.manifest_path | path dirname | path relative-to $root)
        depends: ($package.dependencies | where {|dependency| ($dependency.path? | default "") | is-not-empty } | get name)
    } }
    mut closure = (platform-packages)
    loop {
        let current = $closure
        let grown = $packages
        | where {|package| $package.name in $current }
        | get depends
        | flatten
        | append $current
        | uniq
        if ($grown | length) == ($current | length) { break }
        $closure = $grown
    }
    let platform = $closure
    let owners = $files | each {|file|
        let owning = $packages
            | where {|package| $package.dir != "" and ($file | str starts-with $"($package.dir)/") }
            | sort-by --reverse {|package| $package.dir | str length }
            | get --optional 0
        # "" rather than null: `each` drops nulls, which would silently pass
        # an unowned path instead of running the lanes for it.
        if $owning == null { "" } else { $owning.name }
    }
    ($owners | any {|owner| $owner == "" or $owner in $platform })
}
