---
name: review-rust-gem
description: Perform a hostile, evidence-backed review of one workspace2 Rust slice before parent approval. Use after an implementation checkpoint or when auditing claimed correctness, simplicity, typing, allocation, concurrency, durability, diagnostics, testing, or performance. This skill reviews; it does not silently implement fixes.
---

# Review a Rust gem

Read `../../../ORCHESTRATION.md`, `../deliver-reviewed-rust-slice/SKILL.md` completely, then the one
applicable domain skill. The shared craft laws are the review standard. Do not invent a second style
guide.

The reviewer is the registered `nudox_terra_reviewer` beneath a distinct sidecar parent whose sole
`workspace-write` root is a disposable build directory. It reads a separate exported source snapshot
without Git history or builder rationale; that snapshot is never a writable root. Implicit `$TMPDIR`
and `/tmp` writes are disabled; compiler output, temporary files, and experiments stay under the build
root, and aggregate source digests must match before and after. It is
adversarial toward claims and collaborative toward the implementation. It receives raw artifacts without the builder's rationale or the primary Terra's
suspected answer. It never implements a repair, changes the contract, or owns acceptance. Find the
strongest simple design that actually proves the contract. Green tests and clever types are inputs,
not approval.

The dispatch-only sidecar parent is `gpt-5.6-sol`/`low` and spawns this reviewer with
`fork_turns = "none"`. A full-history fork inherits the parent role; a Luna parent may be accepted for
its own turn while being absent from the child router. Custody exists only when the runtime returns a
nonempty reviewer task ID and resolves this registered role as `gpt-5.6-terra`/`xhigh`. An empty
receiver list or parent narration that a reviewer is running is a blocker, not partial evidence.

## Required inputs

Require: capability index, approved contract card/digest, `TESTING.md` mapping, snapshot and review
packet digests, runtime role receipt, direct consumers, claimed ledgers, commands/results, disposable
build path, and applicable baseline evidence. If any is absent, report the
missing proof; do not infer success.

Freeze scope before reading details: record changed paths, generated/manifest changes, changed
invariant/dependency owners, deleted APIs, and unrelated dirty files. Review is read-only; reproduce
hypotheses only in the disposable external directory. Check wildcard workspace members for orphan/incomplete crate directories before running
gates. Never repair code while compiling the findings; that hides causal evidence.

Before interpretation, run the project Dylint/Clippy suite and a mechanical tripwire inventory over
changed production and test paths:
panic/`unwrap`/`expect`/`unreachable!`, source-dropping `map_err`, `dyn`/`Box`/`Vec`/`Arc`, public tuple
fields, unit/stateless structs, public local traits, one-letter generic parameters, numeric
discriminants/matches, lossy or dual-meaning `From`/`TryFrom`, raw authority reconstruction, and
test-only `Option`/discarded results. Account for every hit as required,
cold-only, or a finding; an empty prose claim is not a scan. For each public marker-to-associated-type
trait, try deleting the wrapper marker and using the already branded associated/domain type directly.
If the mapping adds no behavior or invalid-state exclusion, it is duplicate representation.

At the pre-edit checkpoint, apply the same inventory to every literal ABI declaration and normally
formatted skeleton in the card. Proposed tuple fields, trivial getters/callback delegation, raw
semantic counters, sentinel arithmetic, broad lint allowances, or unplanned public items are findings
before they become source; “there is no changed Rust file yet” does not make those rows zero.

The final review must include this literal table with every row present, including zero-hit rows:

```text
tripwire | count | exact locations | disposition | evidence or finding ID
panic/unwrap/expect/unreachable
source-dropping conversion or map_err
lossy/ambiguous From/TryFrom or raw authority bypass
checked-arithmetic sentinel/saturation or operand loss
dyn/Box/Vec/Arc/Rc
public tuple fields or positional semantic tuples
unit/stateless namespace structs
public local traits or one-implementation delegation
one-letter generic parameters
numeric discriminants/sentinels/offsets/capacities/loop bounds
test-only Option/discarded results/success-only assertions
unsafe/SIMD/allocator/dependency additions
public item without current consumer and falsifier
```

Compiler-backed semantic lints are authoritative for patterns they cover. Text search may inventory
tokens and dependencies, but it cannot establish whether a Rust construct violates a semantic law.
For every changed semantic lint, mutate its specimen through a local macro, a type alias, renamed
unused bindings, and an unrelated same-named method where those attacks apply. Verify every workspace
in the declared shipping inventory receives its tests, strict Clippy variants, docs, dependency
resolution, and semantic policy gates; a central list used by only one command is not gate custody.
Dylint UI suites must resolve their fixture root from `CARGO_MANIFEST_DIR` and deny every subject lint
explicitly. Prove fixture custody once by corrupting one expected diagnostic or source specimen and
observing the suite fail. The test must also reject an empty inventory and an orphan source or golden;
an absolute path alone does not make zero discovery impossible. A green runner that silently discovers
zero intended fixtures is a blocker.
Do not summarize the scan as “accounted for.” A missing row, missing location, or `acceptable` without
a contract law or measurement makes the review incomplete and forbids approval. The primary Terra
must preserve the raw reviewer table; a paraphrased manager ledger is not independent evidence.
“Exact location” means a complete repository-relative path plus line, never `.../`, a bare filename,
or “all changed files.” A zero row names the complete path set actually scanned and the literal search
classes; an em dash alone does not prove the scan.

Compare the candidate with the frozen coupling skeleton. An unplanned invariant owner, dependency
direction, public item, resource lifetime, state axis, or error vocabulary is a scope finding. Search
for public errors, types, dependencies, and reexports that have no current consumer and falsifying
test; delete them rather than calling them preparation for the next phase. Do not penalize coherent
parameter or generic lists merely for their size; find the actual coupling or unearned dimension.

For a large rejected block, review deletion as rigorously as addition. Require a salvage ledger naming
every real mechanism, its current proof, and its destination. Reject a cleanup that removes a proven
property merely to meet a size target, and reject a “salvage” that keeps an unnecessary compatibility
surface instead of moving the property into its strongest owner. The acceptable outcome is smaller
code with the same or stronger falsifiable properties, not fewer lines by forgotten behavior.

Audit the ledger at symbol granularity: old owner, new owner, old falsifier replay, and a mutation that
kills the new proof. Sample private algorithms and test oracles as aggressively as public types; useful
engineering is not limited to API surface. Report whole-block deletion as a finding unless every row is
absorbed or an executable counterexample proves it should not exist.

## Review passes

Run all applicable passes in this order:

1. **Contract:** map every preserve/prove row to code and a falsifying test. Reject deferred rows or
   substituted capabilities.
2. **Boundary:** identify the invariant owner, dependency direction, leaked adapter policy, duplicate
   representation, compatibility shim, and public surface that can be private/deleted. For each
   witness/view with multiple fields, try a downstream struct literal that combines parts from two
   valid owners. Compilation is a blocker when coherence is required.
3. **Less-is-more:** find wrappers, getters, delegate traits, stateless structs, helper proliferation,
   parallel algorithms, repeated fields, redundant wire metadata, and branches that representation
   can remove. Reject `rustfmt::skip`, statement packing, forwarding helpers, callbacks, or parameter
   bags used to manipulate a shape/complexity metric instead of improving the ownership model.
4. **Types:** inspect raw primitives, tuple coordinates, optional correlated state, string stages,
   catch-all variants, generic ledgers, lifetime truth, conversions, field visibility, and invalid
   states. Demand descriptive generic names. Cross every public method with every reachable phase;
   each cell must have a truthful result and complete owner/credit transition.
5. **Ownership/layout:** account for owner plus backing, peak live memory, pointer depth, allocation,
   copies, stable-address need, rejection/drop, `Arc` churn, stack/thread-stack cost, fragmentation,
   and text-size monomorphs. Compare the real lifetime alternatives.
6. **Control/errors:** trace hot and failure paths. Find repeated conditions, unpredictable branches,
   hidden scans, oversized functions, lost sources/values, panic paths, hand-written formatting, and
   error priority drift. Trace every validated tag, offset, and length into projection. A second
   raw-to-closed decode, `unreachable!`, `expect`, silent omission, fallback, or unproved narrowing
   conversion means the representation did not retain its proof. Inspect every helper reachable from
   a closed-enum arm: if it accepts a coordinate that is valid only for that arm but can also observe
   other arms, require the caller to pass an arm-specific borrow/witness instead. Treat fabricated
   data, defaults, empty results, or internal errors in an impossible arm as a blocker, even when the
   branch is currently unreachable.
7. **Concurrency/async:** prove receiver-level concurrency, physical bounds, linearization, ordering,
   waker arm/recheck, cancellation, ABA/reuse, poison, shutdown, and progress. Search for locks hidden
   in libraries. Require Loom on production transitions and Miri for unsafe ownership.
8. **Protocol/durability:** recompute every byte and preimage. Mutate every cell. Trace write/sync/
   directory/receipt/crash/replay semantics and partial operations. Reject duplicated metadata and
   undefined authentication. When authority/version cells occupy digest bytes, verify routing and
   partition words draw from actual digest entropy rather than the fixed prefix. For every closed
   registry, introduce duplicate numeric domain and encoding codes: rustc must reject both by
   construction. Sealing alone and runtime uniqueness tests are insufficient. For every generic
   validated view, prove writer -> validation -> infallible projection for a second real type or
   require removal of the unearned generic.
   Count full passes over every validated region on successful input. A typed cast followed by an
   equivalent semantic scan is duplicate work unless the second pass proves a distinct invariant.
   Exact cell diagnostics may rescan only after the typed cast rejects. Exercise every private direct
   writer through the public validator and require exact typed failure rather than panic or erasure.
   Inventory authority/version cells repeated in fixed records. When the container requires one
   value for all records, require a comparison against one header cell plus a typed payload; measure
   bytes and deleted checks. Conversely, reject hoisting when a record must remain self-authenticating
   outside the container.
   Trace public conversions as chains, not isolated methods. A checked full-width decoder is still
   bypassed when `raw payload -> typed payload -> typed identity` injects the omitted authority from a
   generic parameter. For hoisted authority, require observed cell -> checked proof -> payload bind,
   then compile-fail a proof/bind from one real domain into a second. A zero-sized proof is legitimate
   only when its constructor checks the runtime cell and its type fixes every later authority use.
9. **Diagnostics:** ask the promised operator questions using only emitted typed events. Prove no-op
   laziness, exact chronology/correlation, bounded retention/export, triggered dump, and core/client
   exclusion from server machinery.
10. **Tests/evidence:** try to make tests pass with the implementation broken. Demand exact negative
    assertions, boundary tables, public integration, isolated allocation measurement, raw performance
    evidence, feature/target coverage, and source-bearing fault injection. For a claimed reproducible
    offline toolchain, run the complete gate with empty external Cargo and generated-driver caches
    after only entering the pinned environment. A lockfile plus `--offline` on a warm machine is not
    closure; every Git source and generated compiler driver must come from the pinned closure or a
    repository-owned vendor source.

For a synthetic dispatcher, adapter, or frontend, delete input forwarding or replace the concrete body
with a constant. If public tests remain green or optimized code no longer consumes the input, the proof
is nominal and therefore a blocker. Text/codegen evidence must name a consumer executable or callable
artifact and a retained control; an `.rlib` metadata total cannot prove monomorphized work. Run the
advertised clean gate and inspect repository status immediately—generated lockfiles or artifacts
deleted afterward falsify the clean-gate claim.

## Finding format

Every finding contains:

```text
Severity: BLOCKER | MAJOR | MINOR | QUESTION
Location: exact file and line
Evidence: code/test/measurement observed
Violated law: contract or shared-skill statement
Consequence: correctness, safety, ownership, work, memory, API, or operability
Smallest correction: deletion or redesign boundary, not a patch recipe by default
Falsifier: exact test/measurement that proves the correction
```

`BLOCKER` means violated semantics, unsafe/durability uncertainty, lost errors/owners, unbounded state,
or a missing contract capability. `MAJOR` means unjustified allocation/generic/dependency, material hot
work, abstraction leakage, or weak integration evidence. `MINOR` must still name an actual cost; pure
preference is omitted. Questions never disguise findings.

## Approval rules

Do not approve when:

- any contract row lacks a public falsifying test;
- code calls a weaker behavior by the requested name;
- an allocation/generic/unsafe/SIMD/dependency ledger is incomplete;
- concurrency is modeled in analogous test code rather than production transitions;
- tests assert only success/no panic or use `expect`/`unwrap` as reporting;
- a runtime test is offered as proof that an unwanted constructor/trait/transition cannot compile;
- a simplicity claim depends on suppressed formatting, compressed logical statements, forwarding
  helpers, or hidden parameter-object coupling;
- diagnostics exist only in an outer demo;
- a coherence-dependent public struct literal can combine unrelated valid fields, or validated input
  still reaches a panic/fallback/redecode during trusted projection;
- retained complexity rises without a measured/deleted cost;
- the implementation diverged from its coupling skeleton or introduced future-phase public surface
  without current behavior and tests;
- full gates are red for an owned finding.

Approval requires zero blockers/majors, reproducible commands, honest remaining caps, and a smaller
next decision. “Approve with comments” is not final approval; it is another checkpoint.

## Reviewer self-check

Before handoff, state:

- the strongest attempted counterexample;
- the most likely hidden allocation/branch/lifetime;
- one thing that looked suspicious but evidence cleared;
- whether a simpler standard-library design was genuinely considered;
- confidence and unverified platform/tooling gaps.

Return findings first, ordered by severity, with a short approval status last. Do not write a score
unless an independently validated rubric was explicitly supplied.
