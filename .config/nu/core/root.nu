# Discovers and validates the workspace's runtime ownership boundaries.
# Keeps generated state under `.local` and configuration under `.config`.
# An explicit Nix workspace snapshot remains usable when Git metadata is
# detached or absent, but only from within that exact Cargo workspace.

# Returns the canonical repository root containing the current working
# directory, or the explicitly bound Cargo workspace supplied by Nix.
def repository-root []: nothing -> string {
    let result = (^git rev-parse --show-toplevel | complete)
    if $result.exit_code != 0 {
        let declared = $env.BACKEND_WORKSPACE_SNAPSHOT? | default ""
        if ($declared | is-empty) {
            tooling-fail "outside-repository" "the current directory is not inside a Git worktree and no Cargo workspace was declared" "change into the backend workspace or enter through its Nix shell"
        }
        let root = $declared | path expand
        let current = $env.PWD | path expand
        let manifest = $root | path join "Cargo.toml"
        let contains_current = $current == $root or ($current | str starts-with $"($root)/")
        if not $contains_current or not ($manifest | path exists) {
            tooling-fail "outside-repository" "the current directory is outside its declared Cargo workspace" "change into the backend workspace or enter through its Nix shell"
        }
        return $root
    }
    $result.stdout | str trim | path expand
}

# Returns the live or immutable centrally owned tool configuration directory.
def configuration-root []: nothing -> string {
    let mode = $env.BACKEND_CONFIG_MODE? | default "live"
    match $mode {
        "live" => {
            let directory = repository-root | path join ".config"
            if not ($directory | path exists) {
                tooling-fail "missing-live-configuration" $"repository configuration does not exist: ($directory)"
            }
            $directory
        }
        "immutable" => {
            let directory = $env.BACKEND_CONFIG_SNAPSHOT? | default ""
            if ($directory | is-empty) {
                tooling-fail "missing-configuration-snapshot" "BACKEND_CONFIG_SNAPSHOT is not set" "enter through a pinned Nix check or generated application"
            }
            if not ($directory | path exists) {
                tooling-fail "missing-configuration-snapshot" $"configuration snapshot does not exist: ($directory)"
            }
            $directory | path expand
        }
        _ => {
            tooling-fail "invalid-configuration-mode" $"expected live or immutable configuration mode, observed ($mode)"
        }
    }
}

# Creates and returns the ignored local artifact directory.
def local-root []: nothing -> string {
    let directory = repository-root | path join ".local"
    mkdir $directory
    $directory
}

# Resolves a relative path and proves that it remains beneath the repository root.
def owned-path [candidate: path]: nothing -> string {
    if ($candidate | path type) == "absolute" {
        tooling-fail "absolute-path" $"expected a repository-relative path, observed ($candidate)" "remove the leading root component"
    }
    let root = (repository-root)
    let resolved = $root | path join $candidate | path expand
    if not ($resolved | str starts-with $"($root)/") {
        tooling-fail "path-escape" $"($candidate) escapes the repository" "choose a path beneath the current repository"
    }
    $resolved
}

# Returns the path relative to the repository without accepting external paths.
def relative-owned-path [candidate: path]: nothing -> string {
    let root = (repository-root)
    let resolved = $candidate | path expand
    if not (($resolved == $root) or ($resolved | str starts-with $"($root)/")) {
        tooling-fail "path-escape" $"($candidate) is outside ($root)"
    }
    $resolved | path relative-to $root
}
