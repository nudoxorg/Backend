# Card C5 — driver lowering repairs from live corpus evidence (registered role: nudox_luna_implementer)

## Context
The corpus card (commits 6cd03f6ad, ff1cfcbb) ran twenty REAL nuget artifacts
through the full journey. Thirteen rows complete end to end. The reds are
YOUR scope — driver lowering defects, each pinned by live evidence:

R1 PANIC at compiler/driver/lower.rs:1575/1595 — the FunctionPointer
   reconstruction eagerly indexes `children[0]` in the
   `[TupleElement{ty: children[0], ..}; MAX_TYPE_CHILDREN]` initializer and
   `children[child_count - 1]` underflows when a record claims RESULT_FLAG
   with zero children. A zero-parameter void function pointer
   (`delegate*<void>`) is LEGAL C# and panics the lane. esp-net-source
   0.6.4/0.2.3 (Router.cs) repro it live.
   Repair lawfully: no eager `children[0]`; build the element slice from
   the actual child count; `has_result && child_count == 0` is an invariant
   violation -> typed `BuildError` (Dangling or a new precisely-named arm
   you add with operands), never a panic; `child_count == 0 && !has_result`
   is a legal zero-arity function pointer.
R2 Evidence-erasing fold at compiler/driver/lower/csharp.rs `terminal()` /
   `lane_terminal()`: every ProjectionFault (Image, Depth, NameSpan,
   OwnerOrder, Foreign, AttributeCapacity{spellings}, IndexCapacity)
   collapses to `Lowering(NoSupportedDeclaration)` via `let _ = fault`.
   The corpus proof stalled exactly here: the committed fidelity fixture
   fails collect with NoSupportedDeclaration and nobody can say which fault
   it is. Repair: add a `Projection(ProjectionFault)` variant to
   `CSharpCollectError` retaining the full typed fault (the enum and the
   fault type live in this module), route every fold site through it, and
   extend the compile.rs match arm for the C# collect error so the exact
   fault reaches CompileFailure (one new mapping arm; keep every other
   lane's arms untouched). Delete the `#[expect(dead_code)]` on the
   Image operand — it is now consumed. Update the module doc comment that
   promised the criticism would be recorded: the criticism is now fixed.
R3 With R2 in place, diagnose the fidelity fixture's collect failure,
   fix the TRUE defect (producer-side: report to the manager; consumer-side
   in your paths: fix), and land the corpus card's conformance test
   (committed fidelity fixture collects + admits + validates; operator
   name b"+", constructor name b"Widget", delegate parameter carriers,
   interface-implementation occurrence survives).
R4 Occurrence scratch bound: compiler/driver/lower.rs:48
   MAX_EMISSION_OCCURRENCES = 1024 rejects real single-file artifacts
   (tinyioc 1.4.0-rc1: 1253 resolved references; tinyioc 1.3.0: 1170).
   The manager authorizes raising this DRIVER-INTERNAL scratch bound to
   8192 (cost: boxed scratch ~230 KiB per compile, measured and recorded
   in the module comment beside the constant). The fact geometry 1024 is
   trunk-owned and must NOT move.
R5 Attribute-list bound: nullable 1.3.1 rejects with 23 attribute
   spellings against MAX_REF_LIST_ELEMENTS = 16 for ONE declaration row.
   Diagnose with R2's typed operands: is one real declaration carrying 23
   attributes (then the typed terminal is honest — document it in the
   corpus table), or is the producer mis-owning attribute rows (then
   report the producer defect to the manager with the exact row/spelling
   evidence; do NOT edit helper/** yourself).

## Owned paths (nothing else)
- compiler/driver/lower.rs (panic fix, occurrence bound + comment)
- compiler/driver/lower/csharp.rs (error unfold, fixture diagnosis,
  conformance test additions if owned here rather than in csharp_corpus.rs)
- compiler/driver/types/compile.rs (ONLY the CSharp collect error mapping)
- compiler/driver/tests/csharp_corpus.rs (ONLY: flip rows 9/10/11/12 to
  full journeys and row 1/6/20 verdicts after the fixes; do not restructure
  the support module)

## Gates (run all, report tails)
export CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-csharp/.local/target-csharp
export PATH=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/dotnet-sdk:$PATH
export DOTNET_ROOT=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/dotnet-sdk
export COMPILER_CSHARP_COMPILER=$DOTNET_ROOT/dotnet
- cargo test -p compiler-driver --offline --lib lower::csharp  (19 green)
- cargo test -p compiler-driver --offline --test csharp_corpus  (see below)
- cargo test -p compiler-driver --offline --test csharp_image --test native_compile  (green)
- cargo test -p compiler-languages-csharp --offline  (green, untouched)

Corpus success bar: rows 2-5, 7-19 full journeys green (17 rows), rows
9-12 INCLUDED; rows 1/6/20 either full journeys green or exact typed
capacity terminals with both operands recorded in the row verdict — the
corpus table documents every terminal. 25+5 must become >= 25 green with
0 failed or the remaining failures precisely diagnosed as R5-honest
terminals.

## Bounds
No new dependencies, no unsafe, no semantic redesign of the lowering (the
zero-arity funcptr is a legality fix, not a representation change). The
occurrence bound change is authorized; the fact geometry is not yours.

## Checkpoint & return
Commits prefixed fix(driver):. Return: commit hash(es); the R1-R5 evidence
table (repro -> fix -> gate tail); the fidelity fixture's diagnosed true
cause; the R5 verdict (honest terminal vs producer defect, with operands);
exact commands + tails; smallest remaining red row.
