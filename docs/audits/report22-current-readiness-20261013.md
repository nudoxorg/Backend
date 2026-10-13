# Report 22: current execution and repair checkpoint

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
