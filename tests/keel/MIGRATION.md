# CLI and MCP test migration inventory

This is a cutover map, not authorization to delete tests today. Remove a Rust
case only after its replacement row runs the built product processes and its
expected result is reviewed. Keep implementation-level protocol laws until the
new runner has a named protocol row that drives the real process boundary.

## Existing process coverage

`tests/journeys/tests/surface_interfaces.rs` starts the built `backend-journey-locald`,
then sends real CLI and MCP process requests through the same owner. It covers
surface registry reachability, typed request/reply variants, selected identities,
transport parity, and a Trustfall page/resume/cancel flow. The other important
existing rows are `tests/journeys/tests/surface_matrix.rs` (one production
declaration matrix over the supported source languages), `surface_parity.rs`
(same-revision CLI/MCP result identity), `new_user_remaining_surfaces.rs`,
`source_range_journey.rs`, and `css_impact_journey.rs`. These are process
journeys, not isolated calls into the product libraries.

The answer-table successor should preserve that style: each row names a user
operation, starts CLI and MCP as actual child processes against one owner and
workspace, and compares stable JSON fields, exact identity/cursor bytes,
stdout/stderr where they are part of the CLI contract, and exit status against
reviewed expectations. Strip only documented volatile fields (for example
timestamps and process IDs). Keep diagnostics and timing outside the answer
oracle. Do not make the snapshot the only assertion: include negative controls
that change one input and require a typed refusal.

## Deletion candidates, gated on replacement rows

| Current module/cases | User-visible requirement to carry forward | Replacement row and expected result | Remaining gap / severity |
|---|---|---|---|
| `apps/cli/src/tests.rs`: `every_registry_row_is_reachable_by_the_words_a_person_types`, `help_is_grouped_by_domain_and_names_every_domain`, unknown command/option and missing operand cases, global page-bound refusal, `add_defaults_to_interactive_and_advertises_the_background_option`, quick-start aliases, installed fault actions | CLI argument grammar, documented command discovery, actionable usage failures, aliases and background intent | `cli-usage`: invoke the compiled CLI with valid, missing, unknown, and aliased argv; compare exit code plus stable stdout/stderr; follow one accepted command through MCP and assert the same typed command/result | **High** until all user-facing parse/help outcomes are represented. Internal lowering tests elsewhere in this file are not covered merely by help snapshots. |
| `apps/cli/src/tests.rs`: `a_fault_chooses_the_exit_code_a_script_can_branch_on`, `an_admitted_dependency_refusal_keeps_json_evidence_and_a_nonzero_exit`, `admitted_partial_is_qualified_cli_success_with_exact_receipts`, `markdown_output_is_the_shared_renderer_and_json_is_the_typed_dto`, published-name partial result | JSON, text and Markdown answer shape; qualified partial success versus hard refusal; scriptable exit code | For each representative success/partial/refusal, run CLI and MCP subprocesses on the same fixture; check JSON structurally, exact stable text sections, exit code, and that an MCP tool error is only emitted for a hard refusal | **High**: current process journeys prove transport paths and some parity, but do not yet snapshot the whole CLI diagnostic/exit contract. |
| `apps/cli/src/tests/composite_show.rs`: `large_show_defaults_keep_exact_identity_and_page_all_members_with_zero_exit`, `copied_show_selected_continuation_keeps_same_coordinate_rows_distinct` | A large `show` remains complete through pagination and preserves selected row identity | Dense retained catalog -> CLI `show` with copied continuation -> MCP `backend.document`; compare full ID set, no duplicates, same root and continuation operands | **High**: existing interface journey exercises ordinary document reads, not this dense cross-page proof. |
| `apps/mcp/src/jsonrpc/tests/named_call.rs`: compact schema, defaults, mutation, cursor, target and budget user-visible cases | `backend.*` named MCP routes preserve registered schema/defaults and typed results; malformed inputs refuse before owner dispatch | MCP process initialize/list-tools/call sequence; assert advertised schema and a valid-invalid-valid triplet with stable request IDs and exact typed error; include one resumable query | **High**: journey calls are representative but not an exhaustive registry/schema contract. |
| `apps/mcp/src/jsonrpc/tests/usage_argument_names.rs` and user-visible missing-argument cases in `tests.rs` | MCP diagnostics name JSON argument fields, not CLI flags; malformed calls do not poison the next call | Send malformed `tools/call`, then a valid call over the same MCP process; assert JSON-RPC error vs tool-level error distinction and next response ID | **Medium**; not shown by normal successful surface snapshots. |
| `apps/mcp/src/jsonrpc/tests/semantic_source_pages.rs`, `source_continuation.rs`, `document_section_pages.rs`, `references_pages.rs`, `token_outline.rs`, `record_budget_pages.rs`, `package_graph_budget.rs` | Large payloads page atomically; exact continuations resume only for their original basis/selection; UTF-8, CRLF and escaped bytes stay intact | Add dense corpus cases to table with every page traversed; assert full ID/byte reconstruction, then mutate root, selection, cursor, or caller lifetime and expect the named refusal without partial rows | **Critical**: current ordinary process journeys do not replace these adversarial boundary cases. Keep until a process-level pagination/negative-control row exists for each protocol family. |

## Keep as protocol laws until process-level rows exist

These test groups are not safely superseded by ordinary CLI/MCP answer
snapshots. Retain them until the harness contains adversarial process rows
that demonstrate the same law at the wire boundary; then migrate one law at a
time, not by deleting the whole module.

| Modules / law families | Why a normal answer table is insufficient | Minimum integration replacement |
|---|---|---|
| `apps/cli/src/tests.rs` and `apps/mcp/src/tests.rs`: bounded framing, truncation, oversize frames, canonical codec vectors, correlated IDs, request/reply shape, producer certificate and wrong-package certificate checks | These are wire admission and identity laws; a happy-path snapshot can still pass if malformed input is accepted or correlation is lost | Drive malformed, truncated, oversized and mismatched frames through the actual CLI/MCP transport process; assert refusal, no owner mutation, valid following request, and exact request ID/certificate binding |
| `apps/cli/src/tests/index_wait.rs`, `owner_recovery.rs`, `apps/cli/tests/saved_index_wait.rs`, `apps/mcp/src/jsonrpc/tests/initial_owner.rs`, `index_overlap.rs` | Timeouts, ambiguous mutation delivery, replacement-owner authentication, cancellation, and shared durable ticket identity depend on process death and ordering | Use controllable child owner processes: kill or stall at named boundary; restart; assert one durable operation key, no duplicate mutation, original absolute deadline, and exact terminal receipt from CLI and MCP |
| `apps/mcp/src/jsonrpc/tests.rs`: direct cursor MAC/domain/expiry/cross-role vectors; stdio cancellation and half-close | Cryptographic compatibility and stream lifecycle need exact hostile-wire inputs; ordinary tool output does not exercise them | Replay checked-in byte vectors through the MCP stdio process; forge/expire/cross-role cursors; half-close/cancel and assert framing, bounded termination, and no stale success |
| `apps/cli/src/tests/{outline_admission,versions_admission,membership_admission,graph_window_admission}.rs`; MCP pagination modules | They assert atomic refusal on changed roots/scopes, budget math, all-row completeness and no skipped frontier under deliberately dense data | Process cases must synthesize data above budgets and inspect every continuation plus refusal controls; until those rows exist, **critical** uncovered behavior remains |
| `apps/mcp/src/jsonrpc/tests/{surface_fidelity,search_admission,typescript_admission,stylo_transport,package_compiler_outcomes}.rs` | Typed language/compiler provenance, partial support versus refusal, CSS worker failure, and missing-coverage semantics may be absent in a generic polyglot happy path | Per-language before/after project case with a deliberately unavailable/broken tool control; assert readiness, provenance and stable refusal while other languages stay ready |

Already-process tests stay: `apps/cli/tests/passive_health.rs` checks the
executable does not start an owner or create state for passive health;
`apps/cli/tests/saved_index_wait.rs` exercises the executable across owner
retirements. Do not count them as unit-test deletion targets.

## Runner holdout: test the test boundary adversarially

Before the runner can gate product suites, add an isolated subprocess
acceptance case (not a test of private helper functions) using temporary
manifests and child programs:

1. Give the runner a syntactically malformed manifest and assert a nonzero
   result, a diagnostic naming the manifest and parse location, no product
   process started, and no pass/snapshot artifact written.
2. Give it a valid one-row manifest whose declared process never exits. Require
   failure within the configured deadline plus a small fixed cleanup allowance;
   verify the runner kills and reaps the owned process group (including a
   grandchild holding stdout/stderr open), reports the row and timeout, and
   does not hang while draining pipes.
3. Run a neighboring good row in the same fixture set and require its result to
   be retained separately from the timed-out row. This proves one bad process
   cannot create a false pass or erase unrelated evidence.

The holdout should invoke the checked-in runner executable as a child and
inspect its status, bounded stdout/stderr, report file and process cleanup.
Keep the timeout comfortably above scheduler noise but below the normal CI
job timeout. These cases are high severity: without them, malformed case data
can be silently skipped and a hung child can wedge the gate.
