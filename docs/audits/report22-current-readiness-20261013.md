# Report 22: current execution and repair checkpoint

## Follow-up: 2026-10-14, 16:53 UTC

**Product readiness remains unproved.** Root merged [PR 258](https://dev.nudox.org/git/Nudox/Backend/pulls/258), documenting borrowed forest/leaf ownership and bounded compiler diagnostic facts, and [PR 260](https://dev.nudox.org/git/Nudox/Backend/pulls/260), fixing installer umask leakage and both candidate library pins. Canonical before this documentation checkpoint is `81546bc1ae797e628cf3188bde33a294ebf7bd8e`. Coworker PRs 249–255 and 257 were fetched and preserved, including whole Python/Rust corpus cases, namespace-independent tied search ordering, bounded surface parallelism and exact-artifact CI evidence. Their snapshots are not Root's new native project acceptance.

### Installer regression recovery

The first current packaging suite executed 257 tests and reported seven failures and 23 errors. A stale candidate Mac pin caused bootstrap failures; process-wide umask leakage in current installers and two calls into immutable historical fixtures contaminated later SDK packaging controls. Linux and Mac `install()` now restore the caller's prior umask on success and failure. Tests scope calls into the historical fixture without changing its published bytes.

Root caught a second stale pin after reviewing the first repair: the modified Linux installer still had its old candidate checksum. The follow-up binds both current source libraries and tests their distinction from historical R2 identity. On exact source `25c7019afc532cdb5800bb9a0a6bf89711ff21c7`, **all 260 packaging tests passed** under CPython 3.12.14 in 102.039 seconds. Root verified the full raw-log SHA-256 `f88040cb94fdbc5b70adcb42df85d85f121f16b5b24933949c19f349985e656c`, 260 unique test names, 256 inline plus four separate successful statuses, and the terminal success summary. The narrower runs and earlier 259-test run are not added to that total. Root composed the exact seven-file postimages in two atomic commits, ending at `ec985c4b94dd91c8f0923f345591e10924586b76`, before merging PR 260.

This is packaging-harness proof, using test archives and fixtures; no fresh native installer activation or current product journey ran. The generic Linux source has blank release pins and still refuses to claim the old R2 release. A release must bind its actual manifest and recompute its installer/bootstrap identities. Historical public scripts, channels and binary assets were not changed or promoted.

Anonymous public readback found the stable API catalog still marked `legacy`; the Forgejo diagnostic channel remains `production_ready:false`, using historical Linux c001 and Mac a659 assets. Small public scripts and manifests match their declared hashes. Forgejo archive HEADs returned 200; corresponding GitHub installer/archive HEADs returned 404. Archive bodies were not fetched, so HEAD success is not native installation proof. Readback receipt SHA-256: `a2e826be7ea4d87c942ff9528dd4b236c107e8cb2688d7ef12e860d18073608d`.

### Actual new MCP build and incomplete native run

Exact MCP source `3081b508198e8651ccce45e0cb529e43555cd0a5`, tree `8757fef18dd5930d4d0c35778aa79ae36ff62ad3`, compiled successfully on ILO: **13.456 seconds, zero errors, 881 warnings across its dependency graph**. Its retained test ELF is 168,583,376 bytes, SHA-256 `76763331f7b754d2b40f68a42d982d32c15f811f7902baa9dba3a48088baa7b1`. The actual unfiltered inventory contains 257 unique tests: all original 254 plus three bounded-card controls.

The serial suite was interrupted by the live disk-reserve guard: **196 passed, five failed, one started without a terminal status, and 55 were not started**. There is no terminal full-suite summary and no final panic bodies. Root independently verified the raw Cargo stream, inventory, 201 completed statuses, source/tree and closed scope; raw stdout SHA-256 is `a9d0bc149f9e089085b99632563103a3568862722545fc8d026f99b6d9011ced`. Root's partial-verification receipt SHA-256 is `694254815b9536efb5d9e2031f7160a36daec0436572fcd4fe8b9b36eee86c11`.

Failures were `package_graph_budget::{actual_caller_id_is_admitted_before_selection_and_resumes_refuse_atomically, default_graph_page_shrinks_before_admission_and_exact_copy_calls_cover_all_edges, graph_json_alternative_keeps_resumed_frontier_cancel_authority_and_catalog, minimum_graph_refusal_offers_only_proven_local_cli_json_and_keeps_the_input}` and `record_budget_pages::whole_response_budget_keeps_every_native_catalog_coordinate_through_copyable_continuations`. The scope reaped through ECHILD, its cgroup was deleted and its managed destination permit was released. A later retained-ELF phase is prepared to rerun these five controls and then all 257 without invoking Cargo, but has not executed at this checkpoint.

The implemented catalog is now bounded and versioned: a small closed index links to complete per-handler schema cards with exact UTF-8 ranges, digest-fenced continuation URIs, actual caller-ID accounting and truthful fragment MIME. Root also reviewed two concrete source repairs: shortened previews retain the executable `structuredContent.continuation_call` hint, and proven graph JSON advice uses the dedicated CLI Surface route rather than an impossible named-registry lookup. They are published on [PR 227](https://dev.nudox.org/git/Nudox/Backend/pulls/227) at `12ee1e56ded14852e6fc793ae476e8447419bbba`. Native execution of these latest repairs remains **UNRUN**, and PR 227 remains open. No budgets, issued cursors, complete rows or test names were removed.

### Ordinary desktop build is proved; repaired interaction is not

The normal Linux desktop at `3183000a7788d20c2d4bef362a68869a24500cac`, tree `5f07dbec4d251e06703ad3c45804b7be3e1d82f2`, **built successfully in 357.789 seconds**, with zero compiler errors and 1,684 warnings across the build graph. It produced the ordinary application ELF, 726,409,696 bytes, SHA-256 `af7d79ab988dc2cbe2991a2e8deda2a210d6d3184c03f0c6f6a0f48b5734c183`; zero tests ran in that build.

Root checked the complete raw Cargo stream, source/tool witnesses, actual executable device/inode/hash, application artifact role and closed/released scope. Root's completed build-verification receipt SHA-256 is `7c20776bbc8e12585c49b9a9eab7fa3ce1469c298032827fa7003f177e830b86`. This supersedes the earlier build-pending statement. **Current startup pixels, zoom/motion dispatch, resize/focus, navigation, real Add/publication and offline/cold restart remain UNRUN.** Sol resumed inspection and preparation of the existing native observer; preparation and old black screenshots are not current interaction proof.

### Compiler compositions and resource gate

The complete reviewed Python capture/profile candidate is `62e9af8315a88d0377534c8395b205e707349374`, tree `ef735814fdf4059af3a86d7208331389ebecbfe4`. It preserves all 17 foundational capture atoms plus the full profile companion. Captured minors select source/fact/reuse keys before ingest and persist in bounded rows; the 38-file composition preserves the coworker's Go source-site fix. Native controls remain unrun, and the genuine ordinary 3.12/3.14 gate still needs a verified ILO CPython 3.12 installation. A configured 3.14 tool cannot substitute for that gate.

The reviewed TypeScript PR 234/237 composition remains `d7d2380f9a99dcb40ff62abb7ac58aa8aa063225`; its native parity, memory and real-project publication gates remain open. First-index prototypes stay separate. Neither source composition establishes successful large application indexing.

The MCP stop occurred when free disk fell 169,803,776 bytes below its 99-GiB gate: sixteen GiB destination floor, eighty GiB measured CI reserve and a three-GiB compilation forecast. The planned retained-ELF-only rerun has a justified one-GiB forecast and a 97-GiB gate, while preserving the destination/CI reserves, native/GUI/combined ceilings, retirement carry, fresh v6 census and managed permit. Subsequent disk samples also fell below that gate. No duplicate actor or unguarded native workload was started to bypass it. Owned stale cache retirement is being reviewed; unrelated CI workloads, selected executable proofs and current warm targets are preserved.

Root also ran 35 Python CI-harness controls on exact canonical `83550ee8d6e914fe7192f443dc08903a2f065a39`: surface 21, selection five, quarantine four, and tier routing five, all passed. These are harness controls, not native project acceptance. PR 258 has complete source/postimage review and diff checks; no new Clippy or post-edit native warning count is claimed.

Current matched CLI/MCP/locald binaries, hundreds of fresh large-project flows, useful configured embedding search, repaired GUI captures, Mac bundle/Homebrew delivery and a newly promoted installer remain open product gates.


## Follow-up: 2026-10-14, 12:10 UTC

**Product readiness is not established.** Canonical includes coworker [PR 244](https://dev.nudox.org/git/Nudox/Backend/pulls/244), fixing lost Go source sites with a pinned xxhash corpus case, and Root-merged [PR 246](https://dev.nudox.org/git/Nudox/Backend/pulls/246), removing compiler-proved redundant MCP overlap imports. Its exact head before this documentation checkpoint is `d5f4d91959cd88053f0f6ec671e79569eb9be521`. The user's existing Nix edit remains unchanged at SHA-256 `9cee064afabfacafd6b59a041993f6a0f98f382cf37866caf1d9e12225acff63`.

This section supersedes earlier pending statements only where actual new execution is recorded. Source repairs, library test execution, normal application builds, screenshots, installer delivery and fresh-project acceptance remain distinct.

### Actual current MCP execution on ILO

| Exact source / phase | Actual result | Practical boundary |
| --- | --- | --- |
| `8134dd4c22ecb09d08d96f04fedf64ea2b89200b`, MCP library no-run | Cargo reached Rust compilation and failed after 38.30 seconds on three moved-value errors in shared semantic-history schema construction. | Zero tests executed. Root repaired the helper to borrow reusable fields in `b649b55633539bd2327546965f59ef6860852ee9`. |
| `b649b55633539bd2327546965f59ef6860852ee9`, MCP library no-run | Failed after 194.08 seconds on two outdated fixture constructors: `PackageReference::local` and a missing `SessionEndpoint.cli_context`. The actual Cargo stream contains 879 warnings, two errors and two failure notes. | Zero tests executed. Root repaired only the fixture constructors in `1eb56193f086a6096766b4af1797d887d1388085`. The 879 warnings cover the dependency graph; they are not all MCP warnings. |
| `1eb56193f086a6096766b4af1797d887d1388085`, MCP library no-run | **Passed in 9.85 seconds**; retained one genuine MCP test ELF and listed 254 tests. | The build alone executed zero tests. No normal CLI/MCP/locald application trio was produced. |
| Same source and ELF, nine targeted controls | **Six passed, three failed.** | Failures exposed page-envelope budget admission and stale cancellation/private-route expectations. Later source repairs cannot inherit these results. |
| Same source and ELF, full serial library suite | **254 executed: 242 passed, 12 failed, zero ignored, zero filtered, 38.85 seconds.** Root independently checked every unique raw test status, the exact source/tree/ELF, receipt hashes and closed managed scope. | A failing library suite does not establish working fresh-project CLI/MCP, GUI or installer acceptance. No tests were removed or skipped. |

Exact tested tree: `ed2d8449969ec13edec45d00896d05c9a4bbdc77`. Test ELF: 168,152,880 bytes, SHA-256 `95a4c3059846980a1fc760b88d855d5f45da1025be58d699fdade49d81ad0d25`. The full run reused this immutable artifact without Cargo. Root's independent raw-verification receipt is `current-canonical-lock-repair-20261014/1eb-full254/fresh-guard-full254-01/root-independent-verification.json`, SHA-256 `2247cecf435f78f64b61b2dafacb3d01393317d46993a60042df990c4c25da4a`.

Raw full-suite stdout SHA-256: `bc43e11cad4b307b64af77e89a637cd4d92658b7b4f2a13e2dfae4583dbfab1b`; outer receipt `2a11c9bc97f9ee382c77c464101b77ac5472e67c7059143f05cae3c25b5e543d`; driver `01072e6dff3ed12c6e038e9ef9ec4137f751ce2017faba29c76f735b6f34d574`; closure `a7382fbf9ba5aca93cf98ad68aed5a43ec2f69097712e199c8481da2f2acdcbd`. The test-only phase grew by 2,732,032 bytes under its one-GiB forecast, reaped through ECHILD, removed its exact cgroup and released the managed destination permit.

### Twelve failures remain visible

The failed controls are `every_registry_row_is_reachable_as_exactly_one_tool`; three `named_call` controls (`catalog_native_json_graph_cursor_reaches_lowering_and_copyable_direct_or_compact_call`, `compact_catalog_reads_preserve_typed_operands_refusals_and_semantic_data`, `compact_discovery_has_one_closed_registry_derived_target_set`); four `package_graph_budget` controls (`actual_caller_id_is_admitted_before_selection_and_resumes_refuse_atomically`, `cancellation_after_native_graph_reply_precedes_success_and_oversized_fallback`, `graph_json_alternative_keeps_resumed_frontier_cancel_authority_and_catalog`, `minimum_graph_refusal_offers_only_proven_local_cli_json_and_keeps_the_input`); `record_budget_pages::whole_response_budget_keeps_every_native_catalog_coordinate_through_copyable_continuations`; two `semantic_source_pages` controls (`discovery_complete_coordinate_union_is_monotonic_through_direct_compact_and_raw_calls`, `raw_source_and_uri_resources_reconstruct_the_entire_capture_with_exact_selected_ranges`); and `tool_catalog_names_callable_routes_without_advertising_unready_surfaces`.

Source triage distinguishes product behavior from invalid fixtures:

- Page preparation measures the duplicated readable preview before owner-page selection, while final emission already fits that preview. Root's reviewed child `e30eb0f198` shares that existing fitting step, preserving complete structured coordinates, native cursor, issued limit, request ID and transport accounting. It does not raise budgets or truncate owner data. Its native rerun is **UNRUN**.
- Cancellation correctly returned JSON-RPC `-32800`, with the actual request ID and explicit cancellation reason. Old assertions expected a legacy tool fault. Raw `backend.surface` remains intentionally unavailable through compact `backend.call`; tests now assert that refusal and exercise genuine registered source/version calls instead. Root reviewed and composed the assertion-preserving repairs as `b5dcab0e69b80bfe88d298d3a70689c9f87893ac`, now published to [PR 227](https://dev.nudox.org/git/Nudox/Backend/pulls/227). Native rerun is **UNRUN**.
- The minimum-row pressure fixture exceeded the package URL grammar before reaching response admission. Its source repair uses the actual maximum valid PURL and requirement bounds. A cancelled-page fixture omitted the required unknown-knowledge state; it now preserves that admitted state and the original cursor/authority checks. These test-only repairs are source-reviewed at `968f15db07`; native rerun is **UNRUN**.
- The catalog resource concatenates all input schemas into one Markdown response. Three failures lacked resource contents; a bounded catalog with complete per-tool schema resources is being implemented. Exact new catalog/card measurements and native acceptance are **UNRUN**. Registry-list and legacy semantic-array expectations are also being reconciled with the actual closed, paged protocol.

PR 227 remains open while these native gates are repaired. No successful full-suite result is claimed from source review. Root separately ran the coworker's Python surface-harness controls on exact `debee8269852adad25355d26b4c02c379bd55c44`: **18 passed**, zero failures, 0.225 seconds. These are harness controls, not a native project corpus. PR 246's single-file warning cleanup preserves all overlap assertions; it has source/diff checks, but no new native or Clippy result.

### Compiler and GUI gates

Python environment capture has been substantially repaired in source: independent CPython provenance, held directory identity, Windows component spelling, ABI-tagged compiled-module and package-initializer shadows, exact installed `.pyi` ownership, streaming bounded capsule admission, and retarget witnesses. Latest source `8aedbaa4d9e63961e5ef5b96656d45e931583ee1` adds canonical stdlib/BINDIR confinement and capsule v3. Its native controls remain **UNRUN**, and adversarial review found a remaining post-I/O cancellation-precedence repair. The earlier unsafe checkpoints remain recorded rather than silently credited.

A separate default-version blocker is concrete: vocabulary supports Python 3.10–3.14, but ordinary ingest currently selects 3.14 for all Python input, while captured environments require their actual version. Host policy also selects the first candidate without matching an ordinary project venv. A typed project-wide policy companion is being implemented before source deltas, profile keys and persisted observations are formed. Ordinary Ubuntu Python 3.12 `nudox add .` acceptance is **not established** by the existing 3.14 dependency setup.

TypeScript [PR 234](https://dev.nudox.org/git/Nudox/Backend/pulls/234) and [PR 237](https://dev.nudox.org/git/Nudox/Backend/pulls/237) retain their reviewed parentage/call-endpoint and exact worker-lane cache work. Their current native parity, memory and real-project publication gates remain unrun. A further source-reviewed first-index resolution-consensus prototype at `fc489d7e359a3da084a7372d8fb9323bd9a193f4` retains exact per-outcome authority and cancellation instead of repeating generic import resolution; it is not merged and has no native performance or indexing result.

The repaired ordinary GUI remains frozen at `3183000a7788d20c2d4bef362a68869a24500cac`, tree `5f07dbec4d251e06703ad3c45804b7be3e1d82f2`. Its current build and pixels remain unrun at this checkpoint; the next genuine automated journey is being admitted on ILO. A Sol GUI execution retry failed at model service capacity before any process. The earlier real black startup captures, delayed first paint and missed zoom dispatches remain failures, not proof supplied by fixture screenshots. Root will inspect new actual captures and input traces when available.

### Resource recovery and admission

Root independently verified removal of 2,175 old owned `rlib`/`rmeta` files, reclaiming **8,949,362,688 allocated bytes**, while retaining all nineteen selected executable proofs (3,122,542,512 bytes), receipts, sources and build identities. Only regular, root-owned, single-link, nonsymlink objects from the closed old target were removed under the managed locks. No unrelated CI or user files were removed. Retirement accounting retains the complete 12,191,573,000-byte carry; cache cleanup does not erase charged work.

Retirement manifest SHA-256: `d09d3a03f946e5513d4a321b69c3ba22540d58e6127718823474f5387e8deb9c`; completion `558c03808db027851cdba7225e4cb983c9fe682a468ffe5789e08be2f722ad13`. Process cwd/executable/fd/maps/argv checks preceded deletion. The environment scan was incomplete before deletion; the subsequent complete kernel-aware audit passed. That later scan must not be described as a pre-deletion guarantee.

Fresh v6 fleet guards, destination managed permits, sixteen reachable-fleet slots, eight ILO slots, at most four Cargo jobs, eight-GiB destination memory, sixteen-GiB free disk, the measured eighty-GiB CI reserve and phase-specific additive forecasts remain required. Native/GUI/combined footprint ceilings remain 80/36/116 GiB. The full MCP suite ran with kernel limits of sixteen GiB, zero swap and 1,024 tasks. Offline Mac exclusion remains explicitly authorized only for ILO and is rechecked; local heavy builds are not admitted through that exception.

The public installer channel is still diagnostic and `production_ready:false`, with historical Linux c001 and Mac a659 assets. No new matched application trio, fresh installer pass, investor Mac bundle, Homebrew acceptance, useful configured embedding search, complete language coverage or fresh large-project CLI/MCP/GUI acceptance is asserted here. These are the next product gates, rather than claims inferred from source or library tests.


## Follow-up: 2026-10-14, 08:15 UTC

The isolated ILO resolver experiment **completed successfully** on exact `9658f64a8d670341ef1737cb7059c9a26e4bc24d`: locked/offline metadata first refused the stale lock with exit 101; offline resolution against the retained lock and then locked/offline validation both exited 0. Those two successful metadata outputs are byte-identical, with 1,548 package records and 1,548 resolve nodes. Root verified the raw command-output hashes, lock snapshots, actual scope ECHILD/cgroup deletion and released permit. Only the `backend-library` dependency label changed from `base64` to `base64 0.22.1`; **all 1,577 lock package identities, versions, sources and checksums are unchanged**. The footprint delta was 17,260,544 bytes under the 128 MiB phase cap. No Rust build or tests ran in this experiment.

Receipts and raw metadata are retained under `current-canonical-lock-repair-20261014/resolver-packet-02/actual-run-02`; final verification SHA-256 is `bedf25efd478453c3c4959dece751a6e65da196cd004c9db27bc284a78091d18`, outer receipt `f67954644a906e86320733c9fe1d92ed19a5720bf676b3a2a7dc023885b4cce2`, driver receipt `cd47d4fbe97a31f224ad38290f3871df52496f8503365b94a0b40862d81be863`. Earlier pre-admission timeouts and the failed native MCP build remain preserved.

Root committed the exact generated one-line lock repair as `7d76b97c1675158aa16821a00166942611bb9e1c`, then composed canonical `611e96d3c1` into current [PR 227](https://dev.nudox.org/git/Nudox/Backend/pulls/227) head `8134dd4c22ecb09d08d96f04fedf64ea2b89200b`, tree `1ffc757cb7326ca05f1dc95920171f19a87fbc1f`. All thirteen subsequent coworker/root CI, Nix-helper and documentation postimages are preserved, with no source deletions. Current locked MCP controls, the matched CLI/MCP/locald build and actual project acceptance remain **UNRUN**. The separately reviewed GUI `3183000a` has no `backend-library` base64 dependency in either manifest or lock, so this MCP lock repair was not applied to its frozen source. Its normal build and current pixels remain unrun; the earlier GUI23 wait expired before any permit or build.

The first Python installed-environment checkpoint `659be247a08f16aab772e822112ddb1b46a9aaf2` is **BLOCKED in source review**, not merged. Luna's full nineteen-file audit found automatic execution of a project-controlled venv interpreter before trusted provenance, incompatible Windows wire separators, case-sensitive venv membership on case-insensitive volumes, missed uppercase compiled-extension shadows, and late worker role-size admission. Root's source reading independently confirms the late size admission and full-wire-plus-payload allocation. Repair child `ff9dd1fafc203bb858bb64207504ef9621e26a60` moves worker role checks before snapshot creation; it is source-present and **UNRUN**, with the interpreter, path and streaming repairs still open. Successful ordinary HTTPie/SearXNG dependency installation continues to be separate from checker consumption.

Canonical coworker PRs 242/243 strengthen the bounded tier runner. Root read their seven-file diff and ran `test_tiers.py` on exact `611e96d3c1`: **3 passed**, zero failures. These verify tier dispatch/failure boundaries; no native tier, product binary or corpus is inferred from them. Sol 6.1 workers encountered service-capacity failures, so root integrated the actual lock fix personally and Luna is taking the guarded native execution turn. The user Nix edit remains unchanged. No current installer, matched product, GUI, semantic-search or production-readiness claim is made.

## Follow-up: 2026-10-14, 06:33 UTC

Canonical now includes the reviewed installer-guide correction through PR 239 (`1a454e41a6`) and coworker PRs 233 and 238. Those coworker changes strengthen background-index resumption and public wait-option admission checks. Root ran the Python surface admission suite on exact `9a8367751c`: **11 passed**, zero failures. This is harness admission validation; the new current-binary resumption journey and wait-option subprocess cases remain **UNRUN** here.

The first actual managed MCP no-run build on ILO at `79ee0a714aca65da7f649e158414ba40c10847a9`, tree `8af9b46f4a9b60dd33632fd64624c5e6dfdb7483`, failed with exit 101 after 1.08 seconds: Cargo refused to update the committed lockfile under `--locked`. It produced **zero test artifacts and ran zero tests**, including none of the nine selected MCP controls. Root independently verified the eight exported evidence hashes, clean source afterward, ECHILD/cgroup closure and permit release. The receipt is `current-trio-45c-b4/current79-mcp06-actual-run-01/root-locked-build-verification.json`, SHA-256 `2074eb766d2f29919c7cc61bac1096fe61c64a76d53d1e4790bf879fae33f928`. Earlier successful prepermit checks do not constitute a successful native build.

The lock repair is isolated at `9658f64a8d670341ef1737cb7059c9a26e4bc24d`, tree `eb2444088e529499ef2784cc0c01bc4c02b82207`, composing exact canonical `1ffec523e9` with reviewed PR 227. Root checked all 18,248 unchanged canonical entries, no canonical deletions, the automatic merged postimages and the sole content-conflict resolution in local-service `index.rs`. The candidate retains the merged lockfile, including the expected `base64` edge from PR 227, and will record genuine offline metadata before/repair/locked-after results. **No resolver success or resolver-generated dependency delta is claimed yet.** Canonical has since advanced through PRs 233, 238 and 239; that later source composition remains distinct from this exact candidate.

[PR 237](https://dev.nudox.org/git/Nudox/Backend/pulls/237) publishes the native TypeScript worker-lane cache candidate. Root personally reviewed all twelve changed paths at code parent `bf40fcd895be01e87119afebb80724a6cda5d60a`; independent Sol and Luna reviews are also source-clear after fixing the typed lane error and selected-program observation attribution. The compiler still re-admits the fresh complete input forest, but retains exact program owners between attempts and narrows bind reuse to actual parse/bind inputs while rebuilding checked authority for semantic changes. The docs-only publication head is `cf4f2e39226ed63edf76dd198c80d12aad6a32f0`. **Native compilation, parity tests, peak/idle RSS, and Guava/ASP/GymBroApp first-import recovery remain UNRUN.** This candidate has not been merged.

Root anonymously downloaded the named current Forgejo R2 shell launcher: HTTP 200, 5,845 bytes, SHA-256 `d837c1fc8130285fe60d1dc44ef96f93fa2177f18bdb2c1db031eab7fc28a8ac`, byte-identical to current source. PR 239 removes the mirror guide's obsolete mutable alias and points to that immutable launcher. Independent small-asset readbacks match the bootstrap/channel/platform scripts; large archives were checked by HEAD only. **No fresh installer execution was performed.** The channel remains `production_ready:false`, mapping to historical Linux c001 and Mac a659 binaries, rather than these new source candidates. Delivery identity is separate from whole-project indexing and installer acceptance.

The prepared GUI normal-build phase uses reviewed `3183000a`, with the older `3c65` source archive and `3ee0` executable retained. Its prepermit checks are actual, but no new normal build, current pixels, native input or publication result is included in this section. The Python installed-dependency capsule is still an implementation worktree; successful HTTPie/SearXNG environment setup has not yet established checker consumption. Current matched CLI/MCP/locald product acceptance, remote index ingestion and GUI journeys remain open. No new production release is claimed.

## Follow-up: 2026-10-14, 05:10 UTC

**Product readiness remains unproved.** Root fetched the coworker's PRs 231/232, reviewed their actual diff, and personally merged PR 225 into canonical `3aa8462a1a45f70006264db74ca3e8cd97d9e1c7`, tree `c9bd0a3248476fab277741773527574885cabe81`. The twelve Go repair postimages match the tested b4 source; every other canonical entry is preserved. The preexisting Nix edit remains untouched, SHA-256 `9cee064afabfacafd6b59a041993f6a0f98f382cf37866caf1d9e12225acff63`.

This section supersedes pending statements below only where new execution is recorded. Source review, SDK/native tests, dependency setup, screenshots, and fresh product acceptance remain separate evidence.

### New actual execution

| Exact source / run | Verified result | Limit of the result |
| --- | --- | --- |
| b4 `b4b0dbea34b2f522da8e60daefe84abff0ffd60a`, engine continuation 11 | The **engine library no-run build passed**, then three exact controls passed: complete sibling-package reservation from a tiny primary file, reused-signature carrier reservation, and borrowed-owner lookup with absent/repeated owners. Root independently verified raw Cargo artifact/profile binding, libtest output, unchanged source/tools, kernel drain and permit release. | 1,010 tests were listed; only three were executed, with 1,009 filtered per invocation. The prior interrupted 91 integration artifacts remain unexecuted. No full engine suite, Clippy, or current Caddy product pass is inferred. Go SDK 18/18 was separately verified earlier. |
| Desktop `3c65d18a6462257b1f18e2feb1b663e13c8281f1`, normal build 20 | Ordinary Linux GUI build succeeded in 365.997 seconds. Retained executable: 726,117,880 bytes, SHA-256 `3ee0b926f0a42e70d79b2d0b11d1659b1fc58917b51a97a2e2fef2b206c48699`. | Its library test build failed on two missing test imports. Selected Ask/owner tests did not execute. The named public-facade correction is reviewed source, not a rerun. |
| Same desktop executable, native journey 22 | Real window, native input, accessibility extents, frame trace and thirteen distinct capture paths were retained. Root inspected startup, resize, zoom/reset and Settings images. The journey **failed**. | Startup captures at 0.5, 1, 3 and 10 seconds were black; first presentation followed mouse input at 15,775.784 ms. Five zoom chords produced only two zoom actions and 125%, not 200%. The move exposed an initial 2px X11 origin mismatch. Full motion, Add and publication remained unrun. |
| Ordinary HTTPie dependency setup on ILO | Genuine CPython 3.14.6 installed 72 distributions; HTTPie 3.2.4, requests 2.34.2 and pytest 9.1.1 imported. All 133 original source/config files were preserved; 78 generated build/lib Python files remain separately recorded. Scope closed and released its permit. | Setup only. Generated files require explicit captured-membership disposition. No Nudox indexing or dependency consumption was proved. |
| Ordinary SearXNG dependency setup on ILO | All twelve setup/import commands passed; 117 distributions installed, genuine searx/Flask/Sphinx/basedpyright imports succeeded. All 407 original source/config files were preserved, with no generated Python outside the environment. Scope closed and released its permit. | Setup only. Installed dependencies do not establish native semantic coverage or publication. Paperless dependency setup remains deferred. |

The new engine receipts are retained under `b4-engine07-root-raw/native11-minimal-export/`: outer SHA-256 `5f860023b2657f0c225c9fad21a67737b0f76adf13ca0a372be0124d33c4514c`, driver `ceef33408d1cc583f982fa144e678b303a9b28dd265888d7a2be5c595967f0d2`, and Cargo JSON `e53dff335455e98c85f4110ee2ac59b45bd2d06dec1dd1c5422d879ef034a96e`. The exact engine test ELF is bound at 331,297,384 bytes, SHA-256 `b80380cfac86e60725344efea2fe4a82eb8e412c262edeb807bf78dadffb9b6f`.

### Repairs reviewed, awaiting native execution

The GUI composition is frozen at `3183000a7788d20c2d4bef362a68869a24500cac`, tree `5f07dbec4d251e06703ad3c45804b7be3e1d82f2`, on current canonical 3aa. Root independently bound 111 reviewed paths: 110 exact prior atom postimages and one expected combined lockfile; all twelve Go postimages are exact. Coworker publication assertions and dependency/test fixes remain preserved.

The source repairs address initial scene submission, buffered X11 Map/Expose draining, actual native origin measurement, truthful WGPU presentation results, global zoom during drawer transitions, immutable graph-visit query/caret history, fresh-visit input retirement, deferred Ask acceptance of painted rows, and streaming running-image identity above the former 512 MiB cap. Root and Luna reviewed these changes. **The composed GUI build, native controls and repaired input journey remain unrun.** A `Presented` trace label alone cannot replace actual captured pixels.

TypeScript exact parentage and call endpoints are source-clear at `5d281c798ab0b8d151e381d4bf299e24af596c26`; the previous 64e same-label graph collapse is repaired in source. Its current-canonical composition is `9b7b21fd14a82821d380dbe7324ad6e55e2c4e53`, [PR 234](https://dev.nudox.org/git/Nudox/Backend/pulls/234). Baseline-falsifier and repaired native controls are still unrun. The report's ASP/GymBro member lists cannot be credited from a synthetic source review.

A concrete Python boundary remains missing: `captured_finder` deliberately disables interpreter probing and supplies an empty unspecified site-package list; current selected input carries project sources rather than a captured installed-dependency cohort. HTTPie/SearXNG installation therefore cannot, by itself, make the checker consume those dependencies. A typed immutable environment/dependency capture is being implemented without allowing ambient paths or counting dependency files as project coverage.

### Reporter issues and current gates

Old PRs 196/197/199/201 remain open in Forgejo, but their functional commits are already in canonical through PR 215. Their open labels must not be reported as evidence that wait recovery, complete member paging, short control reads or durable MCP start were never integrated. The exact CLI 42da 82/82 result remains useful, including the real subprocess retirement control; current large-project waits and concurrent reads still require a fresh matched-trio replay. The original eight-second cold-status cutoff was also changed under PR 215. Its startup recovery hint still references a hardcoded `nudox ... health` command and authority-file path; observed-executable/passive replay work in PR 216 remains a separate open gate.

The current matched-trio source `79ee0a714aca65da7f649e158414ba40c10847a9` includes reviewed MCP 227, TypeScript 223, Python 226 and Go 225 source, but predates the coworker's 231/232 changes. It is not a current-canonical binary. No matched trio has yet completed. The first remote phase is narrowed to MCP library compilation and nine exact controls; normal CLI/MCP/locald builds follow only after measured phase closure. Earlier pre-admission attempts and their failures remain retained, including a stale four-GiB disk gate and the missing private loan-registry directory; they executed zero product tests.

Resource limits remain 16 fleet slots, eight on ILO, four Cargo jobs maximum per build, eight-GiB destination memory and sixteen-GiB free-disk floors, plus the measured 80-GiB CI reserve. The narrowly bounded MCP experiment adds a two-GiB phase forecast and derives its disk gate as 98 GiB; it does not lower the 96-GiB reserve. The existing monitor stops on observed footprint breaches; this is not a kernel disk quota. Native/GUI additive ceilings remain 80/36/116 GiB. The offline Mac is explicitly excluded only for authorized ILO validation and is rechecked before guard sampling. Local heavy builds remain paused; source-only work does not prove laptop GUI acceptance.

Installer publication, current binaries, fresh project CLI/MCP acceptance, GUI pixels/input/publication, and useful configured semantic search remain open and distinct. No new production-ready bundle, Homebrew pass, installer success or complete semantic coverage is claimed.


## Follow-up: 2026-10-14, 01:05 UTC

**Product readiness is not established.** Canonical was fetched at `8b36bccf579e1f02f4cacd510152df8fcf8429fd`, tree `1482e861fefaa908ef2b5716454675a85b5b8c07`. Root reviewed and merged PRs 219, 223, 224 and 226. The existing `.config/nix/tools.nix` edit remains outside these commits; its SHA-256 remains `9cee064afabfacafd6b59a041993f6a0f98f382cf37866caf1d9e12225acff63`.

This follow-up supersedes pending-result statements in the historical 22:00 snapshot below. Results belong to their exact sources, not automatically to the latest canonical tree. Overlapping reruns, embedded child tests and SDK observations are not added together to inflate a total.

| Exact source / execution | Actual result | Remaining boundary |
| --- | --- | --- |
| PR 219 source `42da607dd8678230489f70c53328089e2623b522`, CLI continuation 06 | **82 passed, zero failed, zero ignored** across four test executables: 0 binary, 1 saved-index wait, 2 passive-health and 79 library tests. CLI `--help` also passed. Root checked raw phase hashes and counts. | Current CLI native tests; no fresh-project publication or current matched MCP/locald proof. Earlier manifest refusal and TMPDIR endpoint-length failure remain retained. |
| Python production repair `f1c4e1e1863c9a4c46070be9f3678cd6fd0a9136` | All 13 ordinary harnesses: **106 passed, one new configuration-fixture assertion failed, four ignored**. All four original failures passed. Explicit 407-file SearXNG capture passed against both Python 3.13 and 3.14 authority, retaining 6,326 local and 4,620 cross-file targets each, with 333 gaps and 2,125 native diagnostics. | Capture/authority proof, not complete semantic coverage or product indexing. |
| Python test-only child `fbde8bcfa4504b5817c8913d00b2e968ac31c4d0` | The configuration control now exceeds the aggregate bound using genuine errors, independent of platform path length. Library rerun: **53 passed, zero failed, one existing Paperless ignore**. Native cause and warning controls also passed. | Resolves the new fixture assertion through a test-only change; f1's recorded failure remains and its full 13-harness cohort was not rerun at fbde. PR 226's 54 selected postimages match this tested source, independently verified by Root. |
| Go SDK `b4b0dbea34b2f522da8e60daefe84abff0ffd60a` | **18/18 passed**, no failures or skips. Root checked actual JSON test events and retained phase hashes. | Rust Go projection and Caddy publication remain unproved. The earlier 14/17 SDK failure is retained separately. |
| Same b4, engine no-run attempt 07 | Cargo emitted 91 integration-test executables before a forecast-based footprint guard stopped the attempt. **No engine tests executed.** There was no library test executable: its library event had `profile.test=false`, `executable=null`. | New inline demand/projection controls require the missing library harness. Interrupted artifacts are not a completed no-run or suite pass. |
| Desktop owned-context child `80a436b513c4a423eca66bb63d9f8e5356db23d7` | Both lifetime controls passed. Full native run: **1,357 passed, 131 failed, 14 ignored**, zero filtered, in 1,516.13 seconds. No EAGAIN cascade. Root counted all 1,502 top-level statuses before failure bodies and matched the terminal summary. | Lifecycle repair is supported; functional GUI defects remain. Two nested failure summaries inside failure bodies are not additional top-level tests. |

The 42da CLI application SHA-256 is `bd11717424a510b23808eebaba72094a09363d635e01bc6f5c62ac17b7df997d`; all four test artifacts and the application are independently retained before later warm-cache transitions. Historical e315 binaries below remain preserved, but cannot stand in for the current trio.

### GUI: actual pixels, input and lifecycle

Root inspected all ten cold13 images. Certification succeeded and captures were nonblank, but **five still showed Loading** at the chosen capture time. Certification, nonblank pixels and a settled useful page are separate conditions. Reads arriving after capture/close do not convert those five images into ready-page proof.

Focus17 produced 21 actual screenshots and 81 retained files. Settings, motion, text scaling, Shelf, Ask and Add were visited. The final shelf remained empty and no project publication was observed within 180 seconds. Clicking Full motion selected System; zoom reset erased the state needed to establish the preceding 200% setting. The journey failed rather than proving these actions worked.

The concrete coordinate defect was that Linux implemented `a11y_update_window_bounds`, but GPUI core never called it. Accessibility SCREEN coordinates could omit the window origin and dispatch input to a neighbouring control. The reviewed repair synchronizes bounds after accessibility initialization and on bounds changes. The next observer moves the real owned window, verifies SCREEN and WINDOW extents follow the translation, restores it and then acts. Screenshots precede expensive accessibility queries; motion and zoom state are recorded before reset. Blind coordinate compensation is not accepted.

The owned test-context repair avoids retaining a leaked static context through mounted applications and worker callbacks. Its two controls verify destruction and refusal of queued callbacks after the owner is gone. The task cap remains 1,024. All 38 originally classified pre-exhaustion failures remained in the full rerun, alongside newly exposed graph, focus, history, onboarding, scrolling and publication failures. These require causal repairs, not assertion removal.

Ask's source repair observes root and owner changes, renews its query, retires painted-row and Enter capabilities on owner changes, and preserves typed faults. Source review is clear at `2510f3fe9dbdd64f07b74b8e3d6b8af231059406`; nine targeted native controls remain queued. A further same-owner search-completion/queued-Enter interleaving is under investigation, not yet claimed as a tested repair.

The next GUI candidate is `3c65d18a6462257b1f18e2feb1b663e13c8281f1`, tree `6d533bb4a10ae6e48e7698393a65b6fe7ab5610a`: current canonical plus reviewed bootstrap, owned-context, accessibility bounds and Ask changes. Its ordinary build is **in progress on ILO**, Cargo PID 2406329 under actor 2406302 and managed scope `nudox-job-a50203b39434427e90bd223d36e02205.scope`; selected controls and genuine publication/input journey remain **UNRUN**. Preserve the normal executable before test builds. Resize, 200% text, keyboard/focus, motion sequences, graph return-to-symbol, discovery/Shelf, retry, offline use and cold persistence remain gates.

### Current integrations and product work

| Work | State / source | Gate |
| --- | --- | --- |
| [PR 223](https://dev.nudox.org/git/Nudox/Backend/pulls/223), installed TypeScript runtime | Merged at `53e208e1325a1c70367f0c5d808ece4e1a7314f7`; bounded installed API, typed diagnostics, DTO 29 | Guava SDK-only observation: 258 files, zero diagnostics, 999.404 ms. Current native product indexing remains unrun. |
| [PR 226](https://dev.nudox.org/git/Nudox/Backend/pulls/226), Python authority/configuration | Merged at current 8b; selected source `f8ca00190a83850fcd61f65cbbd054b4a14087eb` | Exact tested postimages verified. Fresh HTTPie/SearXNG/Paperless acceptance still required. |
| [PR 225](https://dev.nudox.org/git/Nudox/Backend/pulls/225), Go demand/owner runs | Open, nondraft, b4 | SDK 18/18 passed; engine library controls, Rust projection and Caddy acceptance open. |
| [PR 227](https://dev.nudox.org/git/Nudox/Backend/pulls/227), MCP reply budgets | Open, nondraft, `45c795dd95fb2600a386a12c3dd3e3e0e98049de`, tree `d8827f96ddc91a257bb7d1a872a0f8fd7c8633f1`; includes current TS/Python | Review of this exact 45c composition is clear; current MCP build, controls and matched trio unrun. The later blocked TS64e work is not part of this source. |
| Captured project tree | `edc3f20245aabc3538210312a3bbd12fec8b22bc`, directly on exact 45c | Explicit CLI `project-tree --sources` / MCP `sources:true`, captured membership and genuine cursors. Cargo remains the default. Native and GUI captured-source rendering unrun. Incorrect earlier fb3 composition was rejected and retained. |
| TypeScript exact binding parentage | `64e7f9b501ee0d5d963a9b6081f6ac3a94b9d1b8`, tree `279a27a5820d561bd01561cfe723c393d5a3d0d7` | Capture/storage parentage source review clear, but **composition blocked**: structural call projection still collapses exact same-label callables to one display-coordinate node and misattributes their edges. Binding-aware graph endpoints and a genuine sibling-worker control are being implemented. Codec/projection/UTF-8/cross-file/page-reuse controls remain native-unrun. |

The next matched-trio composition is `79ee0a714aca65da7f649e158414ba40c10847a9`, tree `8af9b46f4a9b60dd33632fd64624c5e6dfdb7483`, parents exact 45c and b4. Zero conflicts or lockfile changes: all 12 Go postimages match b4, all other 45c postimages are retained, and canonical 8b is an ancestor. This is prepared source, **not a built trio or merged PR**. PRs 213/216 below also remain open with their native gates.

A seven-corpus current acceptance family is prepared for HTTPie, SearXNG, Paperless, Caddy, Angular/webdev, Excalidraw and a Guava SDK-only fixture. It needs new matched executable hashes before admission and ordinary dependency setup. Coverage includes default private HOME/XDG-unset startup, add/wait/retry/cancel, all MCP families, complete source/search/graph page unions, wrong-authority/bad-argument refusal, token counts, offline use and cold restart. Original reporter ASP/GymBro captured revisions remain unresolved; a substitute is never presented as the reporter's reproduction. Actual provider/model activation and useful semantic/embedding search remain separate open gates.

The historical SearXNG status/index discrepancy, Cargo-only project tree, uncaptured local dependency metadata, unconfigured embeddings and failed Caddy package-image admission below remain acceptance defects. Partial structural reads are useful but do not imply successful publication. The historical 25-row token experiment still exceeds 12,000 after preview deduplication; candidate native paging must request a smaller complete owner page and preserve its genuine cursor.

### Capacity, installer and evidence

Heavy compilation remains on ILO, protecting the laptop. Fresh canonical v6 sampling and the existing managed permit are required for every new workload. Temporary Mac exclusion must be rechecked against fresh offline evidence; limits remain 16 GiB memory, zero swap and 1,024 tasks per job, confirmed in the kernel before grant.

Current artificial additive footprint ceilings are native **80 GiB**, GUI **36 GiB**, combined **116 GiB**. They are not reserved physical capacity. Admission retains at least 8 GiB destination memory, 16 GiB free disk and the measured 80 GiB CI disk reserve, plus phase-specific prospective growth. Closed workloads are not future forecasts. The b4 interruption added prospective growth to the old ceiling while actual additive native bytes were still below that ceiling; it was not a Rust compilation failure or proof of physical disk exhaustion. Resident tests and warm linking use separate forecasts.

These current caps are recorded in `gui20-sealed-control-ledger.json`, SHA-256 `f916da93f5ea66779328709c3c8736b4e28f8fc7331e48e30bd89980598e9ade`, rather than the superseded GUI19 ledger. The 80 GiB additive native cap and separate measured 80 GiB physical CI reserve are different constraints.

The earlier ced cleanup failure was recovered using exact owned-scope evidence: drained, reaped through ECHILD, cgroup gone, permit released. Missing bounded loan records were not fabricated; the original error remains retained. Later scopes closed cleanly. This does not change product failures.

The public channel remains `production_ready: false`, with historical Linux c001 and Mac a659 assets. Installer helper tests below are not fresh-product installation proof. No new verified investor Mac bundle, release asset promotion or current fresh installer acceptance is claimed.

Root independently checked these local receipt bytes under `/Users/mileswirht/.cache/nudox/evidence/report21-20261013` and `/Users/mileswirht/Downloads/nudox-active-20261013/gui-execution-inputs`:

| Receipt | SHA-256 |
| --- | --- |
| CLI continuation 06 raw driver | `f223d03062b6f446e11627f9983743cef209786031cd4141d0a272bda7cf20f2` |
| CLI continuation 06 outer closure | `55fdd4d5d33c935bf85f26d62ec4d71f2386bfa1e19916f82f44bc7d0d36eb02` |
| Python library54 summary | `9b55cb0a9075adc5873072d4b08d5a5592f37ada24bd5ef6c5970cac0a15b84c` |
| Python library54 raw driver | `a07c1307d4edf6097a1cae29806065869f71c314893c00046578f9f74d55e44e` |
| Go SDK18 actual evidence | `0d6b7cacec0ff1c703fd9fb584357c740cd638c12d8e0860b619a4cf88c2e0f1` |
| Go SDK18 raw JSON events | `9ffd4a251bd740921f44b643c879123ba2cfa7b0b622811f75aeb4e679198dd8` |
| Interrupted b4 engine07 Cargo JSON, retained locally in `b4-engine07-root-raw/backend-engine-test-04.stdout.log`; Root independently verified its 92 artifact roles | `9e6d8afb0251caf55fc64ac5b9b871436db428f35b0ba803f879178b2316e825` |
| Owned19 raw desktop stdout | `70dfaa60dfdc39d25845c1504308095b8fedd12a1a57d5c98a981175db644985` |
| Owned19 driver | `041b35c67b3a8b4573693af38e8c0a7b221cafc76c30ec030156bb76f28ae754` |
| Owned19 managed closure | `697ad8b7ccf9f6544f6bf5949b536a1490d794a3d735e0a7d3a368bc6360fb30` |
| Focus17 final evidence archive | `3e64ced236089ebc6e1b9a988c21c5cb528f4b4937f987915f99cedd47101f07` |

No current repaired-source end-to-end pass, comparative performance win, complete semantic coverage or production-ready release is claimed. The next boundary is an exact current matched trio and ordinary desktop, followed by real publication and complete tool/input journeys from fresh default paths.

## Historical snapshot: 2026-10-13, 22:00 UTC

The following is retained historical evidence. Its pending results and source identities must not be read as the current checkpoint.

Updated 2026-10-13, 22:00 UTC. **Product readiness is not established.** This supplements, rather than rewrites, [Report 21](report21-current-readiness-20261013.md). Canonical was fetched at this checkpoint and remained `09e663d4b5e7a394e79b64ce56a378975239cad1`. The user's existing `.config/nix/tools.nix` edit remains outside these commits.

## Actual results, with source boundaries

| Source and execution | Actual result | What remains unproved |
| --- | --- | --- |
| Production baseline `e315da1ea22958522aae8401af959f45de12e2ac` | Earlier native cohort: 4,054 executed, 4,027 passed, 27 failed; local-service additionally had four test-build errors. Matched CLI/MCP/locald applications built. | Later source repairs cannot inherit these results. |
| Same e315 applications, cold status run 10 | Two real CLI `status` invocations passed: a private fresh HOME with XDG unset, and a precreated explicit XDG state directory. The first created its private default state directories. Both returned available/empty/ready, with semantic coverage explicitly unconfigured. | This proves startup/status, not indexing or semantic readiness. The previously missing explicit XDG directory failure is retained. |
| Same e315 applications, SearXNG read run 13 | 44 actual observations and 656 observer checks completed in 33.36 seconds. A nine-row plain graph equaled the complete three-page MCP graph; cold offline restart retained the same coordinates/revision. Search and Resolve each traversed two genuine three-row pages. | These counts are observations/checks, not product test counts. Six coordinates per query are representative pages, not the full query union. Index publication remained failed. |
| Python capture `2bd97e92` candidate | Ordinary cohort: 98 passed, four failed, four ignored across 13 real test executables. Separately enabled genuine 407-file SearXNG capture passed in 33.42 seconds using Python 3.13/3.14 authority. Each result retained 6,326 local and 4,620 cross-file targets, alongside 333 coverage gaps and 2,125 native diagnostics. | Genuine native capture passed; complete semantic coverage and product indexing did not. New Python repair source has not run. |
| Go, same e315 trio against Caddy | Indexing failed at a 2,551-byte primary source with fact ordinal 1,284: the package-wide image exceeded a reservation estimated from primary-file bytes. 54 observations, 878 checks, one failed check. | Go compilation/publication is not ready. Unrelated cancellation/key/read checks that passed do not negate this compiler refusal. |
| Desktop `cddfd02675b8424e44849eab6d014231e7d2dac6` | Normal Linux desktop build passed in 280.26 seconds. Client cohort passed 178/178. Desktop test compilation subsequently failed on three fixture type/import errors. | A normal binary and client tests do not establish GUI interaction acceptance. |
| Test-only desktop child `ced376e6ba2dc7ca6f98d66733a121845d2b5cc7` | Desktop test build passed in 49.30 seconds; full run finished in 906.46 seconds: 1,096 passed, 390 failed, 14 ignored, zero filtered. | Many failures contain actor-start EAGAIN after task exhaustion under the 1,024-task cap. This does not excuse earlier interaction/assertion failures or establish that all failures have one cause. Classification and serial reruns are required. Scope cleanup itself hit a bounded loan-record error and is under exact owned-scope recovery. |
| PR 219 child `42da607dd8678230489f70c53328089e2623b522` | CLI test build passed in 389 seconds. The run then stopped on a harness manifest-path mismatch before executing tests. Independent role preflight subsequently admitted four test executables and the CLI app against the correct package manifest. | The continuation must execute those tests and the remaining native cohort. A successful no-run build is zero executed tests. |

The e315 trio is preserved independently of the newer source/cache transition: CLI SHA-256 `5e950361d6097143d04b48be02586bb95faed749bf13fc6f9d11634b437fad8d`, MCP `4a5b9f89a26305638fbd351a3cb99321cd20196b6ea24d2ca550b004b6d2af06`, locald `f728f8806dc6b2ead6b76b63bde8a5c5eca7e98b0762118f0fc03789e8d8c2ea`. The cdd normal desktop ELF is 725,063,272 bytes, SHA-256 `dd50f4c0ff379b6fd573e9e6e3435f267578e91b47ccb0cb7800125af1734e98`; its immutable hardlink is retained before later test-source changes.

## Tool capability gaps observed on a real application

SearXNG's retained structural data remains readable despite refused compilation. This is useful partial data, not a successful new index. The saved operation still reports failed profiles while baseline status reports populated/ready; that discrepancy remains an acceptance defect.

The real `project_tree` call rejected the Python project as outside Cargo scope. Dependencies returned typed `not-captured` because the local project was not recorded as a registry package; dependents returned `unconfigured` because the feed lacks dependency metadata. A terminal query returned one `{ "to": null }` row, which is not evidence of useful semantic traversal. Embedding search remained explicitly unconfigured. These results are retained as limitations, not successful empty answers.

A capture-based tree/dependency proposal separates local project identity from optional registry identity. It uses captured source membership and manifest bytes, never substitutes live filesystem reads, never executes setup.py, and distinguishes declared requirements, resolved lockfile edges, local incoming edges and registry reverse-dependency coverage. Its first language-neutral captured-tree slice is being implemented; proposal/source work is not native acceptance.

## Response-budget experiment

A separate managed Node 24.18.0/js-tiktoken 1.0.21 experiment measured 23 immutable complete frames with the pinned ordinary cl100k_base policy. Genuine Search three-row frames measured 4,778 and 3,667 tokens; Resolve measured 4,251 and 3,588. All fit the 12,000-token limit.

Removing duplicate previews from the old 25-row projection still measured **14,588–14,595 tokens**. Deduplication alone does not fix native page admission. Prefixes retaining the old after-25 cursor are diagnostic floors, not valid smaller native pages: accepting them would skip coordinates. The candidate must request a smaller complete owner page, retain its native cursor and fixed resume limit, and measure the entire JSON-RPC envelope, including executable continuation guidance and actual request ID. Candidate Rust encoder/tests remain unrun. The counter scope exited zero, reaped through ECHILD, deleted its exact cgroup and released its permit.

## Reviewed source awaiting its own native result

| Work | Exact source/checkpoint | Gate |
| --- | --- | --- |
| [PR 219](https://dev.nudox.org/git/Nudox/Backend/pulls/219): compiler/readiness cutover | `42da607dd8678230489f70c53328089e2623b522` | Current native continuation; later repaired-source fresh applications required. |
| [PR 223](https://dev.nudox.org/git/Nudox/Backend/pulls/223): installed TypeScript runtime admission | `a24e1f63a69b189df8360aa13a7f19b48118a047` | Strict bounded installed Compiler API admission, typed diagnostics and matched DTO 29; source reviewed, native unrun. Genuine Guava SDK observation was 258 files, zero diagnostics, 999.404 ms; it is an SDK observation, not product indexing. |
| Python native capture/configuration/binding repairs | `3574e7742037db8a23f0f3e6634683175e681d53` | Restore candidate/frontier fault ordering, native definition-range binding authority, exact config vendoring, bounded cancellable diagnostics retaining genuine cause/field/file contexts. Original four failed controls retained; warm native cohort pending. |
| Go package-image demand and owner runs | `f99e320fa815d6c950915a32e3d48b93d9faebda` | Measure package authority before allocation, check coupled limits incrementally, preserve cancellation/deadline terminals, share allocation-free sorted owner lookup with projection. Caddy and genuine sibling/parity controls pending. |
| MCP semantic/source/graph paging | Frozen `c90d4b904dbfd30546df6ceb7e34a5c099025e6e`, guidance fidelity follow-up pending | Native owner cursor/source-range authority, complete-envelope admission, graph post-measurement cancellation, genuine captured-page replay. Rust/native execution pending; no proof inferred from standalone Node projections. |
| [PR 213](https://dev.nudox.org/git/Nudox/Backend/pulls/213): captured paths/search identity | `ce7890da426fcc79b4029d275f62c58e6b9ab5fc` | Native search/delta/cold rename-removal and unusual-path controls pending. |
| [PR 216](https://dev.nudox.org/git/Nudox/Backend/pulls/216): startup replay | `d9c0f1e2a83bf5636e03a90b73e97e3d6d5eb284` | Observed executable/workspace/passive origin retained; current native replay/wire controls pending. |

[PR 221](https://dev.nudox.org/git/Nudox/Backend/pulls/221) and [PR 222](https://dev.nudox.org/git/Nudox/Backend/pulls/222) were reviewed and merged by Root. The narrow process-identity race repair passed 33 Linux race controls; the broader Linux helper cohort had 57 passes and one old Darwin-mock failure. Root's actual Mac CPython helper cohort passed all 58 registered controls. Same-release installer reuse/cleanup passed 25 lightweight helper controls. These are separate registered cohorts and fixture-based installer controls, not native product installation/indexing proof.

## Installer, GUI and execution gates

The public diagnostic channel remains `production_ready: false`, serving historical Linux c001 (October 8, Ubuntu 24/glibc 2.39 floor) and Mac a659 (October 6) binaries. The newer installer helper has not promoted a new binary or proven a fresh current product install. The old GitHub checkpoint/preview URLs returned 404. There is no newly verified investor Mac bundle in this checkpoint.

The prior ten cold GUI starts remain zero complete passes: seven root failures and ten blank captures. Focus diagnostics produced real nonblank Settings/root-failure pixels, but do not convert those runs into acceptance. Focused cold-start captures, resize, increased text size, navigation/return-to-symbol, asynchronous publication/retry, shelf/discovery, keyboard focus, offline/cold persistence and actual motion sequences remain required against the repaired source. Interaction failures are repaired from real traces; assertions and coverage are not removed to achieve a passing count.

Heavy native work runs on ILO, not the laptop. Mac host exclusion is temporary, explicitly authorized and rechecked against fresh offline evidence. Canonical v6 sampling, destination memory/disk guards and existing managed permits remain required. Measured capacities are advisory; a permit is acquired after a fresh guard for each new workload. Cached builds and immutable original sources/artifacts are reused. Finite cgroup limits are checked in the kernel; a source configuration claiming limits is not proof they were applied. The full desktop run exposed both task exhaustion and a cleanup-receipt bound, which are recorded separately from product failures.

## Retained receipt identities

Evidence is retained under the persistent report21-20261013 directory and GUI execution inputs. Root independently read the cold-status and short-read summaries and verified these local hashes:

| Receipt | SHA-256 |
| --- | --- |
| e315 cold-status run 10 driver | `812d3d9ccd570296b4b483e0334beaccb1fe3ec6b84a86f3fabfc9755be502e8` |
| e315 cold-status run 10 managed closure | `17ab895c569d1726888e5202b81cab580bbf2e90606a6bf2b0c67945cedebbac` |
| SearXNG short-read run 13 classification | `46182b6afa892c6998eeed245621f7f756f8a0f5c9eda3865445be68bd208bb3` |
| Actual short-read run 13 raw receipt | `52fa6ee282f8fcac0e525743ef7655eda01f2f203075eac333b3ebc386103c0c` |
| Python native cohort summary | `76c2297f3e301cd413afb586ad6685b359273513bca41c275a9edf0903409c8c` |
| cdd normal/client/test-build driver | `ca72633ab25490be10afca3914df6ae9d78712f1ec842ce02af208bb1574d619` |
| Token experiment 15 measurement | `57060ea2edd3d0cae15ea08bf0001215a03c156150ddac6ea25b9ee144fcf18f` |
| Token experiment 15 managed closure | `ef7d3850d04ead1704429868bec1f071f1dea85e17fc23e37fe30bf7c30fb89c` |

The full ced desktop result and recovery were worker-reported at this timestamp; raw failures and final closure are being exported. They must not be called independently verified before Root reads the actual receipts. No current repaired-source end-to-end pass, performance win or production-ready release is claimed.
