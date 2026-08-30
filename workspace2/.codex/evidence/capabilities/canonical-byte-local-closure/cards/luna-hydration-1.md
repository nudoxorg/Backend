# Frozen Luna card: borrowed hydration planning checkpoint

Registered role: `nudox_luna_implementer`; expected runtime `gpt-5.6-luna` / `max`; config `.codex/agents/nudox-luna-implementer.toml`.

Baseline: controller commit `9d9738a1ae7ded73f8e9a532deb4515f88eb4f9f`, tree `9fed8113fb07b807a8296fe16f9c3dc0cf23329e`. Allowed paths are **only** `crates/nudox-hydration/**` and this capability evidence directory. Never edit root, operation, the chief terminal/brief, any manifest, object/object-pack/store controls, compiler/index/durability/controller, or shared documentation/skills.

## One terminal

Add `Need::bind_borrowed` and `plan_borrowed` over current `nudox_root::BorrowedGenerationView`, preserving the exact existing `DemandBindError::GenerationMismatch` operands and `PlanError` causes. The result must give the chief its existing planning surface (`required`, presence/absence routes, `coverage`, `dep_set`, `stage().verify`) and return the existing sealed `VerifiedGeneration` after full verification. Do not implement operation binding.

## Existing root control and hydration representation

`BorrowedGenerationView::select_closure` already exposes a borrowing selected iterator, compact count, canonical order, and caller `ClosureScratch` selection. Root is read-only. If that surface is insufficient for an allocation-free reusable plan, return the exact missing public symbol and do not edit root. Otherwise add a hydration-only borrowed selected-ordinal plan representation using **only** existing caller-owned `PlanScratch`; it may replace/refactor its internal ordinal storage when existing owned planning behavior, retained-byte facts, and tests remain exact. It may not retain descriptors, root rows, locality backing, a closure, a predicate, `Box`, `Arc`, `dyn`, unsafe, new dependency, generic adapter, or a second scratch owner.

Use the selected iterator once to write the existing canonical dependency-set preimage and classify presence/locality. The returned plan may revisit its borrowed validated selection with stored caller-owned ordinal positions, but must not reparse root bytes or re-run the caller presence predicate. Preserve complete versus range semantics and every error source.

## Tests and gates

Add owning `nudox-hydration` tests for borrowed legal complete/range plans, demand generation mismatch, undersized closure/plan scratch, present/promised/missing coverage, dependency-set parity with owned plan, partial/missing verification, canonical ordering, pointer/owner non-retention, and constant-body/input-removal falsifiers. Run:

```sh
cargo test -p nudox-hydration --locked --offline
cargo clippy -p nudox-hydration --all-targets --locked --offline -- -D warnings
cargo fmt --check
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline
```

The chief red command is expected to remain red for root/hydration symbols only until this card’s result; report its raw status without editing it. Commit one coherent passing hydration checkpoint. Return exact task/config/model/effort/sandbox receipt, commit/tree, changed paths, raw gates, public/dependency ledger, no-allocation/retention facts, strongest counterexample, and remaining red rows. Do not claim review or closure.
