# Creates flattened Rust crates inside one of the four product boundaries.
# Inherits all package and lint policy from the root workspace automatically.
# Validates Cargo discovery before declaring the new crate usable.

# Validates a crate destination and returns its architectural boundary.
def crate-boundary [path: path]: nothing -> string {
    let relative = $path | into string | str trim --char '/'
    let components = $relative | split row '/'
    if (($components | length) < 2) or ($components | any {|component| ($component | is-empty) or ($component in ["." ".."]) }) {
        tooling-fail "invalid-crate-path" "crate paths need a product boundary and one or more invariant-named directories" "use crates/<area>, frontends/<language>, extensions/<adapter>, or apps/<surface>"
    }
    let boundary = $components | first
    if $boundary not-in ["crates" "frontends" "extensions" "apps"] {
        tooling-fail "invalid-crate-boundary" $"crate path must begin with crates, frontends, extensions, or apps; observed ($relative)"
    }
    $boundary
}

# Creates a workspace-inherited crate with a flattened `lib.rs` root.
# @class repository-write
def "main create crate" [path: path, --name: string, --purpose: string]: nothing -> record {
    require-command "create-crate"
    let boundary = (crate-boundary $path)
    let destination = (owned-path $path)
    if ($destination | path exists) {
        tooling-fail "path-exists" $"refusing to overwrite crate directory ($path)"
    }
    let purpose_lines = (validated-purpose $purpose 3 360)
    if not ($name =~ '^[a-z][a-z0-9-]*$') {
        tooling-fail "invalid-crate-name" $"($name) is not a lowercase kebab-case package name"
    }
    let transaction = local-root | path join "transactions" $"crate-(random uuid)"
    mkdir $transaction
    let manifest = {
        package: {
            name: $name
            version: {workspace: true}
            edition: {workspace: true}
            rust-version: {workspace: true}
            license: {workspace: true}
            publish: {workspace: true}
        }
        lib: {path: "lib.rs"}
        lints: {workspace: true}
    }
    $manifest | to toml | save --raw ($transaction | path join "Cargo.toml")
    let crate_header = purpose-header ($transaction | path join "lib.rs") $purpose_lines
    $"($crate_header)\n\n" | save --raw ($transaction | path join "lib.rs")
    mkdir ($destination | path dirname)
    mv --no-clobber $transaction $destination
    let validation = (
        process-result $env.BACKEND_STABLE_CARGO [
            "metadata"
            "--format-version" "1"
            "--no-deps"
            "--manifest-path"
            (repository-root | path join "Cargo.toml")
        ]
    )
    if $validation.status != 0 {
        rm ($destination | path join "Cargo.toml")
        rm ($destination | path join "lib.rs")
        rmdir $destination
        tooling-fail "crate-not-discovered" $validation.stderr "add a workspace member pattern before retrying"
    }
    {
        path: (relative-owned-path $destination)
        name: $name
        boundary: $boundary
        purpose: ($purpose_lines | str join " ")
    }
}

# Creates an empty documented Cargo integration target beneath one owned crate.
# @class repository-write
def "main create test" [crate: path, name: string, --purpose: string]: nothing -> record {
    require-command "create-test"
    crate-boundary $crate | ignore
    if not ($name =~ '^[a-z][a-z0-9_]*$') {
        tooling-fail "invalid-test-name" $"($name) is not snake_case"
    }
    let crate_root = (owned-path $crate)
    if not (($crate_root | path join "Cargo.toml") | path exists) {
        tooling-fail "missing-crate" $"($crate) does not contain Cargo.toml"
    }
    let destination = $crate_root | path join "tests" $"($name).rs"
    if ($destination | path exists) {
        let relative = (relative-owned-path $destination)
        tooling-fail "path-exists" $"refusing to overwrite ($relative)"
    }
    let sentences = (validated-purpose $purpose 3 360)
    let transaction = local-root | path join "transactions" $"test-(random uuid)"
    mkdir ($transaction | path dirname)
    let header = (purpose-header $destination $sentences)
    $"($header)\n\n" | save --raw $transaction
    mkdir ($destination | path dirname)
    mv --no-clobber $transaction $destination
    {
        path: (relative-owned-path $destination)
        crate: ($crate | into string)
        purpose: ($sentences | str join " ")
    }
}
