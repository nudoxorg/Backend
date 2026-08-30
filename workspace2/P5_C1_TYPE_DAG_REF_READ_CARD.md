# P5 C1 recursive type-reference read card

## Terminal

Validate a separate padding-free `0xc2` table containing only primitive and reference records once
into caller `TypeNode` scratch, then lend an exact fused cursor. Prove legal self, forward, and backward
references. The `0xc1` fragment is unchanged. Products, preparation, and writing are later cards.

Control bytes: header `[0xc2, 1, node_count 0..=2, body_bytes]`; primitive `[0, code]` with Bool 0 and
I32 1; reference `[1, target_u8]`. Dense ordinal is `TypeId`. Target must be below node count, but may
equal, precede, or follow the source ordinal.

Baseline is clean `78bb3b83`. Write only format `src/lib.rs`, new `src/type_dag.rs`, new
`tests/type_dag.rs`, the actual-rlib addition in `tests/fragment.rs`, and
`evidence/p5-c1/type-dag-ref-read/**`. No manifest, lockfile, vocabulary, dependency, fragment,
writer, product, atom/list/external-ref, compiler, scheduler, or shared checkout edit.

## Surface and proof

Public: four `TYPE_DAG_*`/`MAX_TYPE_DAG_NODES` constants; `PrimitiveType { Bool, I32 }`;
`TypeNode { Primitive(PrimitiveType), Reference(TypeId) }`; structured `TypeDagHeaderError`,
`TypeNodeFault`, and `TypeDagError`; private-field `TypeDagView<'bytes, 'scratch>`; and private-field
`TypeNodeCursor<'scratch>`. No ID reexport.

Validation priority is short header, magic, schema, node count, declared total geometry, scratch
capacity, tag/width, primitive code or edge, then trailing body bytes. Decode into a fixed two-entry
stack array and copy to caller scratch only after every record passes, so every error leaves scratch
unchanged. `AsRef<[u8]>` returns the exact input. Cursor `next` only splits typed scratch; it never
reads bytes or converts coordinates.

Errors retain exact values: header actual; geometry expected/actual; scratch required/available; node
ordinal plus `Truncated { required, actual }`, `Tag { actual }`, `Primitive { actual }`, or
`Edge { target, node_count }`; and trailing `NodeBytes { expected, actual }`.

## Falsifiers and limits

Goldens cover empty, each primitive, self reference, and paired forward/back references. Every header
prefix/cell, record prefix/tag/code, reference target, trailer, and short scratch asserts exact error
and unchanged scratch. Returned bytes and nodes must point into the caller's input/scratch. Actual
exported format+vocab rlibs must emit E0308 for `EntityId` in `TypeNode::Reference`; a one-token
`TypeId` mutant compiles. Constant-node and input-removal mutants make named tests red.

Formatted caps: production module 240, lib delta 20, runtime tests 260, rlib addition 50, human evidence
160. A separate 65,536-byte cap covers exactly four deterministic `gzip -n -9` artifacts under
`evidence/p5-c1/type-dag-ref-read/codegen/`: consumer and owner LLVM plus AArch64 assembly. Measure
sizes/alignment, caller live bytes, scalar proof copy, full optimized consumer/validator
code, allocation, panic/unwind, indirect calls, and residual decode. No zero-cost/panic-free claim.
Run formatting, warnings-denied Clippy, retained workspace tests, two distinct fresh-target gates,
diff/status checks. Any cap breach, second decode, public field, allocation/unsafe/dependency, or
fragment change rejects the slice.

## Next decision

With local evidence clear but agent roles unavailable, retain `EVIDENCE_BLOCKED` for independent
calibration/review. Next add ordered products as a separate edge-position slice.
