#!/usr/bin/env nu
# Reintroduces the exact Nix-selected toolchain for Dylint's isolated driver build.
# Receives only the Cargo arguments Dylint forwards after sanitising its environment.
# Delegates directly to the pinned Cargo binary without interpreting or rewriting arguments.
def main [...arguments: string]: nothing -> nothing {
    let cargo = $env.BACKEND_DYLINT_CARGO? | default ""
    let toolchain = $env.BACKEND_DYLINT_TOOLCHAIN? | default ""
    if ($cargo | is-empty) or ($toolchain | is-empty) {
        error make {code: "backend::dylint-cargo-bridge-unconfigured", msg: "Dylint Cargo bridge requires pinned Cargo and toolchain facts", help: "run `.config/dylint/test.nu` from the Nix verification environment"}
    }
    with-env { RUSTUP_TOOLCHAIN: $toolchain } {
        run-external $cargo ...$arguments
    }
}
