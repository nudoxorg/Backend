# Luna lane tool closure

Use the explicit `luna-tools` package to enter a lane for Cargo checks:

```sh
nix shell path:.#luna-tools --command cargo --version
nix shell path:.#luna-tools --command cargo test --workspace --offline
nix build path:.#checks.aarch64-darwin.luna-tools-closure
```

The package is a small, pinned command closure containing the stable Fenix
toolchain, the lane's basic process and inspection tools, and the pinned C
compiler authority. It does not realize the backend-control binary, GUI
runtime/capture stack, service binaries, language corpus, or full role command
bundle. This keeps a version probe and core Rust checks independent of the
large product closures. `nix develop` is outside the lane contract.

The full Luna policy command surface is retained as the separate
`luna-role-tools` package and is used by the control-plane checks. The
`luna-tools-closure` flake check queries the realized Nix requisites and fails
if product or `bmake` paths enter the lightweight closure, or if the closure
grows beyond its 512 requisite budget; it also executes the pinned Cargo,
Rust, formatting, lint, Git, JSON, and Nushell binaries.

Evaluation and realization still require the locked flake inputs and fixed
output sources to be available. `--offline` is reproducible after those
inputs and the small closure are present in the local store; it never changes
or bypasses their hashes.
