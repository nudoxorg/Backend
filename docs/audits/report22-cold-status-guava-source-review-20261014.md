# Report 22 cold status and Guava publication source review

Base: `c7cc731f24982a31d23a904279581a1d96273f60`, 2026-10-14.
This is source evidence and a focused recovery projection repair, not native
acceptance of cold startup or Guava indexing. No native build, owner launch,
index operation, or compiler workload ran for this review.

## Cold status

Canonical already contains `39e5e890a9` through merge `e315da1ea2` (#215).
`apps/cli/src/process.rs::run_words` uses the runtime's existing 90-second
`COMMAND_STARTUP_TIMEOUT` once through a `RequestContext`, passes its remaining
budget to `start_locald_cancellable`, and supplies request cancellation. It no
longer cuts cold status off at one second. A checked lease contender waits for
the opening winner under the same deadline. Pending output remains unsent,
with exit 75 only when that bounded wait ends; it is no operation receipt.

The residual report 22 recovery projection still named `nudox health` and
printed the selected authority file path. The focused repair changes the
shared `StartupRecoveryContext` API's third operand from `authority_secret`
to `executable`. Its sole caller selects the running CLI through `current_exe`
and observes an executable file before offering the command. The command
runs `status`; project, endpoint, executable, and workspace operands must be
absolute, lossless UTF-8 without NUL bytes and use the shared POSIX quoting.

`StartupRecoverySelection::uses_default_authority` owns the conventional
credential comparison beside runtime discovery. Custom credential selections
retain only the generic original-command instruction. The shared renderer
never receives or prints a credential path. Conventional retry commands clear
an inherited credential override in a subshell; explicit workspace paths stay
explicit, while default selections restore their captured HOME/XDG anchor
without changing the invoking shell's environment or preparing private state.

The existing CLI recovery replay control now executes an owned capture-only
fixture executable with quotes and shell metacharacters in its path instead
of inventing a `nudox` shell function. It captures actual argv/environment,
then replays the normal option split and read-only discovery in a separate
test process. The controls cover explicit/environment workspace, platform
HOME/XDG defaults, conflicting inherited selections, custom credentials,
non-UTF-8/relative/NUL executable operands, mutation refusal, and unsent JSON.

## Guava retained publication truthfulness

Canonical already contains the relevant typed repairs:

- `66d6503584`: latest authenticated capture outcomes qualify retained rows
  and coverage; capture-only refusal invalidates a reused publication view.
- `e60fa7a955`: captures bind to each package's authenticated source version,
  rather than global workspace root equality.
- `7e657c9623`: removed scope stays historical and explicit semantic version
  freshness is qualified against source scope.
- `8fdde3b57a`: package readiness seeks its typed package prefix so an
  unrelated package refusal cannot change a package-local result.

`ProductSemanticCaptureRecord` is independent of the immutable selected
`ProductSemanticPublicationRecord`. Pending, Failed, Unavailable and Published
outcomes retain the observed source capture and exact prior generation where
applicable. `coverage.rs` folds observed refusal/partial publication to failed
package state and non-complete coverage with its typed reason, including
without embeddings.
A retained complete publication cannot certify a new pending or failed capture.
A capture whose source version differs from the authenticated package frontier
cannot report the package ready. These qualifiers preserve readable retained
declarations and do not alter an older pinned snapshot's independent outcome.

`SemanticAuthority::freshness` compares typed `SelectedGenerationInput` and
`ObservedLatestInput` in the query's exact immutable snapshot. Different native
input digests produce `SemanticVersionFreshness::Historical`; a missing/removed
package or stale source frontier produces `Unverified`, without inventing a
new native input digest. This concerns observed indexing captures, not unknown
filesystem changes that have never been captured. No duplicate Guava product
repair was added in this branch.

## Validation and remaining gates

Passed: Rustfmt parsing/format checks on the three changed Rust files, and
`git diff --check`. All constructor callers were searched; the CLI is the only
`StartupRecoveryContext::new` caller. Existing repairs were checked as ancestors
of the base. Native tests and real cold status/Guava runs are **UNRUN** under the
machine guard; parsing is not runtime proof.

Run under an owned, resource-admitted native gate before claiming acceptance:

- CLI `process::startup_tests`, especially the actual discovery replay and
  pending JSON controls.
- Runtime `retired_contender_waits_for_winner_endpoint_before_reporting_ready`
  and `cancelled_readiness_wait_hands_off_shared_owner_without_killing_it`.
- Local-service `retained_complete_generation_does_not_certify_a_new_pending_or_failed_capture`
  and `published_capture_requires_exact_selected_claim_and_coverage`.
- Local-service `capture_only_refusal_survives_cold_reopen_and_qualifies_retained_indexed_rows`,
  `retained_published_capture_is_correlated_with_exact_package_source_frontier`,
  and `one_package_state_seeks_its_typed_prefix_without_folding_foreign_refusals`.
- Explicit native fixture control
  `native_freshness_requires_current_package_source_even_with_same_input_digest`.
- Real stopped-owner status with original/custom attachments, then the Guava
  observed-change/refused-refresh reproduction. Preserve compiler budgets;
  this review does not prove Guava native checking succeeds.
