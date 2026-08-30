# Research journal

| source/experiment | unresolved row | mechanism or fact | decision changed | saturation |
| --- | --- | --- | --- | --- |
| `COMPILER_IR_GREENFIELD_PLAN.md`, source topology | R2 | Documents two nested workspaces and exact planned IR/compiler package names. | Freeze package names without inventing frontend adapter package names. | source-specific; live metadata still required |
| `INDEX_GREENFIELD_PLAN.md`, source topology and I0-I6 | R2/R4 | Documents exact index package names; names Tantivy/Qdrant concepts but does not supply every public package/API. | Treat missing names as explicit undocumented blockers. | source-specific; Wave B/C public API naming remains unsaturated |
| `OVERNIGHT_COMPLETION.md`, Waves B-D | R4 | Wave D composes compiler/index/Tantivy/Trustfall/Qdrant and three interfaces; it states no public package/API for graph, vector, placement, or unified interface. | Add five `UNDOCUMENTED_PUBLIC_SEAM` facts. | unsaturated pending product documentation |
| pinned Nix `cargo metadata --no-deps --locked --offline` over four manifests | R1/R2 | Root exposes 13 packages; IR exposes `nudox-ir-vocab`; compiler exposes vocab/registry; index exposes vocab. | Existing C0/I0 vocabulary is observed but cannot satisfy absent later named packages. | paired with document topology for inventory decision |
| `flake.nix` and `tools/quality.sh` | R7 | Existing pinned quality shell owns Rust toolchains and quality flow. | Add only tooling consumer `jq` and a Nix app; no shipping graph change. | one control; implementation verification pending |
| `TESTING.md` adversarial contract | R5/R6/R8 | Exact negative facts, deterministic sequences, and no shadow mock rules apply to the readiness record. | Require real-metadata default and isolated fixture mutations. | contract source; test implementation pending |

Remaining uncertainty: Cargo package presence alone cannot prove public API shape or product behavior;
the terminal names that limit explicitly. The five undocumented public seams need product/documentation
authority, not a guessed implementation.
