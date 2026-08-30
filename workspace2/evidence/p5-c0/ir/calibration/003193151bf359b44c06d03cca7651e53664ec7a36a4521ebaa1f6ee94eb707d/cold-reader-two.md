# Cold reader two — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_legal_cold_reader_two`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Fresh cold-reader report.

Custody tuple (exact):

```text
worktree /private/tmp/nudox-prototype-real-compiler-ir
branch codex/prototype-real-compiler-ir
card commit 17af65fcda9ed091a2a75f6dfd2d96d0d60aa76d
card path workspace2/P5_C0_MANAGER_CARD.md
digest 003193151bf359b44c06d03cca7651e53664ec7a36a4521ebaa1f6ee94eb707d
```

The required verification command produced the exact required digest; the card is not stale.

Findings:

- Pinned boundary is unambiguous: `src/lib.rs` is read-only and digest-pinned; `tests/coordinates.rs` is the sole implementation write.
- Skeleton is 95 formatted lines, hard cap 110, with exactly 15 unused reserve. Production delta remains zero.
- The cheapest false proof is a local/shadow `EntityId`/`TypeId` fixture producing `E0308`; the card explicitly rejects this because the fixture must import the actual public `nudox_ir_vocab` rlib.
- Other cheap false proofs are blocked: extra compiler errors are counted; the sole coded error must be `E0308` naming both types; a legal `TypeId` mutant must compile and make the diagnostic predicate fail; retained stderr is mandatory.
- Clean exact-one public rlib custody is explicit and independently repeated before `rustc`; source digest, rlib digest/path, toolchain, command, and raw stderr must be retained.
- No panic/unwrap/expect/unreachable, discarded cleanup, conversion, dependency, compiler edit, manifest edit, or broader C0 surface is authorized.
- Legal mutant success, raw evidence retention, and clean-gate results remain implementation-time obligations, not currently verified.

Hard blockers / major ambiguities: none found in the frozen card. The only hard stop is any implementation violating the stated boundary, cap, exact-one rlib condition, or causal compiler-process proof.

Remaining unverified: all builder evidence, actual test contents, exact rlib custody in the eventual run, stderr retention, legal-mutant result, and full gates.

Calibration-only verdict: **PASS calibration; authorize no implementation acceptance.**
