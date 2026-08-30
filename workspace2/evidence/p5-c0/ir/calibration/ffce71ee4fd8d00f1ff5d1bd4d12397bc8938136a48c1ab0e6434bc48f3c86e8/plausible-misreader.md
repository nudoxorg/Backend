# Plausible misreader — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_plausible_misreader`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Custody verified:

- Worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- Branch: `codex/prototype-real-compiler-ir`
- Commit: `8d990dfe1d8c74ba468d2be70e5b9cf656af63ff`
- Card: `/private/tmp/nudox-prototype-real-compiler-ir/workspace2/P5_C0_MANAGER_CARD.md`
- SHA-256: `ffce71ee4fd8d00f1ff5d1bd4d12397bc8938136a48c1ab0e6434bc48f3c86e8` — exact expected digest

## Plausible hurried implementation

A superficially compliant worker might edit only `tests/coordinates.rs`, but:

- select the first matching `.rlib`;
- hard-code `domains/ir/target/debug/deps`;
- define local aliases or shadow `EntityId`/`TypeId`;
- match merely on “mismatched types” or the type names;
- permit extra compiler errors;
- skip the source digest or compare against the wrong source;
- claim the test repairs the public raw-coordinate rebrand limitation;
- grow the fixture beyond the 110-line cap.

Each is invalid.

| Attack | Literal disposition | Strongest falsifier |
|---|---|---|
| Source mutation | `BLOCKER` | Mutate `src/lib.rs`; source digest must differ from `2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78`, and acceptance must stop before trusting any rlib. |
| Matcher mutation | `BLOCKER` | Replace the expected `EntityId::new(7)` mismatch with another type error or alter the predicate to accept generic diagnostics; exact error count, `E0308`, `EntityId`, and `TypeId` assertions must fail. |
| Target mismatch | `BLOCKER` | Build/run with the advertised `CARGO_TARGET_DIR`, then resolve the rlib from the running executable’s parent dependency directory; a hard-coded or different target path must fail. |
| Stale/multiple rlib | `BLOCKER` | Create or retain zero/multiple `libnudox_ir_vocab-*.rlib` candidates; exact-one discovery must fail causally, never choose one arbitrarily. |
| Shadow aliases/local fixture types | `BLOCKER` | Remove the actual public crate import or replace it with local types; the test must no longer qualify as evidence and must be rejected. |
| Extra errors | `BLOCKER` | Add a second deliberate compile error; the counted coded-error markers must exceed one and the predicate must fail. |
| Legal mutant | `REQUIRED NEGATIVE CONTROL` | Change only `EntityId::new(7)` to `TypeId::new(7)` in otherwise identical fixture input; it must compile, and the expected-diagnostic predicate must fail. |
| Raw scope overclaim | `BLOCKER` | Public `raw` plus `new(u32)` remains a deliberate rebrand counterexample; any claim of global or wire-level unforgeability is outside C0-IR and false. |
| Reserve/scope breach | `STOP / RE-CARD` | 94-line skeleton, 110-line hard cap, and 16 unused lines are binding. Consuming the reserve, exceeding 110 lines, adding public items, or changing production code stops calibration/implementation. |

The permitted terminal is only direct typed-use safety: an ordinary public consumer rejects `EntityId` where `TypeId` is required, with exactly one causal `E0308` diagnostic, while the legal mutant compiles. No compiler, manifest, lockfile, registry, conversion API, dependency, unsafe code, or unlisted path may change.

Blockers and unverified items:

- No source, skeleton, candidate, history, evidence, or working tree was inspected, per instruction.
- Actual toolchain behavior, host layout, Cargo artifact contents, and runtime model topology remain `UNVERIFIED`.
- The known raw-coordinate rebrand limitation is explicit preserved negative space, not an ambiguity.
- Calibration grants no implementation acceptance.

Calibration-only verdict: `PASS — no permission gap found in the canonical card; builder authority remains withheld pending agreement from the other calibration roles and the separate Terra skeleton review.`
