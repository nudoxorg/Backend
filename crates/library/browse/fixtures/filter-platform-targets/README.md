# Filtered target metadata fixture

These snapshots were captured from a local, offline-only workspace using
Cargo 1.98.1 (Homebrew build `797e8a9bc`, 2026-08-05). The workspace has one
member, a normal path dependency, and a `cfg(windows)` path dependency. It has
no build scripts or registry dependencies. The checked-in `Cargo.lock` and
manifests make the sample reproducible without a network connection.

The capture commands were:

```sh
cargo metadata --format-version 1 --offline --filter-platform x86_64-apple-darwin --manifest-path Cargo.toml
cargo metadata --format-version 1 --offline --filter-platform x86_64-pc-windows-msvc --manifest-path Cargo.toml
```

The temporary workspace path was replaced with `/fixture/workspace` in both
JSON files. Cargo 1.98.1 omitted the Windows-only package row from the Darwin
response, while retaining its manifest dependency declaration; on Windows,
the row and resolved edge are present. Cargo's `--filter-platform` contract
filters the `resolve` graph, so the regression also grafts the captured
Windows package row onto the Darwin `packages` array. That models a response
whose package descriptions include an inactive manifest dependency and
ensures reachability, rather than package-array presence, controls the active
inventory.
