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

# Runs one named step, streaming its output, and returns whether it passed.
# The `CI-TIMING` line is for comparing runs: grep a build log for it.
export def ci-step [lane: string, name: string, body: closure]: nothing -> bool {
    print $"== ($lane): ($name) =="
    let started = (date now)
    let passed = (try {
        do $body
        true
    } catch {|error|
        print $error.msg
        false
    })
    let elapsed = (date now) - $started
    let verdict = if $passed { "passed" } else { "FAILED" }
    print $"== ($lane): ($name) ($verdict) in ($elapsed) =="
    print $"CI-TIMING lane=($lane) step=\"($name)\" seconds=($elapsed / 1sec | math round) result=($verdict)"
    $passed
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
    let emulated = $EMULATED_RUNNER
    let passed = ci-step $lane $"($platform) platform tests" {||
        with-plain-tmp {|| run-external "nu" "--no-config-file" $emulated $platform }
    }
    if not $passed { error make {msg: $"($lane) lane failed"} }
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
