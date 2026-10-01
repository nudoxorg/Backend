# Cargo 1.97.1 platform-filter fixture

`linux.json`, `windows.json`, and `project/Cargo.lock` are captured outputs from
the adjacent workspace using the pinned Cargo 1.97.1 toolchain, with
`--offline --format-version 1 --filter-platform` and no build command. The
fixture has one workspace member, a common path dependency, a Windows-only path
dependency, and a disabled optional path dependency. Cargo.lock retains both
inactive rows; the filtered resolve graph proves which dependency is active for
each target. Absolute paths inside the captured metadata are inert input data;
the test parser does not open them.

The captures were produced with:

```sh
cargo metadata --manifest-path project/Cargo.toml --locked --offline \
  --format-version 1 --filter-platform x86_64-unknown-linux-gnu
cargo metadata --manifest-path project/Cargo.toml --locked --offline \
  --format-version 1 --filter-platform x86_64-pc-windows-msvc
```
