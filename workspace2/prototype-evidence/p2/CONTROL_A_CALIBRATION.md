# P2 control A: calibration round 1 (rejected)

## Frozen specimen and topology proof

The rejected specimen was `CONTROL_A_CARD.md` at manager commit
`90eade8436dcaecfd6224a517ad0dbf9d75bf734`, SHA-256
`2247e7157332a872ed4eefeca7caeb5b21a669528012225f9a3047147b082025`.
It was based on clean source candidate
`9a14f069d593c748abd5c036fc2d5415b74d2012`.

| Role | Task ID | Explicit model | Fork | Access |
|---|---|---|---|---|
| cold reader and plausible misreader | `/root/p2_control_manager/control_a_luna_calibration` | `gpt-5.6-luna` | `none` | read-only; no edits or commits |
| blind pre-edit reviewer | `/root/p2_control_manager/control_a_terra_preedit` | `gpt-5.6-terra` | `none` | read-only; no edits or commits |

The reader/restater and reviewer were spawned with explicit model overrides and non-inheriting
forks. Neither saw a builder rationale; no builder received edit authority.

## Reader result

The Luna independently restated the exact five-event terminal, the twelve builder paths and eleven
enumerated evidence paths, 32/92-byte geometry, upstream 68-byte canonical record, reopen order,
source-bearing `FrameDecode`/`FrameReduction`, exact arithmetic values, `0..=92` behavior, stable-path
TOCTOU limitation, normal/feature surface, resource claim, caps, and stop rules. It cleared the
geometry against `nudox-workflow/src/durable.rs`, the publish effect against `reduce.rs`/`recovery.rs`,
and the source types against the direct workflow APIs.

Its plausible shortcuts were: retrying a partial write after the terminal injected I/O error;
exposing a normal-build receipt/fault bridge; retaining/using production replay in the oracle; and
treating the complete 92-byte fault as caller success. The original card rules rejected those
shortcuts. It found one custody ambiguity: the card called `9a14…` the baseline while the trial was
at `90eade…`; the rewrite now distinguishes underlying source baseline from the manager card commit.

## Blind reviewer result

The Terra review rejected the specimen before build with two blockers and five majors.

1. **BLOCKER — forgeable `AppendSuccess`:** the required public receipt/reduction fields allowed a
   downstream mixed literal from unrelated success values. The rewrite makes both fields private,
   lends each independently valid fact, and adds an exact compile-fail mixing fixture.
2. **BLOCKER — overclaimed test-only fault authority:** any public feature-gated API is reachable by
   any dependent that enables the feature. The rewrite names the feature API exactly and states that
   reachability honestly, while keeping it absent feature-off.
3. **MAJOR — missing malformed-header error matrix:** the rewrite names `HeaderTruncated` for
   existing lengths `0..=31`, header mutation priority/operands, and source-bearing I/O rows.
4. **MAJOR — incomplete normal API skeleton:** the rewrite names all normal methods, receivers,
   results, public error/support types, variants, fields, and payloads.
5. **MAJOR — private arithmetic test had no authorized location:** the rewrite authorizes only the
   private pure `journal.rs` unit for the infeasible overflow inputs.
6. **MAJOR — feature equivalence had no oracle:** the rewrite requires the identical nonfault
   golden/reopen/corruption/poison/resume corpus and byte/result/surface comparison under both
   feature states, plus a source audit of every feature gate.
7. **MAJOR — no allocation evidence row:** the rewrite requires an isolated warmed allocation
   counter for nonempty append/reopen and a separate retained-record source scan.

The reviewer cleared durable-end arithmetic (`MAX_DURABLE_SEQUENCE = 200508087757712516`), geometry
and preimages, source retention, 92-byte complete-write error recovery, and the limited stable-path
policy. Its strongest counterexample was constructing one public success result from receipt A and
reduction B, plus enabling the public fault feature in a dependent crate.

## Mechanical pre-edit tripwire

All candidate adapter source, test, fixture, manifest, and lock paths were absent at this rejected
specimen. Therefore all production-code tripwire counts were zero only because no candidate existed.
The reviewer specifically recorded zero source hits for panic/unwrap/expect/unreachable, source-
dropping conversion/map_err, raw receipt bridge, arithmetic saturation, dyn/Box/Vec/Arc/Rc, public
tuples, stateless namespace types, public local traits, one-letter generics, test-only discarded
results, unsafe/SIMD, and added dependencies. Card-only findings above were not treated as source
evidence.

## Calibration result

Round 1 failed and no implementation worker was authorized. The manager replaced the active card as
one canonical body, committed this raw record with that replacement, and must run fresh Luna and
fresh Terra calibration against the new digest before any builder card. Historical `root-review-
durable` remains rejected counterexample corpus only and was neither copied nor cherry-picked.

## Calibration round 2 (rejected)

The second specimen was `CONTROL_A_CARD.md` at manager commit
`a1a7de44ecc97cf0db661d8ea8f5ac2b223466b1`, SHA-256
`0a208aadc2c93e1ed3da344e96fa763937ee589aa4c2c6ec54d42c4e22118dca`.

| Role | Task ID | Explicit model | Fork | Access |
|---|---|---|---|---|
| fresh cold reader and plausible misreader | `/root/p2_control_manager/control_a_luna_calibration_r2` | `gpt-5.6-luna` | `none` | read-only; no edits or commits |
| fresh blind pre-edit reviewer | `/root/p2_control_manager/control_a_terra_preedit_r2` | `gpt-5.6-terra` | `none` | read-only; no edits or commits |

The Luna found three genuine majors: the literal skeleton listed unit `RecoveryObservation::Healthy`
while prose required its `Recovery` payload; receipt accessor signatures were absent and the
raw-integer prohibition could be read to ban the needed scalar fact accessors; and the per-file/phase
stop arithmetic had no denominator or threshold rule. The final rewrite states
`Healthy(Recovery)`, signatures returning the declared facts, and exact file/phase formulas.

The Terra found one major: feature-on deliberately exposes fault API while feature-off omits it, so
full public-surface identity was impossible. The final rewrite compares feature-off with the
feature-on nonfault projection and names the entire allowed feature delta. The Terra cleared the
unforgeable private success pair, honest fault-feature reachability, error skeleton, private
arithmetic unit, header priority, source-bearing decode/reduction, 92-byte recovery, allocation row,
and stable-path limitation. Its tripwire scan found no candidate source; its only card-level finding
was the feature comparison contradiction.

This is the second and final repair round for this new control-A card family. No builder is authorized
unless a fresh third calibration against the final digest has zero blockers and majors; any remaining
semantic card failure is an authority fork back to the parent rather than another rewrite.
