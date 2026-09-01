# Enforces syntax, documentation, layout, naming, and compiler-backed policy.
# Applies cheap structural rules to changed paths before invoking crate checks.
# Keeps semantic Dylint work isolated behind an explicit reviewer-level command.

# Validates the required opening purpose block for one changed source file.
def lint-file-purpose [relative: string]: nothing -> list<record> {
    let path = repository-root | path join $relative
    if not ($path | path exists) { return [] }
    let extension = $path | path parse | get extension
    let prefix = if $extension == "rs" { "//!" } else { "#" }
    let opening = open --raw $path | lines | first 3
    if (($opening | length) != 3) or ($opening | any {|line| not ($line | str starts-with $prefix) }) {
        [
            {
                rule: "file-purpose"
                path: $relative
                message: $"first three lines must be ($prefix) purpose documentation"
            }
        ]
    } else { [] }
}

# Rejects source directories and `mod.rs` files from flattened crate layouts.
def lint-layout-path [relative: string]: nothing -> list<record> {
    let components = $relative | split row '/'
    mut findings = []
    if "src" in $components {
        $findings = (
            $findings
            | append { rule: "flattened-crate", path: $relative, message: "crate code must remain beside Cargo.toml, never beneath src/" }
        )
    }
    if ($relative | path basename) == "mod.rs" {
        $findings = (
            $findings
            | append { rule: "named-module", path: $relative, message: "name module roots after their invariant instead of mod.rs" }
        )
    }
    if ($components | any {|component| $component in [".cargo" ".direnv"] }) {
        $findings = (
            $findings
            | append { rule: "local-artifact-root", path: $relative, message: "tool state belongs beneath .local, never .cargo or .direnv" }
        )
    }
    $findings
}

# Runs structural Rust policies against an explicit changed-file list.
def lint-structural [paths: list<string>]: nothing -> list<record> {
    $paths | each {|path|
    lint-file-purpose $path
    | append (lint-layout-path $path)
  } | flatten
}

# Returns structural findings for explicit repository paths without running compilers.
# @class inspection
def "main lint structure" [--json, ...paths: path]: nothing -> any {
    require-command "lint-structure"
    let relative = $paths | each {|path| relative-owned-path (owned-path $path) }
    let findings = (lint-structural $relative)
    if $json {
        $findings | to json --indent 2
    } else { $findings }
}

# Runs all fast changed-file lints and strict Clippy for changed packages.
# @class verification
def "main lint changed" [--base: string]: nothing -> record {
    require-command "lint-changed"
    let root = (repository-root)
    let rust_paths = (changed-rust-paths --base $base)
    let documented_paths = (changed-documented-paths --base $base)
    let structural = (lint-structural $documented_paths)
    if not ($structural | is-empty) {
        $structural | to json --indent 2 | print
        tooling-fail "structural-lint" $"structural findings: ($structural | length)"
    }
    if not ($rust_paths | is-empty) {
        process-require "ast-grep" (
            ["scan" "--config" $env.BACKEND_AST_GREP "--error"]
            | append ($rust_paths | each {|path| $root | path join $path })
        ) | ignore
    }
    let packages = (changed-packages --base $base)
    for package in $packages {
        process-require $env.BACKEND_STABLE_CARGO [
            "clippy"
            "--locked"
            "--all-targets"
            "--package" $package.name
            "--"
            "-D" "warnings"
            "-D" "clippy::undocumented_unsafe_blocks"
        ] | ignore
    }
    {
        documented_paths: $documented_paths
        rust_paths: $rust_paths
        packages: ($packages | get name)
        findings: 0
    }
}

# Runs the lint engines' own fixture and snapshot suites.
# @class verification
def "main lint self-test" []: nothing -> record {
    require-command "lint-self-test"
    let config = (configuration-root)
    let ast_root = $env.BACKEND_AST_GREP | path dirname
    let expected = (
        control-plane
        | get lint.syntax
        | columns
        | length
    )
    let matrices = glob ($ast_root | path join "tests/*.yml") | length
    if $matrices != $expected {
        tooling-fail "lint-matrix-drift" $"expected ($expected) generated ast-grep matrices, observed ($matrices)"
    }
    process-require "ast-grep" [
        "test"
        "--config" $env.BACKEND_AST_GREP
        "--test-dir"
        ($ast_root | path join "tests")
    ] | ignore
    validate-role-contracts (role-contracts) (command-catalog)
    process-require "nu" [
        "--no-config-file"
        ($config | path join "nu/tests.nu")
    ] | ignore
    let semantic = lint-declarations | where engine == "dylint" | get id | sort
    with-env {
        BACKEND_DYLINT_DECLARATIONS: ($semantic | to json)
        BACKEND_DYLINT_ROOT: ($config | path join "dylint")
        BACKEND_DYLINT_ARTIFACTS: (local-root | path join "dylint")
        BACKEND_DYLINT_TOOLCHAIN: $env.BACKEND_DYLINT_TOOLCHAIN
    } {
        process-require "nu" ["--no-config-file" ($config | path join "dylint/test.nu")] | ignore
    }
    {
        ast_grep: {rules: $expected, status: "passed"}
        nushell: "passed"
        dylint: {
            rules: ($semantic | length)
            status: "passed"
        }
    }
}

# Returns the engine, severity, exception, and proof contract for one lint.
# @class inspection
def "main lint explain" [rule: string]: nothing -> record {
    require-command "lint-explain"
    let matches = lint-declarations | where id == $rule
    if ($matches | is-empty) {
        tooling-fail "unknown-lint" $"no lint policy named ($rule)"
    }
    $matches | first
}

# Runs the semantic lint island after a verifier explicitly assumes custody.
# @class privileged-verification
def "main lint semantic" [--package: string]: nothing -> record {
    require-command "lint-semantic"
    let package_names = if ($package | is-empty) { required-changed-package-names } else { [$package] }
    let config = configuration-root
    let semantic = lint-declarations | where engine == "dylint" | get id | sort
    with-env {
        BACKEND_DYLINT_DECLARATIONS: ($semantic | to json)
        BACKEND_DYLINT_ROOT: ($config | path join "dylint")
        BACKEND_DYLINT_ARTIFACTS: (local-root | path join "dylint")
        BACKEND_DYLINT_TOOLCHAIN: $env.BACKEND_DYLINT_TOOLCHAIN
    } {
        process-require "nu" [
            "--no-config-file"
            ($config | path join "dylint/test.nu")
            "--packages"
            ($package_names | str join ",")
            "--product-root"
            (repository-root)
        ] | ignore
    }
    {
        packages: $package_names
        verifier: (active-role)
    }
}
