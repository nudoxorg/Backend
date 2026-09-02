# Research journal: rust-analyzer-semantic-lane

| source/experiment | mechanism | conflicting trade-off | decision or falsifier changed |
| --- | --- | --- | --- |
| `compiler/ir/wire.rs` header layout read (2026-09-02) | `declared_length` sits at offset 8 (magic 4 + schema 2 + section_count 2), not 12 | Test harness read 12..16 (first directory entry kind+requirement = 0x00010001 = 65537) | R1 root cause; one-line harness fix, fragment encoder innocent |
| `compiler/ir/type_facts.rs` payload grammar | Records (variable: owner u32, tag u8, payload0/1 u32, text cell ×2, nominal cell, children span u32×2) then u32 child count, then one pooled variable-size child array | Test helper assumed fixed 7-byte inline children per record | R2 falsifier ownership: harness walker must walk all records then the pool |
| `compiler/driver/lower/rust.rs` `emit_occurrences` | Method calls stream from source-tree `descendants()` only; tokens inside `once!(..)` are a token tree, not cast expressions | `Semantics::descend_into_macros` reaches expanded nodes; `projected_span` maps back or returns None for foreign-file projections | R3: authority-level descent + projection, no re-emission duplication |
| `compiler/driver/lower/rust.rs` `where_rows` + `compiler/ir/extension_pools.rs` | Constraint cell is a type-fact ordinal; foreign traits already exist in the lane as deduped `Unknown(UnresolvedExternal)` leaf rows (`Option`/`Box` precedent) | Dropping foreign-resolving rows loses written facts; a name-only row is impossible (pooled lane has no text cell) — but the foreign-leaf row IS expressible | R4: emit a row per written predicate; constraint = local trait ordinal or deduped foreign-leaf ordinal; None only when resolution failed entirely |
| `compiler/languages/rust/authority.rs` `analyze` | `load_workspace_at` runs `cargo metadata --offline` internally and already unifies features per `CargoConfig`; config exposes `all_features`/`no_default_features`/`features` | PURL layer needs only member/cache location + config wiring, not a second metadata implementation | R5–R7: zero new dependencies; network fetch stays a parent fork |
| rust-analyzer book (architecture), prior lane knowledge | `definition_origin`, `Semantics::original_range`, `hir_file_for` provide macro provenance | — | Confirms macro-inner facts are provable without new dependency surface |
| Working-tree observation | Concurrent go/python/csharp/typescript lanes commit shared `lower.rs` and sibling files in this worktree | Worker cards must pin exact owned paths; gates re-verify clang-lane mechanical stabilization before each run | Coordination law recorded in brief.md |

Saturation: for each open decision (R3 descent mechanism, R4 representation,
R5–R7 offline locate scope) two independent sources/experiments added no new
candidate-changing facts; research stops there with the uncertainty
(retained: whether `descend_into_macros` yields stable same-file projections
for all macro shapes — the falsifier will decide).
