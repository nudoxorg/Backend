# Plausible misreader — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_legal_plausible_misreader`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Custody verified:

- Worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- Branch: `codex/prototype-real-compiler-ir`
- Card commit: `17af65fcda9ed091a2a75f6dfd2d96d0d60aa76d`
- Card SHA-256: `003193151bf359b44c06d03cca7651e53664ec7a36a4521ebaa1f6ee94eb707d`
- Tree: clean

Plausible hurried misimplementation: add `tests/coordinates.rs` that discovers `libnudox_ir_vocab-*.rlib`, selects the first match, invokes `rustc`, and uses a broad `stderr.contains("E0308") && contains("EntityId") && contains("TypeId")` predicate. If artifact discovery is inconvenient, define local shadow `EntityId`/`TypeId` structs with the same names. Add an unrelated undeclared identifier to the forbidden fixture, and accept the result without compiling a legal `TypeId` mutant. This appears to test the desired diagnostic while avoiding the exact artifact and mutant discipline.

Literal disposition:

- Stale artifact: rejected. Clean preparation plus the fixture’s independent exact-one-rlib check, source/rlib digests, resolved path, and retained command/stderr are mandatory. A stale or unbound rlib is causal failure.
- Multiple rlibs: rejected. Zero or more than one candidate is an immediate causal failure; “first match” is forbidden.
- Shadow types: rejected. The terminal must import the actual public `nudox_ir_vocab` artifact. Local same-named types do not prove the public brand boundary.
- Extra compiler errors: rejected. The predicate must count all coded errors and require exactly one `E0308`; unrelated fixture errors invalidate the proof.
- Legal mutant failure: rejected. Replacing the bad `EntityId` expression with `TypeId` must compile successfully, and the diagnostic predicate must then fail. If it remains green, the predicate is noncausal.
- Source write: rejected immediately. `src/lib.rs` is read-only and must remain byte-identical to its pinned digest; any difference stops the checkpoint even at unchanged formatted LOC.
- Reserve breach: rejected. The 95-line skeleton must remain under the 110-line hard cap with 15 literal unused lines. Consuming reserve, exceeding 20%/25-line forecast variance, or adding an unplanned public item requires re-card.
- Scope creep: rejected. Any compiler, manifest, lockfile, dependency, conversion API, allocation policy, unsafe/SIMD, registry, C0-COMPILER, C1, or other unlisted-path change is outside this C0-IR card.

The smallest falsifier is the exact public-consumer compiler process: one actual rlib passed with `--extern`, one intentional kind mismatch, all-error counting, retained raw stderr, and the successful legal `TypeId` mutant whose predicate must be false. Any shortcut above fails at least one of those literals.

Hard blockers/ambiguities: none found in the frozen card. The wording clearly separates the read-only source digest path from the sole writable test path, the one-rlib artifact requirement from the legal mutant, and the 95/110/15 LOC accounting.

Unverified by this read-only calibration: actual compiler execution, toolchain version, rlib freshness, retained stderr, final source/test digests, and implementation gates. No source or skeleton inspection was performed.

Calibration-only verdict: `PASS` for the card’s adversarial contract—the plausible misreader is detectable and must be rejected. The described implementation itself is `REJECT`; this grants no builder approval or implementation acceptance.
