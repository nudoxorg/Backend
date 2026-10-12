# Native96 Python authority and preparation fixture correction

This is a source-only correction based on native96's actual unfiltered
five-library run. The baseline is
`b40d6736722cef016e4a2695f4ea6877d0ef7766`, tree
`618603479073307002768024aac5c7fee01c7566`. The run exited 101 after
1015.263251 seconds. This correction has not been built or executed. All
replacement native gates are **UNRUN** until the sole ILO broker admits and
executes the exact composed source. There is no live-product pass claim.

## Exact original evidence

The raw files are in
`/private/tmp/sol61-ilo-real-cli-mcp-20261011/native96/`:

- `receipt.json` SHA-256
  `531d41ce5ca7d7de2f6129d81c6561f8f6b2c87d48e6d3a957a731c9c87bec5c`
- `stdout.log` SHA-256
  `9188458e6dcfa8dda4f7bf0195392b60f1ce6a4d2a1964a85a0a58dc24e31140`
- `stderr.log` SHA-256
  `32ab7300aadb4d26b2209f596938f00877d2e864cb3fc01a640fab6c4c9f5b19`

Ten actual publication/profile-worker controls failed at the same assertion
in `install_mixed_native_compiler`, adapter.rs:7397. The observed capability
was `Deferred`; the fixture required `Ready` before any compiler request.
Those failures do not establish a missing executable, external Pyrefly
helper, native producer image admission failure, or publication failure:
execution stopped before those paths were exercised.

## Native authority ownership

The production `DeferredNativePython` retains the exact executing image and
admits it on the first actual Python request. `LocalCompilerClient::capabilities`
reads admitted immutable facts; inspection deliberately does not hash the
image or manufacture a recipe. `wait_for_toolchain` performs controlled native
image admission before dispatch. Its cancellation/deadline/owner-stop paths
remain unchanged.

The fixture now requires `Deferred` with no toolchain identity, authority
fingerprint, manifest, or session lineage immediately after opening its closed
host. The existing real package operation or direct profile worker remains the
first native request. After that operation, the fixture checks `Ready` and all
four actual authority witnesses. TypeScript remains `Unavailable` before and
after native admission. No unrelated warm-up Generate call, fake authority,
external helper fallback, ambient tool search, or production eager admission
was added.

The original controls still require genuine native Python publication and
preserve their exact source partitions, partial TypeScript refusal, selected
generation, old graph pages through receipt installation, cold retry,
publication-grant ordering, cancellation, shutdown, and next-commit assertions.
The direct profile worker additionally checks actual admitted authority before
its existing selected-current and queued-attempt shutdown assertions.

## Query preparation controls

The structured-query control returned the current typed `graph_query_page`
wire reply, while its fixture expected an obsolete `graph_query` kind. It now
strictly decodes the real reply, correlates request 803, admits the cancelled
page against the exact original owner, and checks its original revision/source,
empty rows, and `Cancelled` terminal. The shared worker stays held with zero
projection builds; after release it builds once. This retains the immediate
cancellation and shared-worker independence controls.

The refused-capture control correctly observed coherent committed structural
rows and a compiler-refusal terminal, then assumed their independent search
preparation was already complete. It now explicitly warms/drains the actual
shared preparation worker under the existing 15-second retirement bound before
the final Search assertion. Workspace and view roots must remain exactly those
captured before the wait. The original in-flight capture checks, strict
compiler-refusal wire assertion, final Search success assertion, no-late-reply
check, and committed coverage assertion are retained. This is not a query retry
on a newly selected basis and does not alter publication readiness.

## Required gates and limits

The replacement unfiltered `backend-local-service --lib` gate must retain all
ten original failures and both preparation laws:

- `existing_index_worker_shutdown_preserves_selected_current_and_retires_queue`
- `index_publication_worker_failed_capture_cold_retry_preserves_original_terminal`
- `index_publication_worker_native_dispatch_preserves_old_graph_through_receipt`
- `index_publication_worker_native_grant_unwind_preserves_selector_for_next_publication`
- `index_publication_worker_native_partial_dispatch_preserves_old_graph_through_receipt`
- `index_publication_worker_panicking_payload_drop_preserves_exact_selector_and_next_commit`
- `legacy_finish_add_removes_native_selection_after_staging_before_queue_compaction`
- `publication_worker_removes_native_selection_after_staging`
- `legacy_add_and_cold_retry_expose_exact_native_partial_publication`
- `native_python_publication_survives_missing_typescript_sdk_and_cold_refusal_retry`
- `preparation_structured_query_shares_worker_and_cancel_is_immediate`
- `refused_capture_reconciles_resident_view_before_terminal_reply`

The existing engine laws remain independent required evidence for first-request
admission: `deferred_native_python_has_no_probe_digest_or_recipe_before_request`,
`deferred_native_python_cancel_deadline_and_waiters_leave_admission_retryable`,
and `deferred_native_python_first_package_reconfigures_exact_recipe_and_cold_repeat`.
None was executed for this source correction.

The ten native laws previously stopped before actual native execution; fixing
the precondition can reveal additional real image/compiler/publication failures.
Ready requires admitted identity, but it is not itself a successful-publication
claim. The original downstream publication and selector assertions remain the
success authority. Native96's other fixture, Cargo launcher, retained external
document certificate, and transport failures belong to separate reviewed
repairs; this atom does not claim to resolve them.

No tests were removed, ignored, suppressed, or relabelled as a pass. Source
inspection and `git diff --check` are the only completed checks for this atom.
