# First-party test obligation inventory

The authorized end state is no first-party unit tests. This inventory sequences
unit-law retirements behind independently authored process replacements and
records uncovered obligations; it does not claim that the migration is
complete. The machine-readable ledger is
[`test-obligations.json`](test-obligations.json). It preserves source paths,
function/case names, line spans, source hashes, baseline/current status,
runtime classification, static-extraction uncertainty, and receipt links for
the individual obligations that have actually been replaced.
The source declaration extractor and two-revision regeneration steps are in
[`tools/README.md`](tools/README.md).

After the source comparison below, two more CLI laws were retired behind the
exact-definition process receipt
[`search-page-bound-55551-eb9360-20261116.json`](evidence/search-page-bound-55551-eb9360-20261116.json),
combined with the existing protocol/Rust receipt. The required manifest now has
35 cases. `rust-public-search-page-bound` drains seven authored Rust names
through three pages and compares the first page with the CLI's three-row
global bound; `cli-search-help-page-range` checks the documented search range.
The replaced functions are
`a_global_page_bound_is_not_silently_ignored_by_unpaged_commands` and
`package_graph_help_and_cli_lowering_share_the_engine_page_bound`. The audit
counts and source rows below remain explicitly bound to its two historical
revisions; this later tranche brings the known retirement total to 18 without
claiming that the full source inventory has been rerun at the new HEAD.

## Audit scope and counts

The comparison is `cb92bd655bfc6c6a80f11a1d80dca2d7e2fd4e15` to
`f54c6430cea702d1865795900a98091294f99509`, using a separate worktree at each
revision. It scans first-party `apps/`, `crates/`, `frontends/`, `.config/`, and
`tests/`; vendored and third-party compiler trees, generated snapshots, and
source fixtures are excluded from test-function discovery. Fixture inputs are
listed separately in the ledger.

The source extractor records 12,032 baseline and 12,020 current Rust/Python
test declarations. Current rows contain 11,432 Rust test functions, 585 Python
test functions, and three Rust macro-generated families. It also records 11
Rust parameterized families with 38 explicit `case` attributes, 108 Nushell
assertion/function obligations, seven fuzz oracle targets with 23 corpus seeds,
and all 33 cases in the strictly evaluated Keel required manifest. The host
language and subject-language counts are separate. A path/name classifier
marks 300 Rust, 1,087 Python, 2 JavaScript, 753 TypeScript, 79 CSS, 146 SQL,
and 8,266 mixed/backend subject functions among Rust-hosted tests; these are
review indexes, not semantic labels. CSS, SQL, TypeScript, and Python suites are mostly Rust-hosted,
not test files written in those languages. Static discovery found no JS- or
TS-hosted `test`/`it`/`specify`
declarations in test-named paths; it did find Rust suites exercising those
frontends and real Keel process cases.

The Rust scanner masks nested/line comments and normal/raw strings before
matching test attributes and function bodies. It labels 11,882 current source
functions `not_explicitly_ignored`, 131 explicit ignores `paused`, and three
macro families `unknown`; it does not assert default-feature reachability. The
11 parameterized families and their 38 `case` attributes are separate
unknown-cardinality obligations. Python uses its AST. JavaScript
and TypeScript use a static test-call scan. Nushell assertions and test
functions are line/indent based. Macro expansion, Cargo feature reachability,
runtime filters, test execution, and fuzz campaign coverage were not evaluated.
The ledger labels a child-process API as `process_child`; that alone does not
mean the test launches a product CLI or MCP binary.

The case-definition comparison hashes strict evaluated case JSON with sorted
keys and compact separators. The serializer matches independently recorded
`inventory.definition_sha256` values on exact report matches. Each receipt is
marked `true`, `false`, or `"unknown"` so a reused case ID cannot stand in for
the current definition.

## Excluded upstream test suites

These remain upstream/dependency obligations and are excluded from the
first-party counts above. Their retention follows the pinned dependency trees;
this inventory does not claim they were executed:

- `third_party/tsz`: parser, scanner, lowering, checker/type analysis, solver,
  emitter, LSP, and integration suites under `third_party/tsz/crates/*`.
- `vendor/stylo-native`, `vendor/selectors-native`, and
  `vendor/stylo-derive-native`: Servo/Gecko CSS parser, selector, style engine,
  and derive suites.
- `vendor/syntaqlite-native/syntaqlite/tests` and
  `vendor/syntaqlite-native/syntaqlite-syntax/tests`: upstream SQLite engine
  and syntax suites.
- `vendor/pyrefly_native_1_2/lib/test` and `test_laziness`: Pyrefly conformance
  and laziness suites; vendored typing stubs are fixtures, not Backend tests.
- `vendor/ra_ap_*`: Rust Analyzer project-model and HIR definition test modules
  with their upstream fixture data.
- `vendor/gpui-*` and `vendor/gpui_ce_*`: GPUI platform, component, scheduler,
  and widget test suites; `vendor/accesskit_*` retains its accessibility
  protocol tests.

## Runtime classification

Tests are not classified as integration tests just because they live beneath
`tests/`. The audit recognizes these concrete executable journeys:

- `tests/keel/default.nix` evaluates to 33 reviewed, registered cases: 20 CLI
  protocol rows and one MCP stdio framing row among the larger language and
  owner histories. Passing reports include 58 case/report rows: 29 match the
  current exact definition hash, 27 pass by ID with a different definition
  hash, and two worker reports have no per-case hash. Across the 33 current
  case IDs, 19 have an exact-definition pass, 11 have only ID-level historical
  passes, and three have no passing receipt. The ledger keeps these categories
  separate.
- `apps/cli/tests/passive_health.rs` and
  `apps/cli/tests/saved_index_wait.rs` invoke `CARGO_BIN_EXE_backend-cli`.
- `tests/journeys/tests/css_impact_journey.rs` documents and launches the
  public CLI, MCP, owner, and Stylo worker processes. The listed
  `tests/journeys/tests/` modules that name
  `CARGO_BIN_EXE_backend-journey-{locald,cli,mcp,worker}` launch compiled
  journey processes; they are test shims around the product and should not be
  confused with official release binaries.

The ledger retains `in_process` and `process_child` as source observations for
all other functions. A test that forks itself, launches a compiler, or uses a
test helper remains unclassified as a product-process journey until its child
target is reviewed.

## Verified retirements and baseline changes

Git source comparison found 15 baseline-to-current function deletions linked
to process-case IDs, plus one test rewritten in place. A
separate TypeScript owner test was added and retired between the two baseline
commits, so it is included as a historical row even though it is absent from
the endpoint-only diff. Together these are the 16 known retirements. The case
definition hashes show that five replacement mappings have a passing current-
definition receipt; eleven have only historical same-ID passes and need a
fresh exact-definition receipt before being treated as current replacements.

| Retired source obligation | Public process row and source-bound evidence | Current-definition check |
|---|---|---|
| CLI `an_unknown_command_names_the_nearest_one_it_knows`, `an_unknown_option_names_the_options_the_command_takes`, `a_missing_operand_prints_the_exact_usage_line`, `a_missing_operand_through_a_cli_alias_uses_the_entered_installed_command`, `a_page_bound_outside_its_range_is_refused_with_the_value` | `cli-nearest-command`, `cli-unknown-option`, `cli-missing-command-operand`, `cli-document-alias-missing-operand`, `cli-invalid-page-bound`; [`product-linux-189f-20261116.json`](evidence/product-linux-189f-20261116.json), official source `189fca6cbbdcf9eb1406dfc0c5ca744b35c43da3` | All five passed by ID in 189f and the later protocol report. Only the document alias has a matching current definition hash; the other four are ID-only historical passes. |
| CLI `help_is_grouped_by_domain_and_names_every_domain`, `passive_connection_mode_is_limited_to_health_and_status`, `add_defaults_to_interactive_and_advertises_the_background_option` | `cli-root-help`, both passive connect-only cases, `cli-add-help` plus the default interactive receipt in `rust-call-history`; [`protocol-rust-linux-55551-f3e-20261116.json`](evidence/protocol-rust-linux-55551-f3e-20261116.json), official source `55551f420a870ddf55e93ed0117bbabc815ec612` | All three have a passing report for the current exact definition. The protocol report selected 20 CLI cases plus Rust history (21 total); MCP framing is a separate case. |
| Stylo `native_media_selectors_declarations_and_dependencies`, `observer_proves_root_class_and_duplicate_custom_property_sites`, `class_name_site_excludes_skipped_comments_and_decodes_escapes`, `observer_discards_rejected_rules_and_uses_inserted_rule_ordinals`, `utf8_bom_is_removed_before_stylo_parsing`, `native_location_reports_utf16_column_without_byte_guessing`, `imports_are_recorded_with_their_qualifiers_and_never_loaded` | Corresponding independently authored steps in `css-source-observer`; [`native-linux-20261116.json`](evidence/native-linux-20261116.json), source worker build `c5f063` | All 17 worker steps passed by ID, but none has a matching current definition hash. The original worker report lacks definition inventory; later report IDs have changed definitions. |
| Historical TypeScript owner `native_ts_external_target_does_not_claim_source_observed_at_capture` | `typescript-external-source-policy`; [`product-linux-189f-20261116.json`](evidence/product-linux-189f-20261116.json), official source `189fca6cbbdcf9eb1406dfc0c5ca744b35c43da3` | Exact current definition match in both 189f and later 55551 reports. |

`crates/present/tests.rs::mcp_all_record_details_use_selectable_bounded_text_without_signature_bodies`
is not a retirement: Git shows it was rewritten at the same location as
`mcp_standard_records_share_cli_text_while_summary_stays_bounded`. Keep the
new in-process law. The first experimental Keel report also includes Rust
fanout, Python history, and JavaScript history rows. Those three case rows have
no passing receipt and remain gaps; they are not additional unit-test
retirements. `rust-reference-sites` passed by ID in the 189f report, but that
case definition differs from the current manifest and has no later exact-
definition receipt.

## Coverage matrix and next critical seams

| Surface | Existing process evidence | Remaining obligation / severity |
|---|---|---|
| CLI and MCP grammar, help, passive behavior, and typed replies | The strict manifest contains 20 CLI protocol cases plus one MCP stdio framing case. The protocol report selected all 20 CLI cases plus Rust history (21 cases total); the MCP framing row passed separately in 189f with an exact current-definition match. | **High:** exhaustive registry-to-argv coverage, Cargo-target selection, and the full help/lowering cross-product remain in-process. Do not retire the rest of `apps/cli/src/tests.rs` from the three current-definition help/passive/add replacements. |
| Source, document, reference, and graph pagination | `rust-reference-sites` passed with 15 exact authored reference spans across the MCP continuation chain in `product-linux-189f`; `rust-mcp-cursor-lifecycle` passed against source `55551` for moved-root refusal, restart rejection, and fresh-query recovery. | **Critical:** dense source/document/token/outline/package-graph budgets, copied selection continuations, duplicate IDs across pages, UTF-8/CRLF byte reconstruction, and atomic refusal after changed basis are not a single covered law. Keep `apps/mcp/src/jsonrpc/tests/{semantic_source_pages,source_continuation,document_section_pages,token_outline,record_budget_pages,package_graph_budget}.rs` and CLI admission suites until each family has a dense process row plus negative control. |
| Owner crash, cancellation, and ambiguous delivery | The cursor history keeps one MCP server alive over edit/reindex and then proves a server-local handle fails after restart. Existing CLI child-process tests and selected `tests/journeys` restart cases cover some lifecycle transitions. | **Critical:** there is no full process receipt here for crash after owner mutation but before reply, cancellation/half-close, retry deduplication, shared durable operation identity, original deadline preservation, or replacement-owner authentication. Keep the owner recovery, index overlap, stdio cancellation, and transport boundary suites. |
| Rust/Python/JavaScript/TypeScript compiler evidence | Rust fanout process history is unqualified in the retained 189f report; Python/JavaScript history failed that report's temporal readiness parity. The later JS deletion/restart case and TypeScript external-source case passed, but are narrower obligations. | **Critical:** compiler provenance, source completeness, selection stability, missing/partial evidence, broken-tool refusal, and exact multi-file joins need per-language public process cases. Keep the in-process admission and provenance suites; do not promote a filename-only search result into compiler evidence. |
| CSS and SQL | The CSS worker step matrix passed its worker receipt; `owner-css-sql-history` passed in the exact 11bcc package-account report. The SQLite syntactic worker case has independent process evidence. | **High:** the release preview account receipt is package-specific. It does not replace the roughly 146 Rust-hosted test functions classified under the SQL subject index, nor prove SQL binding, catalog completeness, loader refusal, or owner-level source freshness for arbitrary databases. Keep remaining SQL contract and owner tests. |
| Fuzz and parameterized families | Ledger records seven fuzz oracle targets, 23 corpus seeds, 11 parameterized families, and three macro-generated families. | **High:** none of these counts equals executed case coverage. Expand/check in the required corpus and boundary outputs, then retain the fuzz campaign and parameterized law until a process-level semantic equivalent is reviewed. |

The most useful migration sequence is: first add dense source/document page
chains with exact reconstruction and moved-basis refusal; next add owner
crash/cancel/retry histories with durable operation receipts; then add
per-language compiler-provenance cases with broken-tool and missing-evidence
controls. Each row should be authored from public behavior and carry the exact
source revision plus report that passed it before a corresponding unit family
is retired.
