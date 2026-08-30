# P5 C0-IR Sol reproduction

Reproduced from clean commit `461f802faa660b419fede3bdbd86b48c09fcfac2` on branch
`codex/prototype-real-compiler-ir`.

## Independent gates

From `workspace2`:

```text
CARGO_TARGET_DIR=domains/ir/target RUSTC_WRAPPER= cargo clean --manifest-path domains/ir/Cargo.toml -p nudox-ir-vocab
CARGO_TARGET_DIR=domains/ir/target RUSTC_WRAPPER= cargo fmt --manifest-path domains/ir/Cargo.toml --all -- --check
CARGO_TARGET_DIR=domains/ir/target RUSTC_WRAPPER= cargo test --locked --manifest-path domains/ir/Cargo.toml --workspace --all-targets
CARGO_TARGET_DIR=domains/ir/target RUSTC_WRAPPER= cargo clippy --locked --manifest-path domains/ir/Cargo.toml --workspace --all-targets -- -D warnings
git diff --check
git status --short
```

Results: clean removed 215 files / 9.7 MiB; the package rebuilt; both public integration tests passed;
format and Clippy passed; diff check passed; status was empty immediately after the gates.

## Cross-cutting attacks

- Source remained 34 lines at SHA-256
  `2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78`.
- The public consumer remained 93 lines at SHA-256
  `e214f0c821d2e209ae775cd153fa30422170b6654ede13b92958f68c6df04a0c`:
  net `+79`, cap `110`, reserve `17`.
- Clean construction produced one current public rlib and the fixture passed its exact path through
  `--extern`; zero or multiple candidates fail causally.
- The forbidden fixture has one coded compiler error, `E0308`, with `EntityId` and `TypeId` visible.
- Changing only `EntityId::new(7)` to `TypeId::new(7)` compiles and makes the diagnostic predicate
  false.
- No compiler, manifest, lockfile, dependency, unsafe, production allocation, or public API changed.

Strongest retained counterexample: `TypeId::new(entity.raw)` deliberately reconstructs a different
brand from a copied local coordinate. C0-IR therefore proves direct typed-use separation only; it does
not prove raw, wire, provenance, or authority unforgeability.

Remaining `UNVERIFIED`: other toolchains/targets and rlib byte reproducibility. C0-COMPILER and C1 are
not implied by this evidence.

Verdict for this narrow terminal: `RETAIN BASELINE`.
