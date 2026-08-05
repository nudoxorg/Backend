# Testing Baseline

Run commands from the repository root. The full local check is intentionally
small and reports optional infrastructure explicitly.

After an implementation-agent wave, run `nu .config/scripts/recheck.nu` for the
cheap whitespace, metadata, GUI metadata, and Nushell syntax gate before
starting the longer suites.

## Tiers

| Tier | Command | Coverage |
| --- | --- | --- |
| Fast | `cargo test --workspace --lib` | Root workspace library unit tests. |
| Default | `nu .config/scripts/full-check.nu` | Locked root Cargo tests, fixture-engine tests, measurement-support tests, standalone GUI tests (when manifest present), and compiler Buck tests when `buck2` is available. The script sets `RUSTC_BOOTSTRAP=1` because `nudox-ir` still uses unstable macro declarations; this is a diagnostic bridge until the crate is made stable-compatible. |
| Live | `SERVER_TEST_BACKENDS=1 nu .config/scripts/full-check.nu --live` | Default checks plus required TCP tests against configured catalog/vector/object-store services. Missing opt-in is a failure. Live pipeline uses bin/driver. |
| Infrastructure | `nu .config/scripts/full-check.nu --nix` | Default checks plus `nix flake check --no-update-lock-file`. A requested Nix failure is a failure, never a skip. |
| Nightly | `nu .config/scripts/full-check.nu --nightly` | Mutation (`cargo-mutants`) and fuzz inventory (`cargo fuzz list`) gates. Each missing requested tool fails instead of becoming a green no-op. |

## Exact Commands

- Proptest: `PROPTEST_CASES=256 cargo test --workspace`. This runs registered property tests. The existing `proptest!` block at `workspace/index/tests/object_pack/adversarial.rs` is not currently a Cargo target according to `cargo metadata`, so the baseline does not claim that file is covered.
- Fuzz: `cargo fuzz list` should report `decode-entry` and `decode-body`. Run `cargo fuzz run decode-entry -- -max_total_time=60` or `cargo fuzz run decode-body -- -max_total_time=60`; generated corpora/artifacts remain ignored.
- Mutants: `nu .config/scripts/full-check.nu --nightly` runs `cargo mutants --package nudox-engine --no-shuffle` when `cargo-mutants` is installed. Mutation testing is intentionally not part of the default gate.
- Benchmark: `cargo test -p ir-vcs --release -- --ignored --nocapture bench_replay_vs_snapshot` runs the existing ignored timing/invariant test. There is no Cargo `benches/` harness in this repository.
- GUI semantic tests: `cargo test --manifest-path workspace/gui/Cargo.toml --test shell_flow` and `cargo test --manifest-path workspace/gui/Cargo.toml --test adversarial`.
- The GUI can be started with `cargo run --manifest-path workspace/gui/Cargo.toml`. There is no in-repository screenshot capture or pixel-comparison CI, and no screenshot coverage is claimed by any check.

The Buck command used by the default tier when available is
`buck2 test //workspace/compiler/...`. If `buck2` is absent, the script prints
an explicit skip instead of silently claiming compiler coverage.

## Measurement

`crates/nudox-test-support` provides zero-dependency wall, disk, and RSS
measurement helpers (`RunCost`, `measured()`, `disk_bytes()`). RSS is read via
`getrusage` (macOS/BSD) or `/proc/self/status` VmHWM (Linux). The crate has no
production dependencies and never fails a test on unsupported platforms.

The backend-image check measures and reports source-file download with ETag
validation via `tests/lib/http.nu:download-package-file`. The ETag header is
required and the response bytes must be non-empty.

## Nix Infrastructure

The live pipeline (`tests/backend-image/check.nu`) uses `bin/driver` from the
server derivation. The OCI image config (entrypoint, ports, env, labels) is
asserted by `tests/lib/oci.nu` against the nix2container JSON.
