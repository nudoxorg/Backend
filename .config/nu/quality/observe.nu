# Captures changed-scope timing, profiling, and build evidence under `.local`.
# Separates measurement authority from implementation and research privileges.
# Produces content-addressed run records instead of narrating benchmark claims.

# Creates the durable directory for one selected measurement run.
def measurement-run [kind: string]: nothing -> record {
  let fingerprint = (change-fingerprint)
  let run_id = $"($kind)-($fingerprint | str substring 0..15)"
  let directory = (local-root | path join "runs" $run_id)
  mkdir $directory
  { id: $run_id, directory: $directory, fingerprint: $fingerprint, kind: $kind }
}

# Records nightly Cargo compilation timing for changed packages.
def "main observe build" [--base: string]: nothing -> record {
  require-role "build-timing" ["terra-reviewer" "sol-integrator"]
  if ($env.BACKEND_NIGHTLY_CARGO? | is-empty) {
    tooling-fail "verification-environment-required" "nightly Cargo is absent from the development shell" "enter nix develop path:.config#verification"
  }
  let run = (measurement-run "build")
  let packages = (required-changed-package-names --base $base)
  let package_arguments = ($packages | each {|name| ["--package" $name] } | flatten)
  process-require $env.BACKEND_NIGHTLY_CARGO (["build" "--locked" "-Z" "timings"] | append $package_arguments) | ignore
  let record = $run | merge { packages: $packages, role: (active-role), status: "passed", tool: "cargo-nightly-timings" }
  $record | to json --indent 2 | save --force ($run.directory | path join "run.json")
  $record
}

# Runs an explicitly named benchmark after verifier privilege is established.
def "main observe benchmark" [package: string, benchmark: string]: nothing -> record {
  require-role "benchmark" ["terra-reviewer" "sol-integrator"]
  if ($env.BACKEND_NIGHTLY_CARGO? | is-empty) {
    tooling-fail "verification-environment-required" "measurement tools are absent from the development shell" "enter nix develop path:.config#verification"
  }
  let run = (measurement-run "benchmark")
  process-require $env.BACKEND_STABLE_CARGO ["bench" "--locked" "--package" $package "--bench" $benchmark] | ignore
  let record = $run | merge { package: $package, benchmark: $benchmark, role: (active-role), status: "passed" }
  $record | to json --indent 2 | save --force ($run.directory | path join "run.json")
  $record
}

# Captures a sampled CPU profile for one explicit executable and argument vector.
def "main observe profile" [executable: path, arguments: list<string> = []]: nothing -> record {
  require-role "profile" ["terra-reviewer" "sol-integrator"]
  if ($env.BACKEND_NIGHTLY_CARGO? | is-empty) {
    tooling-fail "verification-environment-required" "profiling tools are absent from the development shell" "enter nix develop path:.config#verification"
  }
  let run = (measurement-run "profile")
  let artifact = ($run.directory | path join "samply.json")
  process-require "samply" (["record" "--save-only" "--output" $artifact ($executable | into string)] | append $arguments) | ignore
  let record = $run | merge { executable: ($executable | into string), arguments: $arguments, role: (active-role), status: "passed", artifact: $artifact }
  $record | to json --indent 2 | save --force ($run.directory | path join "run.json")
  $record
}

# Records release binary contribution facts for one exact package.
def "main observe binary" [package: string]: nothing -> record {
  require-role "binary-measurement" ["terra-reviewer" "sol-integrator"]
  if ($env.BACKEND_NIGHTLY_CARGO? | is-empty) {
    tooling-fail "verification-environment-required" "binary measurement tools are absent from the development shell" "enter nix develop path:.config#verification"
  }
  let run = (measurement-run "binary")
  let result = (process-require $env.BACKEND_STABLE_CARGO ["bloat" "--release" "--package" $package "--message-format" "json"])
  $result.stdout | save --force ($run.directory | path join "cargo-bloat.jsonl")
  let record = $run | merge { package: $package, role: (active-role), status: "passed", artifact: ($run.directory | path join "cargo-bloat.jsonl") }
  $record | to json --indent 2 | save --force ($run.directory | path join "run.json")
  $record
}

# Promotes one comparable verified run to a named local baseline under Sol custody.
def "main observe baseline" [run: path, name: string]: nothing -> record {
  require-role "baseline-write" ["sol-integrator"]
  let source = ($run | path expand)
  if not ($source | path exists) {
    tooling-fail "missing-run" $"measurement record does not exist: ($source)"
  }
  let record = (open $source)
  if ($record.status? | default "") != "passed" {
    tooling-fail "ineligible-baseline" "only passed measurement records may become baselines"
  }
  let destination = (local-root | path join "baselines" $"($name).json")
  mkdir ($destination | path dirname)
  $record | to json --indent 2 | save --force $destination
  { name: $name, source: $source, destination: $destination, owner: (active-role) }
}
