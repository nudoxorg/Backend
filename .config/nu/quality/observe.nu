# Executes only Nix-declared measurements and records their exact environment.
# Gives verifier roles unique immutable runs with comparable facts and typed targets.
# Prevents arbitrary executables, packages, arguments, and baseline path traversal.

# Resolves one closed measurement target and proves its expected operation kind.
def measurement-target [name: string, kind: string]: nothing -> record {
    let policy = control-plane | get observability.measurements
    if $name !~ $policy.namePattern {
        tooling-fail "invalid-measurement-name" $"measurement target has an invalid name: ($name)"
    }
    let targets = $policy.targets | transpose name declaration | where name == $name
    if ($targets | length) != 1 {
        tooling-fail "unknown-measurement-target" $"Nix declares no measurement target named ($name)" "add a typed target to .config/nix/control.nix"
    }
    let target = $targets | first
    if $target.declaration.kind != $kind {
        tooling-fail "measurement-kind-mismatch" $"($name) is ($target.declaration.kind), not ($kind)"
    }
    $target.declaration | merge { name: $target.name }
}

# Captures the environment facts required to compare two measurements honestly.
def measurement-environment [target: record]: nothing -> record {
    let host = (sys host)
    let cpu = (sys cpu)
    let memory = (sys mem)
    let revision = process-require "git" ["rev-parse" "HEAD"] | get stdout | str trim
    let dirty = not (process-require "git" ["status" "--porcelain=v1"] | get stdout | is-empty)
    let rustc = $env.BACKEND_STABLE_CARGO | path dirname | path join "rustc"
    let compiler = process-require $rustc ["--version" "--verbose"] | get stdout | lines
    let target_triple = (
        $compiler
        | where {|line| $line | str starts-with "host:" }
        | first
        | split row ": "
        | last
    )
    {
        host: ($host.hostname? | default "unknown")
        kernel: $"($host.long_os_version? | default $host.name)-($host.kernel_version? | default 'unknown')"
        architecture: (process-require "uname" ["-m"] | get stdout | str trim)
        cpu_model: ($cpu | first | get brand)
        logical_cores: ($cpu | length)
        memory_bytes: ($memory.total | into int)
        power_mode: ($env.BACKEND_MEASUREMENT_POWER_MODE? | default "uncontrolled")
        allocator: $target.allocator
        target_triple: $target_triple
        compiler_version: ($compiler | first)
        control_plane: (open --raw $env.BACKEND_CONTROL_PLANE | hash sha256)
        revision: $revision
        dirty: $dirty
        tool: $target.tool
        protocol: $target.protocol
        warmth: $target.warmth
        repetitions: $target.repetitions
        statistical_method: $target.statistical_method
    }
}

# Creates a collision-resistant immutable directory for one declared measurement.
def measurement-run [target: record]: nothing -> record {
    let started_at = (date now)
    let run_id = $"($started_at | format date '%Y%m%dT%H%M%S%.fZ')-($target.name)-(random uuid)"
    let directory = local-root | path join "measurements" "runs" $run_id
    mkdir $directory
    {
        id: $run_id
        directory: $directory
        target: $target
        environment: (measurement-environment $target)
        started_at: ($started_at | format date "%Y-%m-%dT%H:%M:%S%.fZ")
    }
}

# Hashes declared run artifacts into one closed manifest.
def measurement-artifacts [run: record, artifacts: record]: nothing -> table {
    $artifacts
    | transpose name path
    | each {|artifact|
      let path = $artifact.path | path expand
      if not ($path | str starts-with $"($run.directory)/") {
        tooling-fail "measurement-artifact-escape" $"artifact ($artifact.name) is outside its run directory"
      }
      if not ($path | path exists) {
        tooling-fail "missing-measurement-artifact" $"artifact ($artifact.name) does not exist"
      }
      let bytes = (open --raw $path)
      {
        name: $artifact.name
        file: ($path | path relative-to $run.directory)
        bytes: ($bytes | encode utf-8 | length)
        sha256: ($bytes | hash sha256)
      }
    }
    | sort-by name
}

# Persists one completed run and seals its content-addressed artifacts read-only.
def finish-measurement [run: record, result: record, artifacts: record = {}]: nothing -> record {
    let process_artifact = $run.directory | path join "process.json"
    {
        schema: 1
        program: ($result.program | path basename)
        argument_count: ($result.arguments | length)
        status: $result.status
        stdout: $result.stdout
        stderr: $result.stderr
    } | to json --indent 2 | save --raw $process_artifact
    let durable_artifacts = (
        measurement-artifacts $run ($artifacts | merge { process: $process_artifact })
    )
    let record = {
        schema: 1
        id: $run.id
        target: $run.target
        environment: $run.environment
        started_at: $run.started_at
        completed_at: (date now | format date "%Y-%m-%dT%H:%M:%S%.fZ")
        status: (if $result.status == 0 { "passed" } else { "failed" })
        exit_status: $result.status
        artifacts: $durable_artifacts
    }
    let destination = $run.directory | path join "run.json"
    $record | to json --indent 2 | save --raw $destination
    ^chmod -R "a-w" $run.directory
    if $result.status != 0 {
        tooling-fail "measurement-failed" $"declared measurement ($run.target.name) exited with status ($result.status)" $"inspect ($destination) and ($process_artifact)"
    }
    $record | merge { record: $destination }
}

# Records nightly Cargo compilation timing for the Nix-declared changed-package target.
# @class measurement
def "main observe build" [target: string = "changed-debug-build", --base: string]: nothing -> record {
    require-command "observe-build"
    let declaration = (measurement-target $target "cargo-build")
    if ($env.BACKEND_NIGHTLY_CARGO? | is-empty) {
        tooling-fail "verification-environment-required" "nightly Cargo is absent from the verifier shell" "enter nix develop path:.#verification"
    }
    let run = (measurement-run $declaration)
    let packages = (required-changed-package-names --base $base)
    let package_arguments = $packages | each {|name| ["--package" $name] } | flatten
    let result = (
        process-result $env.BACKEND_NIGHTLY_CARGO (["build" "--locked" "-Z" "timings"] | append $package_arguments)
    )
    finish-measurement $run $result {packages: $packages}
}

# Runs one exact Nix-declared benchmark under reviewer measurement custody.
# @class measurement
def "main observe benchmark" [target: string]: nothing -> record {
    require-command "observe-benchmark"
    let declaration = (measurement-target $target "cargo-benchmark")
    let run = (measurement-run $declaration)
    let result = (
        process-result $env.BACKEND_STABLE_CARGO [
            "bench"
            "--locked"
            "--package" $declaration.package
            "--bench" $declaration.benchmark
        ]
    )
    finish-measurement $run $result
}

# Captures a sampled CPU profile for one exact Nix-declared release binary.
# @class measurement
def "main observe profile" [target: string]: nothing -> record {
    require-command "observe-profile"
    let declaration = (measurement-target $target "cargo-profile")
    let run = (measurement-run $declaration)
    let build = (
        process-result $env.BACKEND_STABLE_CARGO [
            "build"
            "--locked"
            "--release"
            "--package" $declaration.package
            "--bin" $declaration.binary
        ]
    )
    if $build.status != 0 {
        finish-measurement $run $build | ignore
    }
    let artifact = $run.directory | path join "samply.json"
    let executable = local-root | path join "target" "release" $declaration.binary
    let result = (
        process-result "samply" (
            ["record" "--save-only" "--output" $artifact $executable]
            | append $declaration.arguments
        )
    )
    finish-measurement $run $result {profile: $artifact}
}

# Records release binary contribution facts for one exact Nix-declared binary.
# @class measurement
def "main observe binary" [target: string]: nothing -> record {
    require-command "observe-binary"
    let declaration = (measurement-target $target "cargo-binary")
    let run = (measurement-run $declaration)
    let artifact = $run.directory | path join "cargo-bloat.jsonl"
    let result = (
        process-result $env.BACKEND_STABLE_CARGO [
            "bloat"
            "--release"
            "--package" $declaration.package
            "--bin" $declaration.binary
            "--message-format" "json"
        ]
    )
    $result.stdout | save --raw $artifact
    finish-measurement $run $result {binary_contributions: $artifact}
}

# Promotes one passed comparable run to a new immutable local baseline.
# @class baseline-write
def "main observe baseline" [run: path, name: string]: nothing -> record {
    require-command "observe-baseline"
    let policy = control-plane | get observability.measurements
    if $name !~ $policy.namePattern {
        tooling-fail "invalid-baseline-name" $"baseline has an invalid name: ($name)"
    }
    let run_root = local-root | path join "measurements" "runs"
    let source = $run | path expand
    if not ($source | str starts-with $"($run_root)/") {
        tooling-fail "baseline-path-escape" "baseline source must be an immutable local measurement run"
    }
    if not ($source | path exists) {
        tooling-fail "missing-run" $"measurement record does not exist: ($source)"
    }
    let record = (open $source)
    if $record.status != "passed" {
        tooling-fail "ineligible-baseline" "only passed measurement records may become baselines"
    }
    if $record.environment.dirty {
        tooling-fail "dirty-measurement" "a dirty worktree measurement cannot become a baseline"
    }
    if $record.environment.power_mode == "uncontrolled" {
        tooling-fail "uncontrolled-power-mode" "declare and hold a fixed power mode before promoting a baseline"
    }
    let observed = $record.environment | columns | sort
    let required = $policy.requiredEnvironment | sort
    if $observed != $required {
        tooling-fail "incomparable-environment" $"measurement environment differs from the required schema: ($observed | str join ', ')"
    }
    let baselines = local-root | path join "measurements" "baselines"
    if not (glob ($baselines | path join $"($name)-*") | is-empty) {
        tooling-fail "immutable-baseline" $"baseline already exists: ($name)" "choose a new semantic baseline name"
    }
    let digest = open --raw $source | hash sha256
    let destination = $baselines | path join $"($name)-($digest | str substring 0..15)"
    mkdir $baselines
    ^cp -R ($source | path dirname) $destination
    ^chmod -R "a-w" $destination
    {
        name: $name
        digest: $digest
        source: $source
        destination: $destination
        owner: (active-role)
    }
}
