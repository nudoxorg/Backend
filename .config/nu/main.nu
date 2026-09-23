# Composes the repository's complete public tooling command namespace.
# Keeps individual command files focused while presenting one discoverable CLI.
# Provides a cheap doctor before any command is trusted with repository state.

# The wrapper bakes `CARGO_TARGET_DIR` as the relative `.local/target`, as the
# interactive shells resolve it against the entered repository. Resolve it the
# same way here: a relative value otherwise follows every child process's own
# working directory, so a trybuild or build-script cargo run inside
# `crates/<name>` would scatter a second target tree into that crate.
let target_root = (^git rev-parse --show-toplevel | complete)
$env.CARGO_TARGET_DIR = (
    if $target_root.exit_code == 0 { $target_root.stdout | str trim } else { $env.PWD }
    | path join ($env.CARGO_TARGET_DIR? | default ".local/target")
)

# Shows the typed command catalog when no subcommand is supplied.
# @class inspection
def main []: nothing -> table {
    command-catalog
}

# Emits the AST-derived command catalog for tooling contracts and help frontends.
# @class inspection
def "main catalog" [--json]: nothing -> any {
    require-command "catalog"
    let catalog = command-catalog
    if $json {
        $catalog | to json --indent 2
    } else { $catalog }
}

# Reports pinned tool availability, path ownership, and role identity.
# @class inspection
def "main doctor" []: nothing -> record {
    require-command "doctor"
    let required = [
        "ast-grep"
        "cargo-nextest"
        "git"
        "koji"
        "nix"
        "nu"
    ]
    let tools = (
        $required
        | each {|name| { name: $name, available: (which $name | is-not-empty) } }
    )
    let missing = $tools | where available == false | get name
    if not ($missing | is-empty) {
        tooling-fail "missing-tools" $"pinned environment lacks ($missing | str join ', ')" "enter through direnv or nix develop path:.#development"
    }
    {
        repository: (repository-root)
        configuration: (configuration-root)
        local: (local-root)
        role: (active-role)
        tools: $tools
    }
}

# Returns changed paths and their owning Cargo packages without running a gate.
# @class inspection
def "main scope changed" [--base: string, --json]: nothing -> any {
    require-command "scope-changed"
    let scope = {
        base: (if ($base | is-empty) { comparison-base } else { $base })
        paths: (changed-paths --base $base)
        packages: (changed-packages --base $base)
    }
    if $json {
        $scope | to json --indent 2
    } else { $scope }
}
