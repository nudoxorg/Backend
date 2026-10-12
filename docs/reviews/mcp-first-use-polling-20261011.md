# First-use indexing observation policy

Base: `f30aae0998ff60571ba65c97e31cea8fdf0b1755`. This packet is source-only. No Cargo build, native test, owner process, compiler, or live CLI/MCP call was run by this worker. Passing results on another commit do not validate this packet.

## Concrete defect and change

An accepted durable index request could return only prose naming `backend.index_progress` and `operation_key`, without a copyable call. Active durable receipts also acquired the legacy ticket footer, which recommended a process-scoped ticket instead of their durable identity. Published, partially published, unresolved, unknown, and expired receipts received the generic saved-operation poll footer too. CLI instructions used `backend index_progress`, although the installed user command is `nudox`.

`ProductView::index_poll_affordance` now owns the observation policy. Known Accepted/Active receipts select the exact durable operation key. Other durable observations select no poll; their exact evidence and existing faults remain available. Legacy job results without durable evidence retain the exact ticket and event sequence policy, including cancellation-request acknowledgements. Both text renderers, the CLI product DTO at every detail level, and MCP's raw index projection derive their optional `index_poll` from that policy. The DTO contains the executable `nudox` shell command and the same MCP `backend.index_progress` call. It is guidance, not a new request authority or a publication claim.

The raw operation, request digest, source capture, partition, fault, and Active ticket remain unchanged. The ticket remains available for exact cancellation. No journal, indexing, reconnect, cancellation, query-selection, or publication logic changes. In particular, this packet never retries or reindexes unknown/unresolved/expired work. MCP advertises the same poll schema on `backend.index` and on start/progress/cancel, keeping the latter's closed output envelopes. Old product DTOs remain readable with a missing optional field.

## Scope review

The shared client journal persists the key before normal CLI/MCP index dispatch. Both adapters route status reads through its terminal acknowledgement boundary. The normal CLI `add` and MCP `backend.index` paths both project the same shared `Answer::Product`; they therefore get the same next observation as explicit keyed start/progress tools. CLI help now distinguishes starting work from receiving a terminal receipt.

The existing first-use readiness boundaries remain typed: missing Names materialization suggests shelf inspection; incomplete materialization suggests status inspection; health availability/publication/native-oracle readiness remain separate; dependency absence retains the requested package; compiler setup refusal retains its selected tool and actionable setup cause. Exact Names and Tantivy search intentionally have different matching grammars. Underlying exact/search/graph correctness and cold selected-query work belongs to the sibling slice; this packet makes no new readiness or compiler-coverage claim from an empty result.

## Discriminating test gates

All native gates below are **UNRUN for this source** and must be executed by the sole ILO broker on the final composed source. No test filtering or fixture weakening is required.

| Law | Evidence required |
| --- | --- |
| `durable_accepted_and_active_work_share_one_copyable_restart_safe_poll` | Accepted/Active, Started/Status and Summary/Full retain the exact operation; one key-based call and installed shell command; copied CLI operand lowers to a status read; complete response fits the budget; older DTOs still decode. |
| `durable_index_operation_projection_preserves_all_states_and_published_receipt` | Existing complete observation matrix now also rejects polls for Published/Partial/Failed/Unresolved/Unknown/OutsideReceiptWindow. Existing terminal faults, compiler setup cause, source capture, and publication receipt controls remain. |
| `owner_index_job_tools_advertise_exact_tickets_and_immediate_progress` | The normal first-use registry tool and all job tools advertise the same additive field; job schemas remain closed and unrelated search schema is unchanged. |
| `index_first_use_returns_one_admitted_durable_poll_and_copying_it_only_observes` | Closed backend decode/admission validates key/package/intent before the presentation fixture; normal `backend.index` Summary/Full has one exact poll; copying it routes only a status read; exact ID, result classification, raw receipt and complete stdio-line budget metadata remain. This is not a compiler or disk-journal test. |
| `keyed_index_progress_preserves_durable_owner_evidence_and_rejects_mixed_identities` | Returned call can be fed back through the actual MCP router; same operation evidence survives; mixed key/ticket/sequence and invalid key cases still reach no owner command. |
| `partial_terminal_keeps_error_flag_exact_ticket_partition_and_receipt_in_the_same_mcp_wire_result` | Existing closed-wire partial/cancelled/accepted cases additionally distinguish a pending poll from terminal omission while preserving fault slug/cause, exact request digest, source partition and receipt. |
| Legacy ticket/sequence and maximum owner-page tests | Exact executable legacy polling and cancellation acknowledgements remain; the full 16-event page must still fit the complete MCP response budget with the new additive action. |

Required package gates: full unfiltered `backend-present`, `backend-cli`, and `backend-mcp` library suites on the final source. Real matched binaries must separately exercise `nudox add .`, the returned key poll through terminal state, `nudox search`, and MCP first-use reads; source fixtures cannot establish those results.

## Source gates and self-review

`git diff --check` passed. All changed Rust files parsed through rustfmt; formatting checks passed for the eleven files without baseline formatting debt. `budget.rs` retains untouched baseline test formatting and only adds the field to both summary construction and its struct; its rustfmt parse gate passed. Source review checked all `Affordance` matches, DTO constructors, CLI grammar, both adapter index/journal callers, MCP route selection, output schema and complete-line accounting. No blanket lint allowance, coverage deletion, response-budget increase, or dependency change was added.

Existing Luna peer `/root/luna_cursor_work_audit` reviewed the source. Its concrete normal-`backend.index` output-schema gap was corrected by a shared schema and tools/list assertions. Frozen-source review and native gates remain separately reportable.
