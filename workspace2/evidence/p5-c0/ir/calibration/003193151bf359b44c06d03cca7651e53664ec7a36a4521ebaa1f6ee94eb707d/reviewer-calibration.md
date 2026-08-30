# Terra reviewer calibration — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_legal_reviewer_calibration`
Model: `gpt-5.6-terra`
Spawn contract: `fork_turns: none`; read-only; card, governing skills, and frozen skeleton only.

Custody verified:

- Worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- Branch: `codex/prototype-real-compiler-ir`
- Card path/current SHA: `workspace2/P5_C0_MANAGER_CARD.md` — `003193151bf359b44c06d03cca7651e53664ec7a36a4521ebaa1f6ee94eb707d`
- Required `git show 17af65fc...:workspace2/P5_C0_MANAGER_CARD.md | shasum -a 256`: exact expected SHA.
- Skeleton: `workspace2/evidence/p5-c0/ir/skeleton/coordinates.rs`, SHA `7c851ed7fd63805340cf5d8a0f02920b86b072ee8ce7de363c790e15be8dc21f`, 95 lines.

Literal review-rust-gem tripwire table:

| tripwire | count | exact locations | disposition | evidence or finding ID |
|---|---:|---|---|---|
| panic/unwrap/expect/unreachable | 0 | `workspace2/evidence/p5-c0/ir/skeleton/coordinates.rs:1-95` (scanned `panic!`, `unwrap`, `expect`, `unreachable!`) | cleared in supplied skeleton | TRIP-01 |
| source-dropping conversion or map_err | 0 | `coordinates.rs:1-95` (scanned `map_err`, conversion impls) | cleared | TRIP-02 |
| lossy/ambiguous From/TryFrom or raw authority bypass | 0 | `coordinates.rs:1-95` (scanned `From`, `TryFrom`, raw-byte/rebrand paths) | cleared; `EntityId::new`/`TypeId::new` are the card-authorized local dense-coordinate construction | TRIP-03 |
| checked-arithmetic sentinel/saturation or operand loss | 0 | `coordinates.rs:1-95` | cleared | TRIP-04 |
| dyn/Box/Vec/Arc/Rc | 0 | `coordinates.rs:1-95` | cleared | TRIP-05 |
| public tuple fields or positional semantic tuples | 0 | `coordinates.rs:1-95` | cleared | TRIP-06 |
| unit/stateless namespace structs | 0 | `coordinates.rs:1-95` | cleared | TRIP-07 |
| public local traits or one-implementation delegation | 0 | `coordinates.rs:1-95` | cleared | TRIP-08 |
| one-letter generic parameters | 0 | `coordinates.rs:1-95` | cleared | TRIP-09 |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 0 | `coordinates.rs:1-95`; `7` is a named fixture coordinate at lines 11–14 and `2024` is the explicit compiler edition at line 48 | required/clear; no semantic magic number in a listed class | TRIP-10 |
| test-only Option/discarded results/success-only assertions | 2 | `coordinates.rs:26` (`Option` artifact-discovery state); `:92` (legal-mutant success assertion) | required: Option holds zero-or-one discovered rlib and second candidate errors; success assertion is causal-mutant proof, paired with `:93` false-predicate assertion | TRIP-11 |
| unsafe/SIMD/allocator/dependency additions | 0 | `coordinates.rs:1-95` | cleared from skeleton text; manifest/dependency graph unverified | TRIP-12 |
| public item without current consumer and falsifier | 0 | `coordinates.rs:1-95` | cleared; skeleton declares no public item | TRIP-13 |

Seeded-defect attacks:

- First-match/stale rlib: rejected for first-match selection; `rlib` scans all entries and fails on zero or second candidate (`:25-42`). A sole stale artifact is not disproven by the skeleton alone; the card’s clean-build/digest evidence is required.
- Total-coded-error versus E0308-only: rejected; `mismatch` counts all `error[E` diagnostics before requiring E0308 (`:69-76`), rather than merely locating E0308.
- Legal `TypeId` mutant causal success: rejected; it compiles the legal source, requires successful compilation, then requires the diagnostic predicate to be false (`:90-93`).
- Shadow fixture: rejected; stdin source imports `nudox_ir_vocab`, and `compile` passes exactly the resolved artifact via `--extern` (`:13-14`, `:44-56`).
- Raw conversion: rejected; no conversion/rebrand API appears; only the expressly authorized local constructor is used.
- Panic terminal: rejected; compiler invocation failures propagate through `io::Result`, with no panic terminal (`:44-63`, `:86-94`).
- Unlisted compiler edit: unverified, not cleared. The supplied card prohibits it, but I did not inspect compiler paths, candidate diffs, or history.
- Reserve/budget honesty: cleared for the frozen skeleton: 95 total lines equals the 14-line baseline plus 81 forecast delta, leaving exactly 15 lines under the 110 hard cap.

Strongest counterexample: arrange a single obsolete-but-matching `libnudox_ir_vocab-*.rlib` in the target directory. The skeleton correctly rejects ambiguity, but cannot itself prove freshness or source/rlib correspondence; the card’s mandatory clean preparation, exact-one command, source/rlib digests, retained command, and raw stderr are the necessary falsifier-bound evidence.

Hidden cost: two out-of-process `rustc` invocations plus directory scanning and stderr buffering in a test-only public-consumer fixture; no production allocation, dispatch, or representation cost is introduced by the supplied skeleton.

Simple-std status: cleared at skeleton level. It uses only `core`/`std` and the actual public vocabulary artifact; no extra crate, fixture crate, allocation container, unsafe, macro, or runtime tag is present.

Unverified:

- The deliberately unopened pinned comparison path `domains/ir/crates/nudox-ir-vocab/src/lib.rs` is **UNVERIFIED**, not assumed clear.
- Candidate/worktree diffs, compiler-path immutability, manifests/lockfile, dependency graph, current target contents, raw stderr, actual compiler execution, full gates, source/rlib binding evidence, and retained evidence files are all **UNVERIFIED** by this read-only calibration.
- No implementation acceptance is implied.

Calibration-only verdict: **PASS for the frozen card/skeleton’s specified seeded-defect defenses, conditional on the card’s later external evidence gates.** This is not implementation acceptance or authority to edit.
