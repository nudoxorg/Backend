# P2 control card: final calibration review record

## Frozen specimen and explicit model proof

The final specimen was `CONTROL_CARD.md` at manager commit `7d351ebc1cc66d8d12a1b8fc5560d7817087880d`, SHA-256 `8e215f2d7623006f3d2b9e69a1ca36d778386480d72269bc750164150a91680e`.

| Calibration role | Task ID | Explicit model | Fork | Result |
|---|---|---|---|---|
| fresh reader | `/root/p2_manager/p2_control_cold_reader_v3` | `gpt-5.6-luna` | `none` | accurate restatement, then plausible-misreader attack |
| fresh reviewer | `/root/p2_manager/p2_control_reviewer_v3` | `gpt-5.6-terra` | `none` | read-only final review, blocked |

## Independent reader and plausible-misreader result

The reader independently restated the fixed 32/92-byte format, five-command terminal, strict reduce/write/sync/state ordering, poison/reopen rules, exact path list, public receipt surface, normal and feature-gated gates, dependency set, budgets, and negative space. It then tried the cheapest compliant-looking patch: an exclusive `FileJournal`, fixed buffers, a feature-gated fault path, five-event happy path, sequential reopen, public helper/derive additions, and reduced fault assertions.

The card stopped these shortcuts: early effect/receipt through forced sync failure, partial/full truncation through the `0..=92` schedule, retained replay logs/shared oracle, nonregular paths, raw receipt literal/conversion, async/MPSC/Arc/head additions, and unknown generated paths. The reader nevertheless identified ambiguities about prototype-private versus public receipt wording, exact test fixtures, full-write physical persistence, feature test-hook visibility, allocation measurement scope, and future-phase preparation. Those were critique candidates, not a new production authorization.

## Final reviewer findings

### BLOCKER: recovery decode and reducer rejection have no authorized public error variants

The card requires frame checksum, sequence, canonical `WorkflowRecord` decode, and `reduce` validation in that order, but its exact `OpenError` inventory contained only path/I/O/header/checksum/sequence classes. `WorkflowRecordError` and `ReductionError` are distinct source-bearing baseline facts. A builder would have to add an unapproved public error, erase/misclassify an error, or fail to compile the specified behavior.

Required future falsifier: checksum-valid frame with a corrupt workflow event tag must return a source-bearing frame-decode error; checksum-valid frame zero containing `Admitted` must return a source-bearing frame-reduction error. Both retain full length, perform no repair, and construct no journal.

### BLOCKER: checked `SequenceExhausted` has no executable test seam

The card requires checked end arithmetic and `SequenceExhausted` before any write, but reaching the overflow append through real frames is infeasible and no testable arithmetic boundary was authorized. Wrapping/saturating or one-sided checking could pass every listed gate.

Required future falsifier: exercise `u64::MAX` and the first end-offset overflow through one narrowly private checked-arithmetic boundary; assert no write attempt, unchanged recovery, no receipt/effect, and the exact error operands.

### MAJOR: no-follow symlink behavior is not provable with the allowed safe-std control

The card said symlinks are rejected before opening. A `symlink_metadata` then open sequence races with replacement, so a stable-fixture test cannot prove the absolute rule. The smallest future decision is either to narrow the control to a stable-path best-effort policy and mark replacement races unverified, or authorize a reviewed platform no-follow adapter with portability/fault evidence.

## Cleared suspicions

The reviewer cleared feature-on/feature-off divergence as a card law after the normal gates were added; effect-before-sync; partial and full write recovery; resumable repaired reopen; strict physical sequence mismatch; header/full-frame corruption; raw receipt bridges; MPSC/async/Arc/head smuggling; directory sync; source-bearing I/O; and broad evidence-path scope. Those remain card requirements, but none was implemented because the unresolved blockers prohibit builder authority.

## Pre-edit tripwire result

All adapter candidate source, test, fixture, manifest, and lock paths were absent at this specimen. Thus any zero source count is only an absent-path scan, not evidence of a clean implementation. The reviewer recorded card-only mentions of forbidden `panic/unwrap/expect/unreachable`, `map_err`, raw conversion, `dyn/Box/Vec/Arc/Rc`, arithmetic, and dependency words; every post-build scan remains required by the rejected card. No reviewer supplied a numerical score.
