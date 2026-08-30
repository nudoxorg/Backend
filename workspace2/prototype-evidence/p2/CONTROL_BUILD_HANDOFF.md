# P2 control experiment closure

## Verdict

**REJECT PROTOTYPE.**

The blocking control did not receive production edit authority. This is not a product judgment or production closure. The frozen-control calibration exhausted its two permitted semantic repair rounds and still has two executable durability/error-contract blockers plus one symlink-race policy fork. Implementing after that point would substitute coding for calibration.

## Custody and commits

| Item | Value |
|---|---|
| source candidate | `44c22154fd5238e4769562590420371979306050` |
| shared worktree | `/private/tmp/nudox-orchestra`, clean when verified |
| manager worktree | `/private/tmp/nudox-prototype-durable-publication-heart` |
| manager branch | `codex/prototype-durable-publication-heart` |
| manager checkpoints | `c4dad629` initial card; `44cd7e4b` round-one repair; `7d351ebc` round-two repair; this closure commit |
| rejected historical corpus | `root-review-durable` `0ff6f988f449274d1ef49434ece249a6f2f709cc`; never cherry-picked |
| production/test implementation delta | none; no adapter path was created |

## Child topology proof

| Phase | Luna task / explicit model | Terra task / explicit model |
|---|---|---|
| first calibration | `/root/p2_manager/p2_control_cold_reader` / `gpt-5.6-luna`, `fork_turns=none` | `/root/p2_manager/p2_control_reviewer` / `gpt-5.6-terra`, `fork_turns=none` |
| repaired-card calibration | `/root/p2_manager/p2_control_cold_reader_v2` / `gpt-5.6-luna`, `fork_turns=none` | `/root/p2_manager/p2_control_reviewer_v2` / `gpt-5.6-terra`, `fork_turns=none` |
| final reader/misreader/review | `/root/p2_manager/p2_control_cold_reader_v3` / `gpt-5.6-luna`, `fork_turns=none` | `/root/p2_manager/p2_control_reviewer_v3` / `gpt-5.6-terra`, `fork_turns=none` |

All children were read-only calibration roles. A Luna implementation worker was deliberately not commissioned because the final Terra calibration withheld edit authority. This preserves the requested one-Luna/one-independent-Terra capability shape without falsely relabeling a reviewer as a builder.

## Reproduced baseline evidence

`cd workspace2 && cargo test -p nudox-workflow` passed 9 unit tests, 1 integration test, and 0 doctests at the baseline. The repository-root workspace remains non-gateable because the declared `workspace/transport/Cargo.toml` is absent; Cargo metadata fails before P2.

No P2 build exists, so raw P2 fault schedules, allocation/copy/syscall/group/latency metrics, release text, dependency tree, and recovery work are intentionally absent rather than claimed. The required future artifact filenames are frozen by the rejected card but were never produced.

## Strongest surviving counterexamples

1. A checksum-valid but canonical-invalid frame and a checksum-valid first-frame `Admitted` event must each preserve their distinct decode/reducer source during reopen. The card omitted both allowed error variants, so any builder would either erase a source or add unapproved public surface.
2. `SequenceExhausted` was required before I/O but had no feasible public path or authorized narrow test boundary. A wrapping/saturating implementation could therefore appear green.
3. An absolute “reject symlink before open” guarantee is not established by a safe `symlink_metadata` then open control because of replacement races.

## Deletion and rejected-churn ledger

No production source was written then deleted. Card churn is retained in commits and the calibration records: initial card `c4dad629`; repaired cards `44cd7e4b` and `7d351ebc`; review record `CONTROL_REVIEW.md`. The historical rejected 2,212-line, 12-path journal remains isolated on `root-review-durable`; it is counterexample evidence only.

## Remaining unverified platforms and filesystem claims

Device caches/filesystems that acknowledge persistence dishonestly; real power loss; Windows directory synchronization; NFS/remote rename/CAS behavior; cross-process writers; symlink replacement race; syscall trace; x86/Linux performance; MPSC producer progress/cancellation/drop/join/reclamation; group failure fan-out; compact-head safety; head never naming absent bytes; and async completion behavior.

## Smallest future integration research card; no merge

Prerequisites: choose the stable-path versus no-follow symlink policy; freeze source-bearing `OpenError::FrameDecode` and `OpenError::FrameReduction` forms; authorize one private checked-arithmetic test boundary with exact operands; fresh cold-reader, plausible-misreader, and blind reviewer calibration must pass. Then, in a new isolated prototype worktree, implement only the blocking control A in the exact nested adapter paths, with feature-off and fault-feature gates plus the two missing falsifiers. Do not add MPSC/group commit, CAS head, shared `Published`, async adapter, or modify active typed-identity paths. This is a future research card, not a merge or integration authorization.
