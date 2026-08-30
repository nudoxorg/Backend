# Durable publication Wave A.2 — evidence-blocked closure receipt

## Terminal state

**EVIDENCE_BLOCKED.** No candidate or production implementation exists. Phase 0 cannot authorize the
required Luna worker or a production edit while blocking review findings remain. This is an evidence
non-acceptance, not an authority decision, product rejection, or law waiver.

The dispatch condition has since been satisfied by the valid source-isolated reviewer recorded in
`reviews/pre-edit-2.md`; its verdict is **BLOCK**, not approval. This receipt preserves the earlier
two failed sidecar attempts as history. The manager answered F-01 and F-02 with the self-contained
`review-bundles/pre-edit-3` bundle and the frozen contracts under `contracts/`. The chief answered
F-04 by formatting the public oracle and correcting its feature-gated run instruction in `879866bd`.
Those corrections have not been independently approved. F-03, the shared fresh-cache offline tooling
closure, remains an independently reproduced release blocker.

The frozen source snapshot is `4c60694ddf25744dcde60936eef486a6c492df4e`, tree
`a15886a9609860b01be80b01b106b9cc7e494a63`; its rationale-free packet is
`packets/pre-edit-2.md`, SHA-256
`d48564edfbcb3379c53e13f6108d2b91a4b007580f0bed966379db41fcb124da`. The Sol-owned oracle remains
`5e4c1b6f9217cb0d5500b541a3da30dbb0149ffd`, feature `wave-a2-publication-red`, with the no-ignored
command recorded in `index.toml`. The chief integrated the byte-identical oracle as `11124e6a` and
then changed only its formatting and run comment in `879866bd`. No production semantics were added.

## Bounded diagnostics and raw receipts

The authoritative dispatch procedure was read from skill source
`b34735817b7313dcb1e3224ea89d39a8246434ca` and recorded in
`reviews/dispatch-custody-correction.md`. Two materially different Sol/low sidecar attempts used the
same source, packet, fork/no-write custody, and only one changed operational variable:

| Attempt | Parent/task | Raw path | Result |
| --- | --- | --- | --- |
| Required sidecar procedure | `01a0516f-a8ce-7d92-b746-4da2398880c6` | `/tmp/nudox-a2-sol-dispatch.DTd9WW/sidecar.jsonl` and `parent-final.md` | Registered spawn rejected `gpt-5.6-luna` as unknown; no child ID. |
| Terra-default alternate | `01a05171-18a5-7ff0-b29d-da351cefc866` | `/tmp/nudox-a2-sol-dispatch-alt.nFgCRj/sidecar.jsonl` | Empty `receiver_thread_ids`; no spawn result or child ID. |

Raw SHA-256 values, sidecar roots, exact variable, and the rejection rule are retained in the custody
correction. Earlier Luna-parent and direct-Terra runs, including `01a05154` and stale `01a0516b`, are
preserved only as non-review operational evidence. They cannot stand in for a registered reviewer.

## Affected evidence and external action

All DP-01 through DP-15 remain at their pre-edit states; DP-15’s warmed `FileJournal::append` control
is still `REPRODUCED BY TERRA`. The corrected Sol/low dispatcher returned the nonempty registered
reviewer ID `/root/nudox_sol_review_dispatch/nudox_terra_reviewer`, so reviewer custody is no longer
the active block. The unresolved external action belongs to the shared quality/Nix tooling owner:
provide the locked registry crates, pinned Rust-Clippy Git source, and generated Dylint driver through
the pinned offline closure, then prove the complete gates from empty external caches. After that
repair, export a new source-only snapshot and obtain a fresh corrected-custody pre-edit review of
`review-bundles/pre-edit-3`. Do not reuse failed roots or reinterpret this BLOCK as approval.
