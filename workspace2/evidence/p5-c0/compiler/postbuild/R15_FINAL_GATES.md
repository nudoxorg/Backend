# R15 two final clean gates

At source/evidence checkpoint `30a69579a88bd407fbf164017d6661683541320f`, two independent dedicated
`planes/compiler/target-c0-compiler` gates each performed package cleanup, verified zero registry and
vocab rlibs, rebuilt, then verified exactly one of each. Each passed:

```text
cargo test --locked --workspace --all-targets
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Each test run passed four dispatch tests, the subset compiler-process test, and the release example.
The subset terminal was ordinary clean text only (`typescript_lower_is_a_causal_absent_member ... ok`),
with no binary metadata. The dedicated cache was package-cleaned and then removed after each run; final
working-tree status is clean. No source, manifest, or lockfile changed between the two gates.
