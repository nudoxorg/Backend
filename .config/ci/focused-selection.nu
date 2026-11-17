# Product binaries checked in the focused release lanes. The SQLite worker is
# an isolated workspace and is built separately by the native Keel lane.
# The Keel driver comes from the pinned standalone Nix input, outside Cargo.
export def focused-packages []: nothing -> list<string> {
    [
        "backend-cli"
        "backend-mcp"
        "backend-locald"
        "backend-metrics"
        "backend-desktop"
        "backend-frontend-stylo"
    ]
}

export def focused-package-args []: nothing -> list<string> {
    focused-packages | each {|name| ["--package" $name] } | flatten
}

# Workspace-wide archived builds opt in to the retained compiler adapters.
export def archived-feature-args []: nothing -> list<string> {
    [
        "--features"
        "backend-engine/paused-frontends,backend-local-service/paused-frontends,backend-cli/paused-frontends"
    ]
}

# The selected normal/build closure must not silently reacquire a paused
# frontend through a newly added dependency or default feature.
export def assert-focused-dependencies [target: string]: nothing -> nothing {
    let result = (
        run-external "cargo" "tree" "--locked" "--target" $target "--edges" "normal,build" "--prefix" "none" ...(focused-package-args)
        | complete
    )
    if $result.exit_code != 0 {
        error make {msg: $"focused dependency inspection failed: ($result.stderr)"}
    }
    let paused = (
        $result.stdout
        | lines
        | where {|line| $line =~ '^backend-frontend-(clang|csharp|go|java) v' }
        | uniq
    )
    if not ($paused | is-empty) {
        error make {msg: $"focused dependency closure contains paused frontends: ($paused | str join ', ')"}
    }
}
