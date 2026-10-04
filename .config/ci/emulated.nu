# Emulated test lanes, invoked as `nu .config/ci/emulated.nu windows` inside
# the `.#windows-wine` dev shell or `nu .config/ci/emulated.nu arm64` inside
# `.#arm64-emu`. The shell names the target's linker and the runner Cargo puts
# in front of each test binary (Wine, or QEMU user mode), so the tests run
# unchanged and do not know they are emulated.
#
# Only the `platform` selection runs here: the crates that own processes,
# storage, transport and the runtime, where a platform difference shows up.
# Emulation is several times slower than native, so the whole workspace stays
# in the Linux lane. Each exclusion below names the limit it works around.

def lanes []: nothing -> record {
    {
        windows: {
            target: "x86_64-pc-windows-gnu"
            # Each entry is a nextest filterset and the reason. The data file
            # names every test that fails under Wine, one by one, with its
            # first error: `wine-limit` where Wine lacks what Windows has,
            # `unclassified` where only a real Windows run can say whether
            # Wine or the code is wrong (those are handed to the Windows
            # owner). Delete an entry as soon as it is settled.
            excluded: (
                [{filter: "binary(/compile_fail/)", category: "harness", reason: "trybuild drives the host Cargo; there is no Cargo inside the Wine prefix"}]
                | append (open ($env.FILE_PWD | path join "emulated-windows-exclusions.toml") | get exclude)
            )
        }
        arm64: {
            target: "aarch64-unknown-linux-gnu"
            # As for Windows: `qemu-limit` where user-mode emulation cannot do
            # what an aarch64 machine does, `unclassified` otherwise.
            excluded: (
                [{filter: "binary(/compile_fail/)", category: "harness", reason: "trybuild drives the host Cargo against an aarch64 target; it checks compile errors, not the platform"}]
                | append (open ($env.FILE_PWD | path join "emulated-arm64-exclusions.toml") | get exclude)
            )
        }
    }
}

# The crates a platform difference lives in (B7 `platform`): process
# supervision, durable storage, the cluster transport and the runtime.
#
# Not backend-flow: it pulls in backend-engine and the language frontends
# (rust-analyzer, oxc), which take over an hour to build for Windows on their
# own, for tests that exercise no platform code.
def platform-packages []: nothing -> list<string> {
    [
        "backend-version"
        "backend-platform"
        "backend-store"
        "backend-runtime"
        "backend-cluster-transport"
        "backend-compile"
    ]
}


# Prints each failed test case from the run's JUnit report, with the first
# lines of its failure: the record that survives whatever the reporter did.
def print-junit-failures []: nothing -> nothing {
    let target = ($env.CARGO_TARGET_DIR? | default "target")
    let report = ($target | path join "nextest" "emulated" "junit.xml")
    if not ($report | path exists) {
        print $"   no JUnit report at ($report)"
        return
    }
    let suites = (open --raw $report | from xml | get content | where tag == "testsuite")
    let failed = ($suites | each {|suite|
        $suite.content | where tag == "testcase" | where {|case|
            $case.content | any {|child| $child.tag? in ["failure" "error"] }
        } | each {|case| {
            name: $"($suite.attributes.name) ($case.attributes.name)"
            message: ($case.content | where {|child| $child.tag? in ["failure" "error"] } | first | get attributes.message? | default "")
        } }
    } | flatten)
    print $"== ($failed | length) failed test cases \(from JUnit\) =="
    for case in $failed { print $"   FAIL ($case.name): ($case.message | str substring 0..200)" }
}

# Wine tools sit beside the runner the `.#windows-wine` shell names.
def wine-bin [tool: string]: nothing -> string {
    $env.CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER | path dirname | path join $tool
}

# Creates the prefix, then keeps one wineserver running for the whole run.
# Without it, each test's `wine` starts a server that outlives the test and
# holds nextest's output pipes open, so every test reads as a timeout. The
# server is started detached from this process's output for the same reason.
def start-wine []: nothing -> nothing {
    ^sh -c $"'(wine-bin wineboot)' --init </dev/null >/dev/null 2>&1"
    ^sh -c $"'(wine-bin wineserver)' -p </dev/null >/dev/null 2>&1 &"
    sleep 2sec
}

def stop-wine []: nothing -> nothing {
    try { ^sh -c $"'(wine-bin wineserver)' -k >/dev/null 2>&1" }
}

def main [
    platform: string # windows or arm64
]: nothing -> nothing {
    let lane = (lanes | get --optional $platform)
    if $lane == null {
        error make {msg: $"unknown platform ($platform); expected one of: (lanes | columns | str join ', ')"}
    }
    let filter = if ($lane.excluded | is-empty) {
        "all()"
    } else {
        $"not \(($lane.excluded | each {|entry| $"\(($entry.filter)\)" } | str join ' or ')\)"
    }
    let config = (mktemp --suffix .toml)
    # A test that hangs under emulation must not hold the lane for its whole
    # timeout: three slow periods, then it is killed and reported.
    # One retry absorbs a rare timing blip on a host emulating at a tenth
    # of native speed; nextest still names any test that needed it FLAKY.
    # The JUnit report names every failure even where the CI log keeps none
    # of nextest's own output (Concourse gives the task a terminal).
    let store = ($env.CARGO_TARGET_DIR? | default "target" | path join "nextest")
    $"[store]\ndir = \"($store)\"\n\n" + "[profile.emulated]\nfail-fast = false\nretries = 1\nslow-timeout = { period = \"60s\", terminate-after = 3 }\n\n[profile.emulated.junit]\npath = \"junit.xml\"\n" | save --force $config
    let packages = (platform-packages | each {|name| ["-p" $name] } | flatten)
    print $"== emulated: ($platform) \(($lane.target)\) on (platform-packages | length) crates =="
    let counts = ($lane.excluded | group-by category | transpose category entries | each {|row| $"($row.category): ($row.entries | length)" } | str join ", ")
    print $"   excluded ($lane.excluded | length) tests \(($counts)\); each with its reason:"
    for entry in $lane.excluded { print $"   - [($entry.category)] ($entry.filter): ($entry.reason)" }
    let started = (date now)
    if $platform == "windows" { start-wine }
    # What this run sees, so a failure before the first test is explainable
    # from the CI log alone: the tool versions, the runner environment, and
    # nextest's own listing of the selected tests through the runner.
    print $"   nextest: (^cargo nextest --version | lines | first)"
    $env | transpose name value | where {|row| $row.name =~ '^(CARGO_TARGET_DIR|CARGO_HOME|TMPDIR|WINEPREFIX|NEXTEST_|CARGO_TARGET_.*_RUNNER)' }
        | each {|row| print $"   env ($row.name)=($row.value)" }
    let listed = (do { ^cargo nextest list --locked --config-file $config --profile emulated --target $lane.target ...$packages -E $filter } | complete)
    print $"   nextest list exit=($listed.exit_code), (($listed.stdout | lines | length)) listed lines"
    if $listed.exit_code != 0 {
        print "   nextest list stderr (last 40 lines):"
        $listed.stderr | lines | last 40 | each {|line| print $"     ($line)" }
    }
    # Streamed, not captured: nextest prints each failure's output in its
    # final summary, and a long run should show progress as it goes.
    let passed = (try {
        ^cargo nextest run --locked --no-fail-fast --hide-progress-bar --config-file $config --profile emulated --target $lane.target ...$packages -E $filter
        true
    } catch { false })
    if $platform == "windows" { stop-wine }
    if not $passed { print-junit-failures }
    print $"== emulated: ($platform) (if $passed { 'passed' } else { 'FAILED' }) in ((date now) - $started) =="
    rm --force $config
    if not $passed { error make {msg: $"emulated ($platform) lane failed"} }
}
