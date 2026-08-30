# P1 canonical-root baseline digest and LOC ledger

Source candidate: `44c22154fd5238e4769562590420371979306050`.
Manager branch/worktree: `codex/prototype-canonical-root-hydration` at
`/private/tmp/nudox-prototype-canonical-root-hydration`.

| Path | SHA-256 | Formatted LOC | Scope |
|---|---|---:|---|
| `crates/nudox-root/src/packed.rs` | `fff2cd838eada535296ae7ec3e985d8682a04a068964307256bffb727ff56649` | 384 | C0 row owner/control |
| `crates/nudox-root/src/encode.rs` | `7aabc548df9c9814a2522f3cdcfc0c42cb1eb2bd23337285f4246036b3434b04` | 116 | canonical writer/hash grammar |
| `crates/nudox-root/src/builder.rs` | `c2bc75534670d391cb00c54ea5e7879ed37551af25fdd5769ca19fe2ef6296e8` | 329 | arbitrary-order build/control |
| `crates/nudox-root/src/closure.rs` | `d33cf2248f7ce8e82c9f9a5889ae53234dd12465db5c3f0845ed6e757785c274` | 282 | caller selection scratch |
| `crates/nudox-root/src/locality/view.rs` | `3b872a35bddb1623364d6a5f04cc4d57e6159984270f7d0058b26c69e0fbef4f` | 488 | coherent view/control consumer |
| `crates/nudox-root/src/lib.rs` | `e4a903df1bd552ebd3adf4daf5abd0112d3374ef9e0fed8f5e4271a4567744b9` | 38 | public root surface |
| `crates/nudox-root/src/tests/root_contracts.rs` | `dee4b38b65c0ece8caa792202d0260790a1b3fe1577530ac60733656d7acae9f` | 231 | C0 contracts |
| `crates/nudox-root/src/tests/selection_contracts.rs` | `987c12f1fa1fd10290db1eff4bd8e8521c49ac72c816bd2c6cb456950593c68c` | 127 | selection control |
| `crates/nudox-root/src/tests/locality_contracts.rs` | `9d4472b47ae299cc46baa00ec9b1e908a96aceb7c4f5e3c330334242652bd05c` | 276 | locality independence control |
| `crates/nudox-root/tests/locality_validation.rs` | `afdc418186c33453fae743f6a19dd3bf29cc9704ff37bc669c71de313f153d17` | 155 | ordinary top-level test control |
| `crates/nudox-hydration/src/need.rs` | `854fed3155b635b3aa6456c44a58d978f4b390785b6aed1632a4f5b3fa5d537a` | 129 | bound consumer |
| `crates/nudox-hydration/src/plan.rs` | `2921f0ec756f21ee983e6e38203b61cf0bccc1546a32b763f5f145e282b5969b` | 275 | planning authority |
| `crates/nudox-hydration/src/publication.rs` | `e6300a419c3c08c4362c98d25e387b1d2868d841e6e1a5a671ecde6f64cf87a4` | 95 | complete-closure fact only |
| `crates/nudox-hydration/src/tests.rs` | `09c31a4edcc960e54630a773b47d9b243d1fc5d277277b251aec854f7a0295d4` | 175 | internal control tests |
| `crates/nudox-hydration/src/tests/planning.rs` | `298f263115c0be9ffef3572f9241e1fe5e08c061997748575eee90cdc6ca8f78` | 219 | planning control tests |
| `crates/nudox-hydration/src/tests/publication.rs` | `7293a233a5e98d133e70b4f589de9cd84bae0587784f98e43957687520099e45` | 101 | verified-only control tests |
| `crates/nudox-operation/src/pinned_object.rs` | `35ac88109fc3dc45e530c970e851462e0c107753ee3aee4054a3bfd075c322ad` | 478 | read-only adjacent consumer |

Baseline command: `cargo test -p nudox-root -p nudox-hydration --lib`.
Result: 32 passed, 0 failed on the isolated baseline. No source or manager worktree
dirty paths were reported before this ledger/card was written.

Initial committed card SHA-256:
`cfdbf68cab018b9f88fe4d919298eea202a9af710dcb5a316c680bdbc68d6197`.
Calibrated card SHA-256:
`4947ae3dd34e068d56aa05bc3f0600252e18c214e04dc193d8f3f76f3f88ce45`.
That card's C1 rejection and the `P1_CANONICAL_ROOT_CLOSURE.md` packet were retracted
after the accepted `nudox-object-pack` typed-slice precedent was inspected. The first
corrected C1a card exposed a partial-`GenerationView` boundary and is retained only as
rejected calibration evidence. The first split C1a card SHA-256 was
`eb4edc9850f4597163434a1dbdc4ea8c06e28e646bba4d44ee968db737f9534d`. Fresh explicit
Luna cold reading and independent Terra review also rejected that card as not
implementation-ready: the exact grammar/identity rule, borrowed-root coordinate
provenance, and fallible locality scan contract were underspecified. The corrected C1a2
card SHA-256 is:
`5f30af6c191c5ddc2412e4e1d9e29f9cb1fe953a5d285073d43319b59dab444a`.
Its fresh cold reader then found post-validation public-fact rewriting could forge a
root/locality pairing; the established private-facts-plus-`Deref` correction applies to
both witnesses while preserving C0 field reads. The re-budgeted C1a3 card SHA-256 is:
`f3f04853cebd006f450659566f2d5990a8789360a01cdcbb0f73ea1f2b99ab97`.
The allocation-free Floyd hierarchy variant calibrated at that point is retained as a losing
candidate: its deep 100k chain requires quadratic parent follows and fails the whole-operation
work law before any builder dispatch. The replacement C1a4 uses one temporary split
parent-coordinate/2-bit-state `Vec<u32>` during `TryFrom`, then drops it. Its card SHA-256 is:
`aef8752acdafc9d47f55970ad2c4f813cedf2ee1b89222981f02126cbc3dea98`.
Cold review corrected C1a4's small-count scratch bound, unreachable scratch-layout test
claim, and LOC-delta convention without changing the selected safe split-vector mechanism.
The exact C1a5 card SHA-256 is:
`03fd1e43155ccc502decd0d7ac0ed782ec8fdcd8cab41a2d3c27bba13db92299`.
Independent review then corrected the split-vector reserve error to report checked requested
bytes rather than `u32` words, and requires successful lab rows to state allocator-source
UNVERIFIED rather than invent an OOM source. The exact C1a6 card SHA-256 is:
`caa014ae2c1f6588edbc56da3ae3eb917391b0d5d684aa5c209665aac8dc7035`.
Worker commits `3626021f8b79ef4f24d674c7a499c0551efaacd0`,
`a4681ec70d81885a8ccb8de7ac60b492f4416939`, and
`f8eac9c04ccae822075d7449a96d16cc24cf5515` remain only on
`codex/prototype-canonical-root-hydration-c1a-luna-builder` as rejected proof artifacts:
the post-artifact Terra review found synthetic lab allocation/checksum rows, a tiny missing
behavior matrix, hard lint/documentation failures, and a root-view delta over its then-card
limit. That review incorrectly called the card-permitted O(N log N) parent searches and its
required hydration UI fixtures C1b leakage; those two findings were rejected without affecting
the independent P0/P2 non-integration decision. The amended destructive-memoized C1a8 card
SHA-256 is `b5987e9d5961c9ebf2961e61c92b42fb365823a5af3d780faf60377494aa5e67`.
The C1a9 correction separates what the existing `TrackingAllocator` can actually observe from
pointer/copy proof, isolates validation/drop and warmed-use scopes, freezes the global decoder
priority, and adds sentinel-guard pseudocode plus edge tests. Its SHA-256 is
`26d4d3fef91c0a7bffa244d2410e4a51c6c87f92e64a492aa283a56dd8b7d96e`.
The C1a10 cold recheck corrected two literal scope/provenance contradictions: the established
raw descriptor pass now records the exact ordinal/key/source before retained typed-slice access,
and the C0 empty-locality fixture is fully prepared before either C1a tracking scope. Its
SHA-256 is `80cdd35b0e634715eb67b2e062b6343f5267ff60d0f4c30a3e18b14fc56084c7`.
New C1a paths are absent at baseline: `crates/nudox-root/src/root_view.rs`,
`crates/nudox-root/tests/canonical_root_view.rs`, the two named hydration UI fixtures,
and the named layout-lab control/raw TSV artifact.

Final C1a candidate after independent terminal review: `316abe63` on the isolated manager branch.
The source range is `59459baf^..316abe63`; the exact worker source range retained separately is
`6bc8c383^..e850b1ca`. Formatted source evidence: production root delta +599 (638 added, 39
removed), `root_view.rs` 576, behavioral integration test 335, root/view UI sources 59 total,
all C1a Rust test sources 394, and layout-lab control 283. The final manager closure packet is
`P1_CANONICAL_ROOT_C1A_CLOSURE.md`; it records the two clean reproduced gates, raw TSV replay,
candidate decisions, reviewer task/model evidence, and the remaining UNVERIFIED conditions.
