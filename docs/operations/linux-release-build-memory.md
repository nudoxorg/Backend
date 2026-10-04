# Linux Release Build Memory

## Purpose: bound concurrent compiler memory use.

## Evidence: eight-job SIGKILL; historical two-job build completed.

On the reported Linux host with 15 GiB of RAM and no swap, an eight-job release build ended with a `rustc` SIGKILL. The report did not include kernel evidence, so the OOM diagnosis remains unconfirmed. On 2026-10-03, a release build of `76587b45c` plus the worker's `.parse::<u64>().ok()?` fix completed with two Cargo jobs in 18m 27s. The follow-up report at `6cf083681` still failed with two jobs because of source errors. The historical success does not validate the current source or guarantee that two jobs fit every release build.

For hosts with similar memory, run:

```sh
cargo build --release --locked -j 2
```

This limits concurrent compiler processes for that invocation while retaining the repository's release profile (`codegen-units = 1`, thin LTO, and symbol stripping). Keep the default profile and choose a higher job count only when the host's measured memory headroom supports it; the evidence does not justify a workspace-wide concurrency or optimization change.
