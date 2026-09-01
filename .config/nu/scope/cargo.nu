# Maps changed repository paths onto flattened Cargo package boundaries.
# Reads workspace metadata once and derives exact package manifest ownership.
# Keeps default quality work narrow while permitting explicit impact expansion.

# Returns Cargo metadata from the pinned stable toolchain as a typed record.
def cargo-metadata []: nothing -> record {
    let root = (repository-root)
    process-require $env.BACKEND_STABLE_CARGO [
        "metadata"
        "--format-version" "1"
        "--locked"
        "--manifest-path"
        ($root | path join "Cargo.toml")
    ]
    | get stdout
    | from json
}

# Returns workspace packages whose manifest directories own changed paths.
def changed-packages [--base: string]: nothing -> table {
    let root = (repository-root)
    let paths = (changed-paths --base $base)
    let metadata = (cargo-metadata)
    $metadata
    | get packages
    | where {|package| $package.id in $metadata.workspace_members }
    | each {|package|
      let directory = $package.manifest_path | path dirname | path relative-to $root
      let owned = ($paths | any {|path| ($path == $directory) or ($path | str starts-with $"($directory)/") })
      if $owned { { name: $package.name, manifest: $package.manifest_path, directory: $directory } }
    }
    | compact
    | sort-by name
}

# Returns package names selected by changed paths and rejects an empty product scope.
def required-changed-package-names [--base: string]: nothing -> list<string> {
    let names = changed-packages --base $base | get name
    if ($names | is-empty) {
        tooling-fail "empty-crate-scope" "no changed Cargo package was found" "change a crate path or pass an explicit package to the command"
    }
    $names
}
