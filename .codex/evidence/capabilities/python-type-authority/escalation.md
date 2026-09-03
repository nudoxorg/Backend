# Escalation to Sol — three decisions (2026-09-02)

D1 CROSS-LANE TRUNK DEFECT. Fragment prepare rejects python modules where a structural
class (TypedDict/Protocol) with members follows any earlier pushed fact:
`TypeFacts::ForwardReference { ordinal, position: 0, target: ordinal+1 }`. Minimal
counterexamples in compiler/driver/tests/python_render.rs (FORWARD_REFERENCE_SOURCE;
two ignore-annotated regression tests). Python emission appends strictly backward anon
coordinates [128,129,130] (verified by instrumented runs); the forward coordinate appears
during the trunk's lane construction in compiler/driver/lower.rs (owned by the adjacent
trunk lane). Required decision: trunk repair ownership + fix.

D2 LANE CAPACITY vs REAL PACKAGES. MAX_EMISSION_FACTS=128 (lower.rs, shared trunk) cannot
host real modules: six 1.17.0 six.py ~164 declaration facts fails with the honest
LoweringUnsupported terminal after the full PURL fetch/unpack/workspace chain passes.
Required decision: raise the frozen capacity (fragment geometry + memory bounds) or define
the product's module-splitting policy. Blocks the lifecycle and real-package matrix.

D3 RENDER SURFACING FORK. The live compile_ir Ir surfaces only records the trunk's
builtin_type() lifts (str, bool, widened literals); python int (arch-signed record) does
NOT lift (no `: i32`), compound types (list/dict/callable/unions) render no type, function
signatures render no tails, docs/embedding displays are empty — all of these live only in
the durable fragment planes. Goldens freeze the honest current output. Required decision:
extend trunk lifting so render displays surface python compound types/docs, or accept
fragment-plane-only evidence.
