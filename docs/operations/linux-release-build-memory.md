# Linux Release Build Memory

## Purpose: bound concurrent compiler memory use.

## Evidence: eight-job SIGKILL; historical two-job build completed.

On the reported Linux host with 15 GiB of RAM and no swap, an eight-job release build ended with a `rustc` SIGKILL. The report did not include kernel evidence, so the OOM diagnosis remains unconfirmed. On 2026-10-03, a release build of `76587b45c` plus the worker's `.parse::<u64>().ok()?` fix completed with two Cargo jobs in 18m 27s. The follow-up report at `6cf083681` still failed with two jobs because of source errors. The historical success does not validate the current source or guarantee that two jobs fit every release build.

For hosts with similar memory, run:

```sh
cargo build --release --locked -j 2
```

This limits concurrent compiler processes for that invocation while retaining the repository's release profile (`codegen-units = 1`, thin LTO, and symbol stripping). Keep the default profile and choose a higher job count only when the host's measured memory headroom supports it; the evidence does not justify a workspace-wide concurrency or optimization change.

## 2026-10-04 repair verification

The Linux Build Failures 3 report identifies two ordinary Rust errors at `c6dd09e2d`: moving the owned Rust import name before borrowing it, and mapping an accessibility harness error as `GalleryError`. Commits `02da6ca8f7` and `1ffd4034f9` fix those boundaries; `66139a6278` ignores all of the generated root `build/` directory. These changes were first published in canonical `fb526fe5423256e023227b01bb8531e56e170c9d`.

Actual checks, with source identity verified before and after each command:

| Source | Check | Result |
| --- | --- | --- |
| `fb526fe542` | macOS workspace/all-targets check with GUI harness features | Passed. |
| `fb526fe542` | Compiler library tests | 130 passed. |
| `956bdf8308648baca42861c79c8c234562c76f47` | macOS workspace/all-targets Clippy, including desktop visual harness and local-service search benchmark features | Exit 0, unchanged tree `f7afeb424058db5a0a1802e25dcb03477541e2a3`. |

The final Clippy command was:

```sh
cargo clippy --locked --offline -j 1 --workspace --all-targets --keep-going   --features backend-desktop/visual-harness,backend-local-service/search-bench   --message-format=json
```

That run used pinned Rust/Cargo 1.97.1 and completed on 2026-10-04 at 23:29:11 UTC. Its raw output SHA-256 is `7e9164415477fb1e4659e70f3b9188f42b12dbf17ca340db68659d4ab48fb439`. It clears the configured denied lints and type errors; it was **not** run with `-D warnings`, and the warning backlog remains. Clippy compiles test code but does not execute tests or launch the GUI.

A separate Linux x86-64 release attempt used clean `fb526fe542`, pinned Rust/Cargo 1.97.1, `cargo build --workspace --locked --offline --release -j 2 --keep-going`, an enforced 15 GiB cgroup memory maximum, and zero swap. It reached the desktop and remaining workspace crates without an observed compiler error. The last recorded peak was about 8.27 GB (decimal); all recorded OOM/swap events were zero. The run was stopped after other workloads exceeded the four-build capacity limit. Cargo's terminal exit was not observed, and the desktop/CLI/MCP/locald executables were not produced. This is **interrupted evidence, not a successful release build or a proof that the complete build fits 15 GiB**. The cached objects are retained for a separately admitted continuation.

The Linux host was NixOS with Rust 1.97.1, not the reported Parrot/Rust 1.99.0 host. The exact Parrot release command, complete Linux executable/runtime checks, fresh macOS bundle launch, signing/notarization, and native Windows behavior remain separate validation gates. Wine exclusions and cross-compilation are not native Windows behavioral proof.
