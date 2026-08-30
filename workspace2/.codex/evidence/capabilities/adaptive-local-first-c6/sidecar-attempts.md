# Pre-edit sidecar dispatch attempts

No dispatch row below is a reviewer result. Attempts 1 through 5 used the first read-only export;
attempts 6 and 7 used the corrected read-only export. Every measured before/after aggregate source
digest matched. Only attempt 7 had the required empty workspace-write allow-list and explicit
disposable `TMPDIR`/`CARGO_TARGET_DIR`; attempt 6 was an in-app custody diagnostic.

| Attempt | Parent task | Requested reviewer | Raw operational result | Disposition |
| --- | --- | --- | --- | --- |
| `pre-edit-dispatch-1` | `01a05148-0a64-74f0-80f1-08c8f774453a` | registered `nudox_terra_reviewer`, Terra/xhigh | The temporary role registry omitted required peer role files; the parent then reported `Full-history forked agents inherit the parent agent type; omit agent_type, or spawn without a full-history fork.` No reviewer task ID appeared. | Invalid dispatch; retained as configuration/fork diagnostic only. |
| `pre-edit-dispatch-2` | `01a05149-1a6a-7f11-ac16-6e8ec3ff06db` | registered `nudox_terra_reviewer`, Terra/xhigh, `fork_turns="none"` requested | Complete role files were present. The parent emitted a wait tool item with `receiver_thread_ids: []`, created no reviewer child, then logged `failed to renew cache TTL: missing field base_instructions at line 97 column 5`. | Invalid dispatch; retained as distinct sidecar/runtime diagnostic only. |
| `pre-edit-dispatch-3` | `01a0514e-3d88-7c71-b631-0a94429e06fc` | registered `nudox_terra_reviewer`, Terra/xhigh, `fork_turns="1"` requested | A minimal registry contained only the copied reviewer config and the revised packet. The parent created no reviewer child; the raw event stream reports `collab spawn failed: no thread with id: 01a0514e-3d88-7c71-b631-0a94429e06fc`, `agent_name must use only lowercase letters, digits, and underscores`, and `missing field base_instructions at line 97 column 5`. | Corrected path still cannot spawn; external runtime owner required. |
| `pre-edit-dispatch-4` | `01a0516d-e712-71c3-9858-f86333876129` | registered `nudox_terra_reviewer`, Terra/xhigh, `fork_turns="none"`, task `pre_edit_reviewer` | The corrected distinct Sol/low parent used a minimal reviewer registry and the live `pre-edit-2` packet. It emitted an empty `receiver_thread_ids` array, no child task ID, and repeated `missing field base_instructions at line 97 column 5`. | Fresh resumed-audit blocker; no review exists. |
| `pre-edit-dispatch-5` | `01a05171-8344-7880-ab42-2fa73e586901` | same exact corrected Sol/low and reviewer contract as attempt 4 | The only changed variable was the Codex executable: desktop bundled `0.151.0-alpha.7.1`. The cache-schema error disappeared, but its raw stream immediately emitted `wait` with `receiver_thread_ids: []`; no reviewer child task exists. | Permitted executable-only alternate exhausted; no review exists. |
| `pre-edit-dispatch-6` | `/root/c6_corrected_review_dispatch` | registered `nudox_terra_reviewer`, Terra/xhigh, `fork_turns="none"` | The in-app Sol/low parent returned `agent thread limit reached`; no child or receiver ID exists. | Corrected-packet diagnostic only; no review exists. |
| `pre-edit-dispatch-7` | `01a0518b-b234-7042-bd8f-3b90c315ef40` | registered `nudox_terra_reviewer`, Terra/xhigh, `fork_turns="none"` | Fresh disposable-root Sol/low parent narrated dispatch but its first tool event was `wait` with `receiver_thread_ids: []`; runtime workers then panicked while reading an invalid environment value. | Corrected-packet bounded sidecar exhausted; no review exists. |

The original packets were invalidated by committed chief corrections. `pre-edit-3.md` is the live
packet, but attempts 6 and 7 produced no reviewer for it. The corrected snapshot digest remained
`92e0cff7855a0257622d358d1697ffc32b59565e769b1d0c88a7087413ec99d6`. Exact available runtime
receipts are retained under `raw/`; their SHA-256 digests bind the index.
