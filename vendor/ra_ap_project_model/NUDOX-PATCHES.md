# Nudox patch notes

These crates are vendored from rust-analyzer 0.0.341 under the MIT OR Apache-2.0
license. Their upstream source and license texts are retained here so the narrow
authority changes can be reviewed against the pinned release.

`ra_ap_project_model::CargoConfig` adds `isolate_env`, defaulting to `false` for
upstream-compatible behavior. When enabled, project loading marks its child
environment as isolated. The project model then uses the explicit `CARGO`,
`RUSTC`, `CARGO_HOME`, `PATH`, `RUSTUP_TOOLCHAIN`, and `RUST_SRC_PATH` entries in
`extra_env` without consulting the parent process environment for those values.

`ra_ap_toolchain` consumes that internal isolation marker, clears inherited
variables before applying the explicit overlay, and resolves tools through the
same overlay. This keeps an admitted compiler path and Cargo home consistent
through sysroot discovery, Cargo metadata, and rustc queries.

`ProjectWorkspace::run_build_scripts_with_runner` and
`ProjectWorkspace::run_all_build_scripts_with_runner` expose an injectable
`BuildScriptProcessRunner`. rust-analyzer still builds the complete Cargo
command, including its argv, working directory, lockfile handling, and
environment overrides; the runner receives that command plus the explicit
clear-or-inherit base environment policy and stdout/stderr line callbacks.
rust-analyzer retains its Cargo JSON parsing and diagnostic handling. Build
command construction applies the internal isolation marker from
`CargoConfig.isolate_env` and returns the matching policy alongside the
command.
