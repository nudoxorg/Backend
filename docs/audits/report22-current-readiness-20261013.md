# Report 22: current execution and repair checkpoint

## Follow-up: 2026-10-14, 01:05 UTC

**Product readiness is not established.** Canonical was fetched at `8b36bccf579e1f02f4cacd510152df8fcf8429fd`, tree `1482e861fefaa908ef2b5716454675a85b5b8c07`. Root reviewed and merged PRs 219, 223, 224 and 226. The existing `.config/nix/tools.nix` edit remains outside these commits; its SHA-256 remains `9cee064afabfacafd6b59a041993f6a0f98f382cf37866caf1d9e12225acff63`.

This follow-up supersedes pending-result statements in the historical 22:00 snapshot below. Results belong to their exact sources, not automatically to the latest canonical tree. Overlapping reruns, embedded child tests and SDK observations are not added together to inflate a total.

| Exact source / execution | Actual result | Remaining boundary |
| --- | --- | --- |
| PR 219 source `42da607dd8678230489f70c53328089e2623b522`, CLI continuation 06 | **82 passed, zero failed, zero ignored** across four test executables: 0 binary, 1 saved-index wait, 2 passive-health and 79 library tests. CLI `--help` also passed. Root checked raw phase hashes and counts. | Current CLI native tests; no fresh-project publication or current matched MCP/locald proof. Earlier manifest refusal and TMPDIR endpoint-length failure remain retained. |
| Python production repair `f1c4e1e1863c9a4c46070be9f3678cd6fd0a9136` | All 13 ordinary harnesses: **106 passed, one new configuration-fixture assertion failed, four ignored**. All four original failures passed. Explicit 407-file SearXNG capture passed against both Python 3.13 and 3.14 authority, retaining 6,326 local and 4,620 cross-file targets each, with 333 gaps and 2,125 native diagnostics. | Capture/authority proof, not complete semantic coverage or product indexing. |
| Python test-only child `fbde8bcfa4504b5817c8913d00b2e968ac31c4d0` | The configuration control now exceeds the aggregate bound using genuine errors, independent of platform path length. Library rerun: **53 passed, zero failed, one existing Paperless ignore**. Native cause and warning controls also passed. | Resolves the new fixture assertion; the full 13-harness cohort was not rerun at fbde. PR 226's 54 selected postimages match this tested source, independently verified by Root. |
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
| [PR 227](https://dev.nudox.org/git/Nudox/Backend/pulls/227), MCP reply budgets | Open, nondraft, `45c795dd95fb2600a386a12c3dd3e3e0e98049de`, tree `d8827f96ddc91a257bb7d1a872a0f8fd7c8633f1`; includes current TS/Python | Source review clear; current MCP build, controls and matched trio unrun. |
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
