# Normalizes external command execution into typed Nushell records.
# Preserves program, arguments, status, standard output, and standard error.
# Makes every caller explicitly decide whether a failed process is acceptable.

# Executes a program through file-backed capture and returns a bounded process result.
def process-result [program: string, arguments: list<string> = []]: nothing -> record {
    if ($program | str trim | is-empty) {
        tooling-fail "invalid-program" "external program name may not be empty"
    }
    let started_at = (date now)
    let capture = local-root | path join "process-captures" (random uuid)
    mkdir $capture
    ^chmod "0700" $capture
    let stdout_path = $capture | path join "stdout"
    let stderr_path = $capture | path join "stderr"
    try {
        run-external $program ...$arguments o> $stdout_path e> $stderr_path
    } catch {
        # LAST_EXIT_CODE retains the exact external status; no error prose crosses this boundary.
    }
    let status = $env.LAST_EXIT_CODE? | default 127
    let stdout_bytes = ls $stdout_path | get size | first | into int
    let stderr_bytes = ls $stderr_path | get size | first | into int
    let operation = $env.BACKEND_AGENT_TOOL? | default ($program | path basename)
    let maximum = control-plane | get observability.process.maximumCaptureBytes
    if ($stdout_bytes + $stderr_bytes) > $maximum {
        let fingerprint = {status: $status, stdout_bytes: $stdout_bytes, stderr_bytes: $stderr_bytes} | to json | hash sha256
        record-tooling-event $operation $started_at 1 $stdout_bytes $stderr_bytes "result" "external-process" $fingerprint | ignore
        rm --recursive $capture
        tooling-fail "process-output-limit" $"($program | path basename) emitted ($stdout_bytes + $stderr_bytes) bytes; the combined limit is ($maximum)" "route a declared artifact directly to .local instead of command output"
    }
    let stdout = open --raw $stdout_path
    let stderr = open --raw $stderr_path
    let fingerprint = if $status == 0 {
        ""
    } else {
        {
            status: $status
            stdout: ($stdout | hash sha256)
            stderr: ($stderr | hash sha256)
        } | to json | hash sha256
    }
    record-tooling-event $operation $started_at $status $stdout_bytes $stderr_bytes "result" "external-process" $fingerprint | ignore
    rm --recursive $capture
    {
        program: $program
        arguments: $arguments
        status: $status
        stdout: $stdout
        stderr: $stderr
        stdout_bytes: $stdout_bytes
        stderr_bytes: $stderr_bytes
    }
}

# Executes a program and raises a lossless error when it exits unsuccessfully.
def process-require [program: string, arguments: list<string> = []]: nothing -> record {
    let result = (process-result $program $arguments)
    if $result.status != 0 {
        let policy = (
            $env.BACKEND_PROCESS_ARTIFACT_POLICY?
            | default (control-plane | get observability.process.defaultArtifactPolicy)
        )
        let admitted = control-plane | get observability.process.artifactPolicies
        if $policy not-in $admitted {
            tooling-fail "invalid-process-artifact-policy" $"unknown process artifact policy: ($policy)"
        }
        let identity = ({
      program: ($program | path basename)
      status: $result.status
      stdout: ($result.stdout | hash sha256)
      stderr: ($result.stderr | hash sha256)
    } | to json | hash sha256)
        let run_id = $"(date now | format date '%Y%m%dT%H%M%S%.fZ')-($identity | str substring 0..15)-(random uuid)"
        let directory = local-root | path join "process-failures" $run_id
        mkdir $directory
        ^chmod "0700" $directory
        let manifest = {
            schema: 1
            program: ($program | path basename)
            argument_count: ($arguments | length)
            status: $result.status
            stdout_bytes: $result.stdout_bytes
            stderr_bytes: $result.stderr_bytes
            stdout_sha256: ($result.stdout | hash sha256)
            stderr_sha256: ($result.stderr | hash sha256)
            artifact_policy: $policy
        }
        let artifact = $directory | path join "failure.json"
        $manifest | to json --indent 2 | save --raw $artifact
        ^chmod "0600" $artifact
        if $policy == "private-debug" {
            $result.stdout | save --raw ($directory | path join "stdout.txt")
            $result.stderr | save --raw ($directory | path join "stderr.txt")
            ^chmod "0600" ($directory | path join "stdout.txt") ($directory | path join "stderr.txt")
        }
        let cause = {
            code: "backend::external-process"
            msg: $"($program) exited with status ($result.status)"
            help: $"bounded failure manifest: ($artifact)"
            inner: []
        }
        tooling-fail "process-failed" $"external process failed with status ($result.status)" $"inspect ($artifact); opt into private-debug only for non-sensitive local tools" --inner [$cause]
    }
    $result
}
