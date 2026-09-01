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
    cp ($lint_root | path join "cargo.nu") $cargo_shim
    cp ($lint_root | path join "rustup.nu") $shim
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
