# Runtime checkpoint: 2026-10-12

This checkpoint follows the [previous readiness report](runtime-readiness-20261011-evening.md). It separates actual native execution from source-reviewed changes. Public installers still select historical binaries; no new production-readiness or public-release claim is made.

## Integrated source and open regressions

Root merged Forgejo PR #155 into canonical at `b81b361942bce1e0cf44d5e7edec75f975f9cf3a`, after fetching and preserving coworker changes. Its tree is `666c99efd84735ac9f4c79e8c4fe9906f6c69977`. The coworker merge required deduplicating two imports of the same `NodeAccess` trait. The canonical checkout's preexisting Nix edit remains unchanged.

The follow-through source corrects three concrete projection/compiler defects found while reading the full GymBro attachment and reviewing the actual implementations:

* Shelf assembly now recognizes captured filenames attached to content-addressed compiler declarations. It preserves unavailable counts on package-only replies and does not invent source for external or uncaptured rows.
* Relative names such as `AuthService.currentUser` select a leaf posting and verify the exact retained parent chain in the same view and admitted package. Review caught an absent-package ownership false match; the corrected source refuses that chain while retaining ordinary unqualified lookup and legacy qualified labels. Existing opaque declaration identities remain unchanged.
* Ambient module export lookup reads each admitted literal module's persistent bound scope before the lossy merged export map. It uses the binder's existing public-row predicate, exact module/body/scope/arena identity, and typed named/wildcard owner edges. Direct exports precede named reexports, which precede wildcard exports; CJS hydration and ambient/File recursion share bounded cycle state. Earlier blocked prototypes are not credited as fixes.

These changes received Root full-diff review and independent Luna review. New shelf, routed MCP read, relative-name/cold-cursor, selected ambient-owner and default-format controls remain **native-unrun**. The original 33 partition tests and five resolution tests are unchanged; their latest actual results remain 25 passed/eight failed and four passed/one failed on the separate temporary observational source. Source review is not evidence that those failures are fixed.

The reported Angular mixed `references`/`extends` false cycle is already corrected in the current candidate's producer. The new ignored Compiler API control checks the exact solution-root/app/spec shape, selected app config, option inheritance without inherited project references, app/spec membership and closure-witness mutation refusal. It requires the genuine pinned Node/TypeScript API and remains unrun. A separately pinned real Angular20 application is prepared for ordinary npm setup and installed-product acceptance; it does not reproduce GymBro's solution-reference layout.

## Actual matched Linux build and package

| Gate | Actual result | Scope |
| --- | --- | --- |
| Matched CLI/MCP/local daemon build, `e0767e71b7` | Exit 0, 372.229 seconds | Normal diagnostic opt0/debug0 binaries, zero tests; not optimized benchmarks or GUI |
| Linux packaging attempt 01 | Exit 32, 0.036 seconds | A removed historical SDK mount failed before the packager ran; failure retained |
| Linux packaging attempt 02 | Exit 0, 72.269 seconds | Genuine retained Node 24.18.0/TypeScript 5.9.3 inputs; 135-file input inventory matches original receipt |
| Fresh HTTPie ordinary setup | Exit 0, 19.757 seconds | New Ubuntu UID-1001 home, upstream dependency install, pip check and actual HTTPie CLI help; no Nudox acceptance credited |

The [build audit](current-e076-trio-95-build-evidence-20261012.json) and [package audit](current-e076-package95-evidence-20261012.json) bind exact source `e0767e71b7077f9bbd1604cbf7bb097090fbbba3`, tree `519dcf63c5b558a509f254998131f768c3f3c14e`, raw streams, terminal receipts and generated metadata. Root independently rehashed those mirrored records. Archive and executable hashes are producer-reported; direct Root binary rehashes are not claimed.

The candidate archive is `nudox-linux-x86_64-e0767e71b7.tar.gz`, 221,791,292 bytes, SHA-256 `ba67413f5569fbd695dd4ff21637e6046439b0337586e3f3c0cfa0a33ec84275`. The packaging manifest separately preserves original build hashes and transformed ELF hashes. Its glibc 2.39 requirement limits this proof to Ubuntu 24-class environments; Ubuntu 22 and current Mac native installation remain open. A repeated installer, literal `nudox add .`, copied exact polling action, immediate first search, all positive MCP surfaces, offline/cold lifecycle and real larger TS/Python/Go projects remain distinct acceptance gates. The current candidate has not yet passed those gates.

## GUI and resource boundaries

The merged GUI source binds retained graph actions and header actions to their exact reading visit. The native packet contains the original 56 filters plus five reading laws. Pure executor controls are not native tests or screenshots. No Nudox GUI actor was present in the fresh computer-use inventory; current normal GUI, native61 execution, resize/motion captures and all six mounted interaction flows remain open. Linux rendererless TestAppContext controls do not establish pixel correctness.

ILO is the reachable native host under the user's explicit authorization. The remote Mac remains offline. Fresh guards retain the fleet/host process caps, destination memory and disk floors, conservative unknown-process occupancy and managed slots. Local admission remains blocked by unresolved process groups; no unrelated actor was stopped or discounted by process name. Packaging retry charged its preserved SDK inputs and both attempts, retained the original baseline and retirement carry, and stayed inside its two-GiB forecast and twenty-GiB cumulative envelope. New jobs require new admission; these historical samples cannot authorize later work.
