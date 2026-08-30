# Wave A.1 foundation integration — Phase 0 research journal

| Source / experiment | Mechanism fact | Decision or falsifier changed | Status |
| --- | --- | --- | --- |
| `PACKED_COLLECTIONS.md` §§ canonical and range progression | Canonical pack bytes stay unchanged while external range proof/outboard data may authenticate a partial fetch; range bind requires count, digest/proof, and artifact identity | Rules out relabeling the already-known directory prefix hash as authentication; A2 requires the expected full artifact and explicit range | adopted |
| `crates/nudox-object-pack/src/{write,header,index,view}.rs` at frozen red commit | Writer has exact full output preflight; header gives measured prefix; index validates fixed directory and cumulative body extents but has no full-artifact proof witness | Preserves writer preflight as the proof-production entry point and requires a new authenticated wrapper rather than treating `ObjectPackIndex` as authenticated | adopted |
| `crates/nudox-root/src/{packed,locality/view}.rs`, `crates/nudox-hydration/src/{need,publication}.rs` | Native root construction retains packed rows; canonical writer grammar already exists; `Need::bind` owns generation comparison and publication already has a private verified witness | Migrates the planning root borrow to a canonical `ValidatedRoot`; retains one demand bind and evolves, rather than duplicates, the verified capability | adopted |
| `crates/nudox-store-memory/src/store.rs` and `crates/nudox-operation/src/pinned_object.rs` | Store is generic owner-first-write; operation currently starts from a view and repeats request generation/object equality | Store must consume a capability from `nudox-object` without importing pack; operation must consume hydration's capability and delete repeated checks | adopted |
| Bao upstream design/docs and BLAKE3 tree-hash documentation, to be recorded with immutable source revision during implementation selection | A content-addressed Merkle tree can verify an arbitrary byte range against a known full-root hash, but the proof grammar/outboard lifetime and locked dependency closure are material protocol choices | Keeps ABI proof carriers opaque and blocks preselection of Bao or any new dependency before a measured private control comparison | unresolved, no protocol decision made |
| Direct source-isolated reviewer `01a05156-e0f9-7d01-b958-298f51832b45`, receipt `packets/pre-edit-1-reviewer.jsonl` | The original packet did not include digest-matching brief/matrix/index inputs, its source export exposed historical `MANAGER_EVIDENCE.md`, and the chief test discarded a rejected owner while never varying the expected full-pack artifact | Invalidates the first review packet; adds chief red falsifiers for owner identity, different artifact identity, and adjacent boundary/no-half-state cases before an implementation candidate can be selected | adopted; Phase 0 reopened |
| Chief red replacement `7f90b60d8ee3efa27c7d60f6819781fab2327f71` | Exact missing identity, rejected-owner bytes/pointer/capacity, wrong full artifact, header/index truncation/mutation, stale bind, and no-capability/store-state observations are now public red assertions | Clears the prior chief-test gaps; matrix remains RED until production APIs make the hardened test pass | adopted |

Research saturation for Phase 0: two independent local sources establish the
existing format and consumer ownership; neither provides the missing
full-artifact range-verification mechanism. The external primary-source row is
needed only when choosing a concrete private proof algorithm. If it requires a
new dependency, persistent outboard format, unsafe/SIMD, or changes the
permanent `ObjectPackEncoding` meaning, stop for `AUTHORITY_FORK` instead of
silently selecting it.
