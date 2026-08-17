# Stub for @nix//toolchains:rust.bzl — see nix/build/stubs/nix/flake.bzl for context.
#
# nix_rust_toolchain() creates a filegroup placeholder that is never reached
# at analysis time when `[nix] toolchain = 0`; the rust toolchain alias
# resolves to system_rust_toolchain instead.

def nix_rust_toolchain(*, name, rustc = None, rustdoc = None, clippy = None, default_edition = "2021", visibility = []):
    native.filegroup(name = name, srcs = [], visibility = visibility)
