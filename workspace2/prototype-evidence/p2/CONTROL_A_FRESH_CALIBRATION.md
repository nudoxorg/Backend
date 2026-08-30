# P2 control A: fresh calibration round 1 (rejected)

## Frozen specimen and topology proof

The specimen was `CONTROL_A_CARD.md` at commit
`1b76e77c`, SHA-256
`64a346145b93db75897a50a70c48ca962b4e6e902359dcca45951a9c6d00b34d`.

| Role | Task ID | Explicit model | Fork | Access |
|---|---|---|---|---|
| reader and plausible misreader | `/root/p2_build_manager/control_a_luna_reader` | `gpt-5.6-luna` | `none` | read-only; no edits or git writes |
| blind pre-edit reviewer | `/root/p2_build_manager/control_a_terra_preedit` | `gpt-5.6-terra` | `none` | read-only; no edits or git writes |

## Luna raw structured result

The reader restated the exclusive file control, five-event terminal, twelve builder paths,
32/92 grammar, zero-first sequence/end arithmetic, strict reduce/write/sync/state order,
full header/frame priority, public/private receipt surface, fault-feature delta, normal
dependency list, resource claims, 500/430/70 production and 560/474/86 test budgets, stop
arithmetic, and every command. It confirmed that the new feature-on falsifiers require
`DirectorySync`/`TailRepairSync`, `io::ErrorKind::Other`, no owner, and respectively the
exact 32-byte header or 124-byte repaired prefix.

Its plausible-misreader deck retained these executable falsifiers: complete invalid frames
must never become torn-tail repair; a terminal write error must never be retried; a sync
error must not advance recovery/effect; arithmetic must fail before I/O; source-bearing
errors must retain the injected kind; no event/record dynamic collection may be hidden by
`Vec`, `Box`, `Arc`, `Rc`, `collect`, or preallocation; the feature delta cannot leak; mixed
success fields must not compile; and the oracle cannot delegate to production code.

It identified one real resource wording ambiguity: creation must open a parent directory to
sync it, while the card said “one exclusive `File`.” A builder could retain that descriptor
or claim it was within the one-file resource law. Its falsifier is a field/descriptor ledger
showing retained and creation/reopen/append peak owners. This was not an implementation
recommendation and was carried to the parent authority decision.

## Terra review result

The independent reviewer rejected the card with one blocker. `WorkflowRecord::from(event)`
can create the record, but its only canonical-byte operation is
`nudox_id::FixedCanonicalRecord::canonical_bytes`; `nudox-workflow` does not reexport that
trait, record fields are private, and the frozen normal dependency list did not contain
`nudox-id`. Therefore the specified feature-off adapter had no safe callable route to frame
bytes `8..76` without a new dependency, shared API change, unsafe representation access, or
noncanonical substitute.

The manager independently reproduced this with a temporary feature-off nested package whose
normal dependencies were exactly `blake3`, `nudox-workflow`, and `thiserror`. Calling
`record.canonical_bytes()` produced E0599 and identified `FixedCanonicalRecord`; importing
that trait produced E0432 because `nudox_id` was not a direct dependency. No repository file
was changed by this reproduction.

The reviewer cleared arithmetic, five-event reduction, both new persistence falsifiers,
private receipt pairing, broad retained-log audit, feature projection, and tail priority. Its
full tripwire table is retained in `CONTROL_A_FRESH_PREEDIT_REVIEW.md`.

## Result and parent authority

This card is rejected for build authority. The parent then made exactly two authority
decisions for one replacement-card calibration: permit only
`nudox-id = { path = "../../crates/nudox-id" }` as an additional normal adapter dependency
solely to import `FixedCanonicalRecord` for `WorkflowRecord::canonical_bytes`, with a
minimal feature-off compile/golden proof and dependency/text charge; and define “one
exclusive `File`” as one retained journal descriptor, allowing one transient parent-directory
descriptor that is dropped before return. The replacement must report retained `1`, creation
peak `2`, and reopen/append peak `1`; retaining the directory descriptor fails. No shared
workflow/identity edit or other dependency is authorized.

## Fresh replacement-card calibration round 2 (rejected)

The replacement specimen was `CONTROL_A_CARD.md` at commit `ac15cb84`, SHA-256
`8a7072093f9fc4d31b870955b8b0ef42f4f381f9dd851eb53ff1c1a6e83cbf64`.

| Role | Task ID | Explicit model | Fork | Access |
|---|---|---|---|---|
| fresh reader and plausible misreader | `/root/p2_build_manager/control_a_luna_reader_r2` | `gpt-5.6-luna` | `none` | read-only; no edits or git writes |
| fresh blind reviewer | `/root/p2_build_manager/control_a_terra_preedit_r2` | `gpt-5.6-terra` | `none` | read-only; no edits or git writes |

The Luna cleared the direct `nudox-id` boundary: `WorkflowRecord` already implements
`FixedCanonicalRecord<68>`, so the one authorized direct dependency permits exactly the required
feature-off 68-byte copy without a shared API, reexport, unsafe block, or further dependency.
It correctly restated the new retained/peak descriptor quantities. Its critical results were that
the card needed to distinguish source candidate `be015582…` from its own card commit explicitly,
prove the descriptor figures structurally rather than through an unapproved public counter, and
source-audit the production outgoing copy rather than merely testing the trait in the oracle.

Its remaining plausible evasions were a 92-byte write-error truncation, retry after a terminal
write error, full-corruption repair, reduction-before-write drift, feature API leakage, and retained
event/record collection. Each must retain its exact existing falsifier. The independent Terra
findings and complete tripwire table are appended to the paired review record below.

This round is rejected. The parent then resolved the only interpretation question: the mandatory
compile-fail test is specifically a downstream attempted `AppendSuccess { receipt: ..., reduction:
... }` literal using facts from two successes; private fields/construction make that compile-fail.
Consumers may combine independent borrowed receipt and reduction facts in their own type, because
that type is not an `AppendSuccess` authority witness. The parent authorized exactly three
structural card repairs: binding test forecast `488`/reserve `72` only; exhaustive manual dynamic
container/alias/wrapper/collection-route audit; and lexical-scope proof of a transient directory
`File` with no retained field. No other surface or dependency authority changed.

## Final fresh calibration round 3 (clear)

The final specimen was `CONTROL_A_CARD.md` at commit `e67b6ec1`, SHA-256
`a9d97dd625dba2ad2ff068472b5f5cf1ccd03e21df993c06cac8f819ea15bb2b`.

| Role | Task ID | Explicit model | Fork | Access | Result |
|---|---|---|---|---|---|
| fresh reader and plausible misreader | `/root/p2_build_manager/control_a_luna_reader_r3` | `gpt-5.6-luna` | `none` | read-only; no edits or git writes | clear |
| fresh blind reviewer | `/root/p2_build_manager/control_a_terra_preedit_r3` | `gpt-5.6-terra` | `none` | read-only; no edits or git writes | clear |

The Luna independently restated every terminal, path, grammar, arithmetic boundary, failure order,
feature projection, dependency, retained/peak descriptor, copy/allocation, and cap law. Its
plausible shortcuts—card/source custody confusion, substitute encoding, field exposure, retained
collection aliases, early directory close, full-frame repair, source-erasing fault checks, and
shared `DurableAppend` smuggling—are each stopped by a literal card falsifier. It found no blocker
or major. Its source inspection confirmed the existing `FixedCanonicalRecord<68>` implementation,
direct streaming replay, `Publishing -> Publish` recovery, and stated arithmetic.

The Terra supplied a complete zero-blocker/zero-major tripwire table in the paired review record.
It specifically cleared the intentionally narrow direct `AppendSuccess` mixed-literal proof,
canonical-byte trait seam, descriptor lexical proof, retained-log audit, feature-on persistence
falsifiers, and all reserve arithmetic. It approved exactly one builder checkpoint. No production
source was edited before this committed clearance.
