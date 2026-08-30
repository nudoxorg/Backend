# C1-FORMAT R3 independent Sol reproduction

## Custody

```text
candidate: 51398c2955ca43c7e9f9b3469e85ff09d6c8c24f
source baseline: 527de5bbc37e3157ad254a401aeecd70d1910c0c
rustc: 1.97.1 (8bab26f4f 2026-07-14)
cargo: 1.97.1 (c980f4866 2026-06-30)
host: aarch64-apple-darwin
fresh target: /private/tmp/p5-c1-r3-sol.kjFsol
captured terminal: /private/tmp/p5-c1-r3-sol-log.pQ7VDE
```

The target/log paths are ephemeral reproduction custody. The rlib hash identifies only the exact
artifact consumed by this run.

## Independent gate

```text
cargo test --locked --workspace --all-targets -- --nocapture: PASS
format unit tests: 1 passed
format integration tests: 5 passed
vocabulary integration tests: 2 passed
cargo fmt --all -- --check: PASS
cargo clippy --locked --workspace --all-targets -- -D warnings: PASS
git diff --check: PASS
format rlib cardinality: 1
captured terminal NUL bytes: 0
```

The actual-rlib test independently resolved and consumed:

```text
path=/private/tmp/p5-c1-r3-sol.kjFsol/debug/deps/libnudox_ir_format-b61fc910457ad02a.rlib
cardinality=1
sha256=60cefc807b5639d05eb7152002a4fed885e1737ef7c2f9cb458a10ef4c429af9
```

Source identities:

```text
manifest afeb037e49f08d479c39a866e06ec6ca8ffbbaeace5a52978682615bfcc1cb9c
library  cc5506ad173201b6cd7a4015757a52bf8e4f41b2fa8393814eca6919c8ef26ca
tests    94a02b1ccde791dacf4478779e3bbe839365869e4269360a2951be4c5af9b1cd
```

## Hostile source audit

The live format source/test tree contains no `unsafe`, allocator hook, `Vec`, `to_vec`, panic-style
`unwrap`, or panic-style `expect`. The fixed stack corpus covers 0/1/2 lanes, every strict golden
prefix, every header cell, trailing geometry, every payload byte, exact-size fused exhaustion, and
private correlated lane containment. The compiler-process deck uses the actual single exported rlib,
null stdout, exact one-primary diagnostics, a legal named-validation consumer, and a causal noisy-error
rejection.

The result proves only the first C1 child slice: a padding-free, two-lane borrowed validator/view.
Allocation/copy cost, optimized whole-consumer codegen, builders, type DAGs, atoms, pooled lists,
external references, frontend lowering, and every C2 capability remain unverified and unclaimed.

**PROMOTE FOR FUTURE INTEGRATION REVIEW**
