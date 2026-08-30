# Hostile pre-edit review — durable-publication-wave-a2

## Verdict: BLOCK

This is a source-isolated **pre-edit** verdict. The exported tree is the D0
baseline, not a publication candidate; its lack of `DurablePublisher` is
expected at this checkpoint and is not counted as an implementation defect.
The pre-edit packet nevertheless cannot authorize a production edit: required
proof/custody inputs are absent, the proposed public authority has an
unresolved mixed-owner construction attack, and the required pinned gates do
not reproduce from a clean offline Cargo home.

## Ranked findings

### F-01 — BLOCKER: the proof matrix, capability index, prior raw review, and fault/resource falsifiers are absent

**Location:** `pre-edit-2.md:34-39`, `pre-edit-2.md:43-48`,
`pre-edit-2.md:52-57`, and
`workspace2/adapters/durable-journal/tests/wave_a2_red.rs:91-129`.

**Evidence:** The packet requires separate exact bounds for item, byte,
waiter, receipt, and group; every listed cancellation, receiver-loss, poison,
join, physical write/sync/head/CAS-or-rename, and crash prefix needs an
independent-reducer falsifier. It also says the packet digest, previous review,
direct-consumer ledger, and every pinned gate are bound in `index.toml` and
`reviews/pre-edit-1.md`. The sealed source has no
`.codex/evidence/capabilities/**`, no `index.toml`, no proof matrix, no frozen
brief, and no prior raw review. The supplied direct journey exercises only one
happy submission/reopen path; it provides no fault/cancellation/bounds/resource
oracle. It does not state the fixed group or per-submission retained/live bytes,
allocations, copies, or work delta demanded at packet line 45.

**Violated law:** every contract row needs an owner and falsifier before edit;
missing Sol refinement or resource proof is a finding, not a green assumption.

**Consequence:** an implementation could make the Sol journey pass while
dropping a queued item on cancellation, returning capacity twice, publishing a
head before bytes are durable, or allocating an unbounded waiter/receipt queue.
There is no frozen test to kill those mutants.

**Smallest correction:** supply one frozen capability directory containing the
approved contract digest, coupling skeleton, direct-consumer/resource ledgers,
and one exact test/measurement per law. In particular, bind the capacity
model, all pending cancellation boundaries, every physical fault/crash prefix,
duplicate/conflict, receiver loss, poison, shutdown/join source priority, head
validation, and group/per-submission resource delta before production work.

**Falsifier:** mutate each planned implementation at one boundary (drop queued
payload, move head publication before durability, reuse a completion credit,
erase the write source, or exceed each independent capacity) and require the
named focused test/measurement to fail with the exact typed result and unchanged
accounting.

### F-02 — MAJOR: the literal public `PublishedGeneration` contract does not yet make mixed-owner construction unrepresentable

**Location:** `pre-edit-2.md:24-29`, `pre-edit-2.md:39`,
`workspace2/adapters/durable-journal/tests/wave_a2_red.rs:93-125`, and
`workspace2/adapters/durable-journal/src/lib.rs:64-106`.

**Evidence:** The packet requires readable `PublishedGeneration` fields
`pinned_root`, `dep_set`, and `publication`, and a public
`PublicationFacts.stable: ReceiptFacts`; it also requires unforgeability and
that a mixed-owner construction fails. A normal public struct with those fields
admits this downstream counterexample: retain a genuine stable publication fact
from generation A, combine it with the root/dependency facts of B, and use a
struct literal. `StableReceipt` is non-forgeable in D0, but its readable facts
are public; that does not prevent reuse of a genuine fact in a different public
record. The packet says construction is private but supplies no required
representation (`#[non_exhaustive]`, private seal, or equivalent) and no
downstream compile-fail falsifier.

**Violated law:** a published authority is unforgeable and construction may not
depend on caller coherence.

**Consequence:** a caller could fabricate an apparently published B that never
passed the immutable-publication/head validation associated with B.

**Smallest correction:** freeze a representation that retains the three
readable facts while forbidding downstream literals—for example a
`#[non_exhaustive]` published record with only adapter-controlled construction—
and bind construction to one consumed `VerifiedGeneration` plus the stable
publication/head facts.

**Falsifier:** an external compile-fail fixture attempts a `PublishedGeneration`
literal combining root/dependency inputs from B with A's publication facts; a
separate reopen mutation substitutes B's head or bytes under A's publication
facts and must report the exact typed validation error.

### F-03 — BLOCKER: the required clean offline gate and semantic-lint gate cannot run from the pinned environment

**Location:** `pre-edit-2.md:17-18`, `pre-edit-2.md:46-48`, and
`workspace2/flake.nix:1-78`.

**Evidence:** With a clean `CARGO_HOME` and all Cargo target/temp output under
the disposable build root, Nix supplied the pinned `cargo 1.97.1` and
`rustc 1.97.1`. The exact red command then stopped before compilation:
`error: no matching package named 'blake3' found`, while offline. The Dylint
suite likewise stopped before any UI or shipping lint ran because the pinned
`rust-clippy` Git revision for `clippy_utils` was not available offline. Thus
the red journey did not reach its expected missing-publication API diagnostic,
and no Clippy/Dylint result exists.

**Violated law:** advertised offline pinned gates must be reproducible from an
empty external Cargo/generated-driver cache after entering the pinned
environment.

**Consequence:** a warm local cache can manufacture green evidence; this review
cannot validate formatting, tests, Clippy, Dylint, docs, dependency resolution,
or source-level semantic policy reproducibly.

**Smallest correction:** make the Cargo registry sources and the Dylint
compiler-driver Git source available through the pinned Nix closure or a
repository-owned vendor source, then rerun each exact command from an empty
external Cargo/generated-driver cache.

**Falsifier:** repeat the exact red, quality, and Dylint commands with fresh
`CARGO_HOME`, `RUSTUP_HOME`, `CARGO_TARGET_DIR`, and `TMPDIR` beneath the build
root; deleting either required pinned source must make the appropriate command
fail rather than silently use a host cache.

### F-04 — MINOR: the red test's command documentation contradicts the frozen terminal and the file is not rustfmt-clean

**Location:** `workspace2/adapters/durable-journal/tests/wave_a2_red.rs:3`,
`workspace2/adapters/durable-journal/tests/wave_a2_red.rs:59`, and
`pre-edit-2.md:17-19`.

**Evidence:** The test comment says to run an ignored test, but the test has no
`#[ignore]` and the frozen command explicitly has no `-- --ignored`. Independently,
the pinned `cargo fmt --check` reports a formatting diff at the long error
attribute on line 59.

**Violated law:** normally formatted skeletons and exact, reproducible terminal
commands are part of pre-edit evidence.

**Consequence:** the current direct journey cannot pass the advertised quality
gate and a maintainer following its comment uses the wrong invocation model.

**Smallest correction:** format the Sol-owned test and make its run instruction
match the frozen terminal.

**Falsifier:** run the exact `cargo fmt --manifest-path
adapters/durable-journal/Cargo.toml --all -- --check` in the pinned environment.

## Strongest attempted counterexample

Take a genuine published fact for generation A. Obtain a separately verified
generation B. If the required readable fields are implemented as an ordinary
public struct, construct `PublishedGeneration { pinned_root: B.root, dep_set:
B.dep_set, publication: A.publication }`. This has individually valid facts but
never proves that B's immutable publication and head are stable. The packet
requires this to fail, but provides neither an unconstructable representation
nor a compile-fail test. This is the strongest design attack because a happy
reopen test cannot distinguish a private construction boundary from a public
literal.

## Cleared suspicion

The inherited D0 `FileJournal` is not falsely presented as the future MPSC
publisher. It keeps the file/state/sequence/poison ownership in one
single-owner value (`src/journal.rs:19-24`) and requires `&mut self` for append
(`src/journal.rs:58-60`). Its D0 closure expressly excludes MPSC admission and
group commit (`DURABLE_JOURNAL_D0_CLOSURE.md:3-5,28,45-49`). The source scan
found no production `panic!`, `unwrap`, `expect`, `unreachable!`, `unsafe`,
`Arc`, `Box`, `Rc`, or dynamic dispatch in the pre-edit scope; the existing
`map_err` sites retain typed sources/operands.

## Simplest standard-library control considered

Use one owner thread with `std::sync::mpsc::sync_channel` and `try_send` for
bounded producer admission, retaining `FileJournal` and probes solely in that
thread; attach a one-shot standard channel response to each pending submission.
This is a useful control, not an accepted design: it must still prove the
separate item/byte/waiter/receipt/group bounds, per-submission allocation and
drop behavior, cancellation, poison, and head durability. No such comparison or
measurement was supplied, so the control cannot close F-01.

## Literal tripwire table

Scope: the complete A.2 literal ABI in `pre-edit-2.md:20-29` and the Sol-owned
pre-edit skeleton `workspace2/adapters/durable-journal/tests/wave_a2_red.rs:1-129`.
There is no A.2 production source yet; inherited D0 source was additionally
scanned for panic/error/allocation constructs as recorded above, but its legacy
hits are not attributed to this future edit.

| tripwire | count | exact locations | disposition | evidence or finding ID |
| --- | ---: | --- | --- | --- |
| panic/unwrap/expect/unreachable | 0 | `tests/wave_a2_red.rs` (literal token classes `panic!`, `unwrap`, `expect`, `unreachable!`) | no hit | scan complete |
| source-dropping conversion or map_err | 1 | `tests/wave_a2_red.rs:88` | `JourneyError::from` retains the typed verification error; not source-dropping | cleared |
| lossy/ambiguous From/TryFrom or raw authority bypass | 0 | `pre-edit-2.md:20-29`; `tests/wave_a2_red.rs` (new conversion declarations) | no A.2 conversion is declared; authority representation remains unspecified | F-02 |
| checked-arithmetic sentinel/saturation or operand loss | 0 | `tests/wave_a2_red.rs` (checked/saturating/wrapping/overflowing declarations) | no hit | scan complete |
| dyn/Box/Vec/Arc/Rc | 2 | `tests/wave_a2_red.rs:70,77` | fixture `vec!` allocations create the real root/locality input; no candidate resource ledger exists | F-01 |
| public tuple fields or positional semantic tuples | 0 | `pre-edit-2.md:20-29`; `tests/wave_a2_red.rs:1-129` | no tuple field/type declaration | scan complete |
| unit/stateless namespace structs | 0 | `pre-edit-2.md:20-29`; `tests/wave_a2_red.rs:1-129` | no declaration | scan complete |
| public local traits or one-implementation delegation | 0 | `pre-edit-2.md:20-29`; `tests/wave_a2_red.rs:1-129` | no declaration | scan complete |
| one-letter generic parameters | 0 | `pre-edit-2.md:20-29`; `tests/wave_a2_red.rs:1-129` | no declaration | scan complete |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 10 | `tests/wave_a2_red.rs:65,66,68,71,75,77,102,103,111` | fixture values plus two capacity values; no zero/one/full/+1 bound matrix or resource accounting | F-01 |
| test-only Option/discarded results/success-only assertions | 1 | `tests/wave_a2_red.rs:91-129` | one happy public journey with facts/reopen checks, but no negative/fault/cancellation falsifier | F-01 |
| unsafe/SIMD/allocator/dependency additions | 0 | `pre-edit-2.md:8-13`; `adapters/durable-journal/Cargo.toml:1-22` | no candidate addition; existing D0 dependencies are baseline | scan complete |
| public item without current consumer and falsifier | 6 | `pre-edit-2.md:20-25`; consumer references `tests/wave_a2_red.rs:11-14,101-126` | all six named surfaces have the Sol consumer, but no negative construction/fault/resource falsifier | F-01, F-02 |

Supplementary source inventory: D0 has 24 `map_err` sites at
`src/journal.rs:42,43,47,70,87,88,107,138,143,155,160,165,167,172,177,181,198,199,210,217,222,224,259`
and `tests/wave_a2_red.rs:88`; each retains an explicit source or uses a typed
`From` conversion. The two D0 scalar `From` implementations are
`src/lib.rs:37-40,58-61`; they preserve all bits and do not reconstitute an
authority. They are baseline controls, not A.2 additions.

## Exact commands and results

All compiler experiments used a read-only source snapshot copied only into the
disposable build root; `TMPDIR`, `CARGO_HOME`, and `CARGO_TARGET_DIR` were under
that root.

1. Aggregate source receipt, before review:

   `find /tmp/nudox-a2-fresh-review-source.MnmslM/source -type f -print0 | sort -z | xargs -0 shasum -a 256 | shasum -a 256`

   Result: `971798e696325e45ac7e070a83fec15cb4df78ef11ebf2a763684d68cbc57e9a`.

2. Exact feature gate in Nix (from the copied build tree):

   `nix develop <build>/compile-red#quality -c bash -c 'export PATH="$NUDOX_STABLE_TOOLCHAIN/bin:$PATH"; cargo test --manifest-path adapters/durable-journal/Cargo.toml --locked --offline --features wave-a2-publication-red --test wave_a2_red'`

   Result: pinned `cargo 1.97.1` / `rustc 1.97.1` resolved; Cargo then failed
   offline before compilation: `no matching package named blake3 found`.

3. Pinned formatting/Clippy attempt:

   `cargo fmt --manifest-path adapters/durable-journal/Cargo.toml --all -- --check`

   Result: failed with a rustfmt diff at `tests/wave_a2_red.rs:59`; the following
   Clippy command was not run because formatting exited nonzero.

4. Pinned Dylint suite in a separate copied build tree:

   `nix develop <build>/compile-dylint#quality -c env NUDOX_DYLINT_NIX_ENV=1 <build>/compile-dylint/tools/dylint/run.sh`

   Result: failed before semantic tests because offline Cargo could not checkout
   the pinned `rust-clippy` Git revision needed for `clippy_utils`.

5. Aggregate source receipt, after review: the same command as (1) returned
   `971798e696325e45ac7e070a83fec15cb4df78ef11ebf2a763684d68cbc57e9a`.
   No `target` directory was created beneath the source snapshot.

Packet SHA-256 observed: `d48564edfbcb3379c53e13f6108d2b91a4b007580f0bed966379db41fcb124da`.

## Effective role, custody, and tooling facts

- Registered configuration read:
  `workspace2/.codex/agents/nudox-terra-reviewer.toml`; it specifies
  `nudox_terra_reviewer`, `gpt-5.6-terra`, `xhigh`, and `read-only`.
- Dispatch task: `/root/nudox_sol_review_dispatch/nudox_terra_reviewer`.
  A separate runtime capability-index receipt was not supplied, so the config
  proves the required registration but not an independently recorded runtime
  resolution.
- Read-only source:
  `/tmp/nudox-a2-fresh-review-source.MnmslM/source` (the effective process could
  not write it). Sole output root:
  `/tmp/nudox-a2-fresh-review-build.ffU0f7`.
- The effective process could write ambient `/tmp` and began with an ambient
  `TMPDIR`; this does not meet the requested platform-level denial of implicit
  `/tmp` writes. I redirected all compiler/temp output manually beneath the
  build root, but the missing denial remains a custody/tooling gap.
- Nix evaluation emitted non-fatal busy SQLite eval-cache notices while two
  disposable builds ran. This did not alter the source receipt.

## Reviewer self-check

Confidence: high that the pre-edit evidence is insufficient and the source was
unchanged; medium on platform reproducibility because the clean-cache failure
prevents compilation of the intended red diagnostic. The most likely hidden
cost remains per-pending reply/waiter ownership and group buffer lifetime; no
resource ledger exists to bound it. The standard-library control above was
considered but has no evidence advantage yet.

**APPROVE or BLOCK: BLOCK.**
