#!/usr/bin/env nu
# Bridges Dylint's narrow rustup discovery calls into a Nix toolchain.
# Implements only the two queries Dylint 6.0.4 performs while building a driver.
# Refuses every mutation or ambient-toolchain request instead of emulating rustup broadly.
def main [...arguments: string]: nothing -> nothing {
    let toolchain = $env.BACKEND_DYLINT_TOOLCHAIN? | default ""
    let rustc = $env.BACKEND_DYLINT_RUSTC? | default ""
    match $arguments {
        ["show", "active-toolchain"] => {
            if ($toolchain | is-empty) {
                error make {code: "backend::dylint-toolchain-unbound", msg: "Dylint requested the active toolchain before Nix bound its identity"}
            }
            print ($toolchain + " (Nix supplied)")
        }
        ["which", "rustc"] => {
            if ($rustc | is-empty) {
                error make {code: "backend::dylint-rustc-unbound", msg: "Dylint requested rustc before Nix bound the compiler path"}
            }
            print $rustc
        }
        _ => {
            error make {
                code: "backend::dylint-rustup-unsupported"
                msg: (
                    "Dylint requested an unsupported rustup operation: " + ($arguments | str join " ")
                )
                help: "the Nix bridge supports only `show active-toolchain` and `which rustc`"
            }
        }
    }
}
