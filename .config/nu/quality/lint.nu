# Enforces syntax, documentation, layout, naming, and compiler-backed policy.
# Applies cheap structural rules to changed paths before invoking crate checks.
# Keeps semantic Dylint work isolated behind an explicit reviewer-level command.

# Returns the active role used to enforce command capability separation.
def active-role []: nothing -> string {
  $env.BACKEND_AGENT_ROLE? | default "human"
}

# Proves that the active role may use one privileged command family.
def require-role [capability: string, permitted: list<string>] {
  let role = (active-role)
  if $role not-in ($permitted | append "human") {
    tooling-fail "capability-denied" $"role ($role) may not use ($capability)" $"permitted roles: ($permitted | str join ', ')"
  }
}

# Validates the required opening purpose block for one changed source file.
def lint-file-purpose [relative: string]: nothing -> list<record> {
  let path = (repository-root | path join $relative)
  if not ($path | path exists) { return [] }
  let extension = ($path | path parse | get extension)
  let prefix = if $extension == "rs" { "//!" } else { "#" }
  let opening = (open --raw $path | lines | first 3)
  if (($opening | length) != 3) or ($opening | any {|line| not ($line | str starts-with $prefix) }) {
    [{ rule: "file-purpose", path: $relative, message: $"first three lines must be ($prefix) purpose documentation" }]
  } else { [] }
}

# Rejects implementation-bearing top-level items in a flattened crate `lib.rs`.
def lint-library-surface [relative: string]: nothing -> list<record> {
  if not ($relative | str ends-with "/lib.rs") { return [] }
  let path = (repository-root | path join $relative)
  let forbidden = (open --raw $path | lines | enumerate | where {|row|
    let line = ($row.item | str trim)
    not (($line | is-empty)
      or ($line | str starts-with "//!")
      or ($line | str starts-with "#![")
      or ($line | str starts-with "use ")
      or ($line | str starts-with "pub use "))
  })
  $forbidden | each {|row| { rule: "library-surface", path: $relative, line: ($row.index + 1), message: "lib.rs may contain only crate docs, attributes, use, and pub use" } }
}

# Rejects source directories and `mod.rs` files from flattened crate layouts.
def lint-layout-path [relative: string]: nothing -> list<record> {
  let components = ($relative | split row '/')
  mut findings = []
  if "src" in $components {
    $findings = ($findings | append { rule: "flattened-crate", path: $relative, message: "crate code must remain beside Cargo.toml, never beneath src/" })
  }
  if ($relative | path basename) == "mod.rs" {
    $findings = ($findings | append { rule: "named-module", path: $relative, message: "name module roots after their invariant instead of mod.rs" })
  }
  $findings
}

# Runs structural Rust policies against an explicit changed-file list.
def lint-structural [paths: list<string>]: nothing -> list<record> {
  $paths | each {|path|
    lint-file-purpose $path
    | append (lint-library-surface $path)
    | append (lint-layout-path $path)
  } | flatten
}

# Runs all fast changed-file lints and strict Clippy for changed packages.
def "main lint changed" [--base: string]: nothing -> record {
  let root = (repository-root)
  let rust_paths = (changed-rust-paths --base $base)
  let structural = (lint-structural $rust_paths)
  if not ($structural | is-empty) {
    $structural | to json --indent 2 | print
    tooling-fail "structural-lint" $"($structural | length) structural finding(s)"
  }
  if not ($rust_paths | is-empty) {
    process-require "ast-grep" (["scan" "--config" ($root | path join ".config/ast-grep/sgconfig.yml")] | append ($rust_paths | each {|path| $root | path join $path })) | ignore
  }
  let packages = (changed-packages --base $base)
  for package in $packages {
    process-require $env.BACKEND_STABLE_CARGO ["clippy" "--locked" "--all-targets" "--package" $package.name "--" "-D" "warnings" "-D" "clippy::undocumented_unsafe_blocks"] | ignore
  }
  { rust_paths: $rust_paths, packages: ($packages | get name), findings: 0 }
}

# Runs the lint engines' own fixture and snapshot suites.
def "main lint self-test" []: nothing -> record {
  let config = (configuration-root)
  process-require "ast-grep" ["test" "--config" ($config | path join "ast-grep/sgconfig.yml")] | ignore
  validate-role-contracts (role-contracts) (command-catalog)
  process-require "nu" ["--no-config-file" ($config | path join "nu/tests.nu")] | ignore
  { ast_grep: "passed", nushell: "passed" }
}

# Returns the engine, severity, exception, and proof contract for one lint.
def "main lint explain" [rule: string]: nothing -> record {
  let matches = (open (configuration-root | path join "lint/policy.nuon") | where id == $rule)
  if ($matches | is-empty) {
    tooling-fail "unknown-lint" $"no lint policy named ($rule)"
  }
  $matches | first
}

# Runs the semantic lint island after a verifier explicitly assumes custody.
def "main lint semantic" [--package: string]: nothing -> record {
  require-role "semantic-lint" ["terra-reviewer" "sol-integrator"]
  let package_names = if ($package | is-empty) { required-changed-package-names } else { [$package] }
  for name in $package_names {
    process-require $env.BACKEND_STABLE_CARGO ["dylint" "--package" $name "--all"] | ignore
  }
  { packages: $package_names, verifier: (active-role) }
}
