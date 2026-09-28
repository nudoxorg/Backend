This repository's own dependency tree, captured on 2026-09-27:

- `metadata.json`: `cargo metadata --offline --locked --format-version 1
  --filter-platform aarch64-apple-darwin`, trimmed to the fields the tree
  reader uses, with the checkout path rewritten to `/workspace/backend`.
- `Cargo.lock`: the lockfile at the same moment.

The tests read real facts from it (toml twice, bincode 1.3.3 under syntect),
so it is pinned rather than read from the live workspace, which moves.
