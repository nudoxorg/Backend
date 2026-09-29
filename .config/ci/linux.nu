# Linux CI lane runner, invoked as `nu .config/ci/linux.nu` inside the
# `.#compiler` dev shell. The lane is the whole native x86_64/aarch64 Linux
# gate: the root flake's structural checks, then `backend test pr` (every
# workspace test except the deep-accuracy suites and the PR quarantine).
#
# MachineConfigurations only schedules this file; what the lane runs lives
# here so a Backend PR can change it.

# Prints the process output the CLI captured for a failed step. The CLI keeps
# a child's stdout/stderr out of the log (process-require writes them beside a
# manifest instead), and the CI worktree is deleted with the container, so
# this is the only chance to show why a step failed.
def print-captured-failures []: nothing -> nothing {
    let captured = (glob ".local/process-failures/*/std*.txt")
    if ($captured | is-empty) {
        print "no .local/process-failures/*: the failure was before the CLI, so the text above is all of it"
        return
    }
    for file in $captured {
        print $"---- ($file) ----"
        # Bounded: a full cargo failure can run to tens of MB and Concourse
        # stores every log line. Keep the verdict summary plus a large tail.
        let output = (open --raw $file | lines)
        let verdicts = ($output | where {|line|
            ($line | str contains 'FAIL [') or ($line | str contains 'TIMEOUT [') or ($line | str contains 'tests were not run')
        })
        if not ($verdicts | is-empty) {
            print '== nextest failure summary =='
            $verdicts | last 100 | str join (char nl) | print
        }
        # Each failing test's own output comes long before the tail (nextest
        # prints it as the test fails), so pull every panic with the lines
        # that explain it: the assertion, left/right, and the message.
        let panics = ($output | enumerate | where {|row| ($row.item | str contains 'panicked at') or ($row.item | str contains 'stderr ───') } | get index)
        if not ($panics | is-empty) {
            print $"== ($panics | length) panics \(first 60, 25 lines each\) =="
            for start in ($panics | first 60) {
                $output | skip ([($start - 3) 0] | math max) | first 28 | str join (char nl) | print
                print '--'
            }
        }
        $output | last 300 | str join (char nl) | print
    }
}

# Runs one named step, streaming its output, and returns whether it passed.
def step [name: string, body: closure]: nothing -> bool {
    print $"== linux: ($name) =="
    let started = (date now)
    let passed = (try { do $body; true } catch {|error| print $error.msg; false })
    print $"== linux: ($name) (if $passed { 'passed' } else { 'FAILED' }) in ((date now) - $started) =="
    $passed
}

def main [
    --skip-flake-check # run only the test step (the flake check is already cached on the host store)
]: nothing -> nothing {
    # The flake closes over ~1,100 pinned corpus archives; the default 1,024
    # soft descriptor limit is exhausted before any check fails.
    ulimit --file-descriptor-count --soft 65536

    let flake = if $skip_flake_check {
        true
    } else {
        # `nix develop` points TMPDIR at its own /tmp/nix-shell.* directory.
        # A local (preferLocalBuild) check derivation then gets a build dir
        # under it that its sandbox cannot see, and nushell builders fail
        # with "$env.PWD points to a non-existent directory". Check with the
        # plain /tmp the flake check always had outside the shell.
        step "root flake check" {||
            with-env {TMPDIR: "/tmp", TMP: "/tmp", TEMP: "/tmp", TEMPDIR: "/tmp"} {
                run-external "nix" "flake" "check" "-L" "path:."
            }
        }
    }

    # "private-debug" makes the CLI write the child's stdout/stderr beside its
    # manifest; the default "metadata-only" keeps just byte counts, so a
    # failure would reach CI as a bare exit status.
    let tests = (with-env {BACKEND_PROCESS_ARTIFACT_POLICY: "private-debug"} {
        step "backend test pr" {|| run-external "backend" "test" "pr" }
    })
    if not $tests { print-captured-failures }

    if not ($flake and $tests) {
        error make {msg: "linux lane failed"}
    }
}
