# Pre-edit review packet: wave-d8-seam-readiness

## Terminal

One Nix-invoked offline command emits a deterministic machine-readable documented-public-seam
readiness record from actual locked Cargo metadata for `Cargo.toml`, `domains/ir/Cargo.toml`,
`planes/compiler/Cargo.toml`, and `planes/index/Cargo.toml`. `--require-ready` is nonzero on
`BLOCKED`.

## Fixed contract

- Documented package sets are the literal names in `brief.md` for those four manifests.
- Current package presence is a source fact, not a product implementation claim.
- Missing Trustfall graph, Qdrant/local vector, adaptive placement, Tantivy lexical, and unified
  in-process/CLI/MCP/GPUI public package/API documentation are literal
  `UNDOCUMENTED_PUBLIC_SEAM` blockers.
- Tooling only: `flake.nix`, two scripts under `tools/`, and capability evidence. No Cargo manifest,
  product source, shipping dependency, test-only crate, or product API change.
- The command must be offline after Nix closure realization and make exactly four `--no-deps`,
  `--locked`, `--offline` metadata calls.
- Mutation evidence must kill added, removed, duplicated, malformed, and failed metadata inventories.

## Required evidence

- Current output is deterministic, JSON-parseable, and `BLOCKED` for exact documented reasons.
- `--require-ready` is nonzero for current and mutated blockers.
- The mutation test proves the normal command does not use a constant/fake inventory.
- No product behavior, public API suitability, or Wave D.8 closure is claimed from package presence.
