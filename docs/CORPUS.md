# Corpus

The real-package corpus is a Nix-owned test input. Its package declarations,
URLs, roles, and fixed-output hashes live in [nix/corpus.nix](../nix/corpus.nix).
Measured entry expectations live in [nix/entry-baseline.toml](../nix/entry-baseline.toml).

The corpus is not materialized in the checkout. The flake test creates its
single temporary Nix-store output and passes that output to the integration
checks. Ordinary workspace commands therefore have no `result` or
`result` directories to maintain.

Run the corpus-backed checks with:

```bash
nix build .#checks.$(nix eval --raw --impure --expr 'builtins.currentSystem').corpus
```

The package catalog is deliberately data-only in `nix/corpus.nix`; adding a
package means adding its Nix record and fixed-output hash there. Do not add a
second TOML manifest or a fetch script.
