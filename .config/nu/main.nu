# Composes the repository's complete public tooling command namespace.
# Keeps individual command files focused while presenting one discoverable CLI.
# Provides a cheap doctor before any command is trusted with repository state.

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
        tooling-fail "missing-tools" $"pinned environment lacks ($missing | str join ', ')" "enter through direnv or nix develop path:.config#development"
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
