# P2 control A: fresh blind pre-edit review round 1 (rejected)

## Specimen and independent reviewer proof

The reviewer examined `CONTROL_A_CARD.md` at `1b76e77c`, SHA-256
`64a346145b93db75897a50a70c48ca962b4e6e902359dcca45951a9c6d00b34d`.
It was `/root/p2_build_manager/control_a_terra_preedit`, explicitly
`gpt-5.6-terra` with `fork_turns=none`, read-only. It received no builder rationale.

## Findings

### BLOCKER A-1 — canonical outgoing bytes are unavailable within the frozen dependency boundary

**Location:** `CONTROL_A_CARD.md:76-77,95-109,121`;
`crates/nudox-workflow/src/durable.rs:56-60`; `crates/nudox-workflow/src/lib.rs:14-17`.

**Evidence:** The outgoing 68-byte record bytes are supplied only by the
`FixedCanonicalRecord::canonical_bytes` trait implementation. That trait is defined by
`nudox-id`, is not reexported by `nudox-workflow`, and was not one of the adapter’s permitted
normal dependencies. Private record fields provide no substitute.

**Violated law:** A required safe feature-off capability cannot be compiled with the card’s
literal dependency boundary.

**Consequence:** The builder would need an unapproved dependency, a shared API edit, unsafe
representation access, or a noncanonical encoding.

**Parent correction:** One replacement card may add only the direct path dependency on
`nudox-id` for this trait, plus a minimal feature-off compile/golden proof and ledger charge.
It must be recalibrated before a builder is considered.

## Literal tripwire inventory

Scanned set: `CONTROL_A_CARD.md:1-161` and direct workflow boundary files
`crates/nudox-workflow/src/{durable.rs,reduce.rs,recovery.rs,lib.rs}` and
`tests/durable_shared.rs`. Search classes: ABI/public items/reexports/conversions,
arithmetic/constants, dynamic owners/collections, panic/error propagation, test assertions,
unsafe/dependencies, and feature gates.

| Tripwire | Count | Exact locations | Disposition | Evidence or finding |
|---|---:|---|---|---|
| panic/unwrap/expect/unreachable | 2 | `CONTROL_A_CARD.md:156` | required scan text only | cleared |
| source-dropping conversion/map_err | 1 | `CONTROL_A_CARD.md:156` | required scan text only | cleared |
| lossy From/TryFrom/raw authority bypass | 3 | `CONTROL_A_CARD.md:88,113,135` | receipt conversion/raw construction prohibited | cleared |
| arithmetic sentinel/saturation/operand loss | 1 | `CONTROL_A_CARD.md:71` | exact operands and pre-I/O rule | cleared |
| dyn/Box/Vec/Arc/Rc | 5 | `CONTROL_A_CARD.md:10,88,137,156,157` | exclusions/audit text only | cleared |
| public tuple fields/positional tuples | 1 | `CONTROL_A_CARD.md:110` | declared `Healthy(Recovery)` view | cleared |
| unit/stateless namespace structs | 0 | `CONTROL_A_CARD.md:1-161` | literal ABI scan | cleared |
| public local traits/delegation | 0 | `CONTROL_A_CARD.md:92-115` | `DurableAppend` excluded | cleared |
| one-letter generic parameters | 0 | `CONTROL_A_CARD.md:1-161` | ABI scan | cleared |
| numeric discriminants/sentinels/offsets/capacities | 19 | `CONTROL_A_CARD.md:55-71,80,86,88,115,117,119,121,130,134,142` | named/falsified wire and budget values | cleared |
| test-only Option/discarded/success-only assertions | 0 | `CONTROL_A_CARD.md:117,127-140` | exact state/source/owner/image assertions | cleared |
| unsafe/SIMD/allocator/dependency additions | 3 | `CONTROL_A_CARD.md:10,121,140` | dependency boundary is incompatible with A-1 | A-1 |
| public item without consumer/falsifier | 0 | `CONTROL_A_CARD.md:95-115,127-140` | every item has a named falsifier | A-1 capability gap |

## Cleared suspicion and verdict

The strongest counterexample is an adapter using exactly the listed normal dependencies that
reaches `WorkflowRecord::from(event)` but cannot obtain bytes `8..76`. The reviewer otherwise
cleared the exact 32/92 arithmetic, reduction effect, directory/tail fault source and persistent
image tests, opaque mixed-pair rejection, retained-log scan, feature projection, and full-frame
versus tail handling. Platform durability, TOCTOU, hostile writers, and allocation/copy evidence
remain unverified until implementation.

**Rejected.** No builder authority follows from this review.

## Fresh blind pre-edit review round 2 (rejected)

The reviewer was `/root/p2_build_manager/control_a_terra_preedit_r2`, explicitly
`gpt-5.6-terra` with `fork_turns=none`, read-only, against `CONTROL_A_CARD.md` at
`ac15cb84` / SHA-256 `8a7072093f9fc4d31b870955b8b0ef42f4f381f9dd851eb53ff1c1a6e83cbf64`.

It reported a pairing blocker by treating separately borrowed receipt/reduction references as a
coherent authority witness. The parent rejected that stronger interpretation: the card’s required
fixture is the direct downstream `AppendSuccess` struct literal using a receipt from one success
and a reduction from another, and it must fail because the paired fields/constructor are private.
An arbitrary consumer-owned borrowed pair is not an `AppendSuccess` authority and is not forbidden.

The reviewer found three accepted corrections:

1. The test cap had contradictory prose: estimate `488` plus reserve `72` is binding under cap
   `560`, so no simultaneous `552` test-surface authorization can remain.
2. The dynamic retained-log audit needs every standard collection family, aliases/wrappers, and
   collection-building route over `WorkflowEvent`/`WorkflowRecord`, followed by manual review of
   every production hit; its seeded `VecDeque<WorkflowRecord>` and wrapper-held
   `BTreeMap<u64, WorkflowRecord>` must fail.
3. Descriptor peak/retention needs a structural proof: the journal owns only its `File`; the parent
   directory `File` is a lexical local enclosing directory sync and is dropped before return. A
   field/source audit must fail a retained directory field or early close before sync. No public
   descriptor counter is authorized.

The reviewer otherwise cleared frame arithmetic, the existing canonical-byte trait availability,
the five-event `Publish` effect, feature-on directory/tail source-and-byte-image falsifiers, and
the typed I/O source forms. Its tripwire recorded no proposed adapter source; baseline-only hits
were the source-preserving `map_err` sites in `durable.rs:209,213,237-239`, an existing test-only
`Vec` in `durable.rs:285`, and existing `DurableAppend` at `durable.rs:174` outside adapter scope.

## Final fresh blind pre-edit review round 3 (approved)

The final reviewer was `/root/p2_build_manager/control_a_terra_preedit_r3`, explicitly
`gpt-5.6-terra` with `fork_turns=none`, read-only, against `CONTROL_A_CARD.md` at
`e67b6ec1` / SHA-256 `a9d97dd625dba2ad2ff068472b5f5cf1ccd03e21df993c06cac8f819ea15bb2b`.

It found **no findings**. Its literal tripwire scan covered the card and
`crates/nudox-workflow/src/{durable.rs,reduce.rs,recovery.rs,lib.rs}` plus
`tests/durable_shared.rs`: no prospective panic/unwrap/expect/unreachable, source-dropping
conversion, raw authority bypass, arithmetic loss, dynamic owner, tuple field, namespace type,
local trait, one-letter generic, test-only discarded result, unsafe/SIMD, or unfalsified public
item. Baseline-only source-preserving `map_err` sites, canonical test `Vec`, and
`DurableAppend` were correctly outside the new adapter.

The reviewer cleared the narrow private `AppendSuccess` literal rule, the exact four normal and
two dev dependency boundary, 32/92 geometry and source-bearing errors, the complete write/torn
tail distinction, retained-log manual audit, lexical directory descriptor proof, feature delta,
and 488/72 binding test forecast. Its strongest attempted counterexample was the mixed literal
using receipt from one success and reduction from another; private fields/no constructor reject it.

UNVERIFIED remains implementation-dependent fault behavior, allocation/copy measurements,
feature-surface output, platform directory sync, TOCTOU, and cross-process writers. **Approved for
exactly one Luna builder checkpoint.**
