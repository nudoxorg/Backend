# Runs the semantic lint island with a Nix-provided nightly compiler.
# Roots Cargo and Dylint artifacts under the repository-local artifact tree.
# Prevents a checked-in or ambient .cargo directory from influencing UI results.
source ../nu/core/failure.nu

const DYLIB_DIRECTORY = path self | path dirname

def main [--bless, --packages: string = "", --product-root: path]: nothing -> nothing {
    let lint_root = $env.BACKEND_DYLINT_ROOT? | default $DYLIB_DIRECTORY
    let artifacts = $env.BACKEND_DYLINT_ARTIFACTS? | default (
        $lint_root | path dirname | path dirname | path join ".local" "dylint"
    )
    let toolchain = $env.BACKEND_DYLINT_TOOLCHAIN? | default ""
    let nightly_cargo = $env.BACKEND_NIGHTLY_CARGO? | default ""
    if ($nightly_cargo | is-empty) or ($toolchain | is-empty) {
        tooling-fail "missing-nightly" "BACKEND_NIGHTLY_CARGO and BACKEND_DYLINT_TOOLCHAIN are required" "enter the Nix verification shell"
    }
    let nightly_rustc = $nightly_cargo | path dirname | path join "rustc"
    let declared = $env.BACKEND_DYLINT_DECLARATIONS? | default "[]" | from json | sort
    if not ($declared | is-empty) {
        let compiled = (
            open --raw ($lint_root | path join "lib.rs")
            | parse -r 'pub (?<name>[A-Z][A-Z_]+),'
            | get name
            | each {|name| $name | str downcase | str replace --all "_" "-" }
            | sort
        )
        if $compiled != $declared {
            tooling-fail "dylint-registry-drift" $"declared ($declared), compiled ($compiled)"
        }
    }
    let shim_directory = $artifacts | path join "bin"
    let cargo_shim = $shim_directory | path join "cargo"
    let shim = $shim_directory | path join "rustup"
    let environment = {
        CARGO_HOME: ($artifacts | path join "cargo")
        CARGO_TARGET_DIR: ($artifacts | path join "target")
        DYLINT_DRIVER_PATH: ($artifacts | path join "drivers")
        RUSTUP_TOOLCHAIN: $toolchain
        BACKEND_DYLINT_RUSTC: $nightly_rustc
        BACKEND_DYLINT_TOOLCHAIN: $toolchain
        BACKEND_DYLINT_CARGO: $nightly_cargo
        PATH: ($env.PATH | prepend $shim_directory)
    }
    [
        $environment.CARGO_HOME
        $environment.CARGO_TARGET_DIR
        $environment.DYLINT_DRIVER_PATH
        $shim_directory
    ] | each {|directory| mkdir $directory }
    # A sandboxed Nix build has no network: the flake check passes the lint
    # crate's dependencies vendored from its Cargo.lock, and Cargo reads them
    # from there instead of crates.io. Outside the check nothing changes.
    # The sources are reached through CARGO_HOME: Dylint's build script
    # treats a copy outside it as a source checkout with a sibling `driver`
    # directory, and the driver it builds on first use resolves from them.
    let vendor = ($env.BACKEND_DYLINT_VENDOR? | default "")
    if not ($vendor | is-empty) {
        let vendored = $environment.CARGO_HOME | path join "vendor"
        if not ($vendored | path exists) { ^ln -s $vendor $vendored }
        $"[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"($vendored)\"\n"
            | save --force ($environment.CARGO_HOME | path join "config.toml")
    }
    # The shims run under this same nu: a Nix build sandbox has no
    # /usr/bin/env for their `#!/usr/bin/env nu` line to name.
    for pair in [[$cargo_shim "cargo.nu"] [$shim "rustup.nu"]] {
        open --raw ($lint_root | path join $pair.1)
            | str replace --regex '^#![^\n]*' $"#!($nu.current-exe)"
            | save --force --raw $pair.0
    }
    run-external "chmod" "+x" $cargo_shim $shim
    let arguments = if $bless {
        ["test" "--locked" "--" "--bless"]
    } else { ["test" "--locked"] }
    cd $lint_root
    with-env $environment {
        run-external $nightly_cargo "build" "--locked" "--lib"
        let source = glob ($environment.CARGO_TARGET_DIR | path join "debug" "libbackend_semantic_lints.*") | where {|candidate| ($candidate | path parse | get extension) in ["dylib" "so" "dll"] } | first
        let extension = $source | path parse | get extension
        let linked = $environment.CARGO_TARGET_DIR | path join "debug" $"libbackend_semantic_lints@($toolchain).($extension)"
        cp $source $linked
        if not ($packages | is-empty) {
            if $product_root == null {
                tooling-fail "missing-product-root" "semantic product linting requires --product-root"
            }
            cd $product_root
            for package in ($packages | split row ",") {
                run-external $nightly_cargo "dylint" "--lib-path" $linked "--" "--package" $package "--all-targets"
            }
            return
        }
        run-external $nightly_cargo ...$arguments
  }
}
