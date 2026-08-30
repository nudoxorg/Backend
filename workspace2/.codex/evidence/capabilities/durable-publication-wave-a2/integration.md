# Chief integration receipt — durable publication Wave A.2

## Integration boundary

The chief branch selectively cherry-picked the manager's evidence commits. It did not merge the
manager branch or `orchestra-shared`, and it did not import a production candidate. The manager
terminal remains `EVIDENCE_BLOCKED`; no Luna implementation card was authorized and no failed or
superseded operational attempt is reclassified as review or approval.

| Manager commit | Chief commit | Integrated mechanism |
| --- | --- | --- |
| `dea46fb0` | `bfb85392` | Phase-0 evidence freeze |
| `98f1c6d3` | `1159c2ec` | Self-contained pre-edit packet |
| `4ec8c959` | `01727186` | Authority ownership decision |
| `38bfc26b` | `0606ce84` | Pre-edit findings and warmed append control |
| `5e0bf174` | `8ee48607` | Sol-oracle binding |
| `4c60694d` | `afd923fa` | Exact red-terminal freeze |
| `a87a33bc` | `f1bc7746` | Source-only oracle snapshot binding |
| `e85e2a41` | `5d24393c` | Corrected reviewer-custody procedure |
| `9bfc2627` | `a77eab38` | Evidence-blocked terminal receipt |
| `b58b9e75` | `b909dd51` | Valid BLOCK review, frozen contracts, and self-contained next-review bundle |

Manager commit `5e4c1b6f` was deliberately not cherry-picked because the chief branch already owns
the byte-identical oracle in `11124e6a`. At both commits, the manifest blob is
`097869b83d62f00360be8d1a94aa3e0d99b880c2` and the public journey blob is
`4c05eed66a8cbf711d2ca50a7d4899b165c9362b`. Historical manager receipts retain the manager commit
name; this receipt binds those bytes to the chief commit without rewriting custody history. Chief
commit `879866bd` subsequently changes only the oracle's run comment and rustfmt layout.

## Corrected review custody

The operational procedure is bound to the two reviewer-sidecar skill files as fixed at shared
commit `b34735817b7313dcb1e3224ea89d39a8246434ca`; those files were read without merging that commit.
A valid next review requires a dispatch-only `gpt-5.6-sol`/`low` parent, a registered
`nudox_terra_reviewer` child spawned with `fork_turns="none"`, effective
`gpt-5.6-terra`/`xhigh`, and a nonempty runtime child task/receiver ID. The two bounded attempts in
`reviews/dispatch-custody-correction.md` produced no such ID. They remain operational failure
receipts only. A later native dispatch satisfied the procedure: parent
`/root/nudox_sol_review_dispatch` returned child
`/root/nudox_sol_review_dispatch/nudox_terra_reviewer`, whose source-isolated verdict is preserved
verbatim in `reviews/pre-edit-2.md`. Its result is **BLOCK**, so implementation remains unauthorized.

## Retained and rejected mechanisms

Retained: the accepted single-owner `FileJournal`, exact public red consumer, warmed nonempty append
allocation control, proof matrix, immutable-publication/head laws, the non-forgeable authority and
resource/fault contracts, the valid BLOCK review, and raw failed-dispatch receipts. Rejected: P2
production code, unconditional local-error oracles, direct/stale/full-history review substitutes,
any review narration without a nonempty child ID, and any production or transport work that would
bypass the frozen pre-edit review or shared fresh-cache offline gate.
