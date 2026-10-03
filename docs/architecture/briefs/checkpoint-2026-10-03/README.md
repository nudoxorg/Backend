# Native GUI and live index readiness campaign — 3 October 2026

This extends the [2 October exhaustive checkpoint](../checkpoint-2026-10-02/README.md). It records the current implementation contracts, observed failures, evidence requirements, deployment boundaries, and remaining work. It does **not** declare the GUI or distributed deployment production-ready. Source review, compiled tests, native interaction, and remote service acceptance are separate gates.

## Current evidence

The integration candidate is frozen at `033461b522` in `/private/tmp/nudox-gui-flow-integration-20261002` while its native product and visual-harness binaries build. A separate refinement worktree, `/private/tmp/nudox-gui-flow-refinement-20261003`, collects reviewed changes without mutating the source being compiled. Successful compilation and native acceptance of that candidate remain outstanding at this checkpoint.

The continuously running Sol explorer has examined more than 150 actual native screenshots, paired accessibility trees, pointer and keyboard interactions, process exits, and two short screenshot-sequence films. Its historical executable has SHA-256 `92ca417fb0f66b2b9a1edc12440d0b9252ff5b6510ede5fdfc35786da5aa137e`; its containing checkout is not compiler provenance. Those captures are failure research, never acceptance of newer source. The detailed ledger is `docs/reviews/sol-gui/exploratory-native-audit-2026-10-03.md`; raw evidence is `/private/tmp/nudox-gui-user-audit-20261003`.

The reviewed service-retention branch `558dfc86139084ac9085035b065d41981ace029b` passed all **20** `backend-local-service` service tests with locked, offline dependencies. Evidence is `/private/tmp/backend-lease-expiry-run-558dfc861-01`. The final receipt distinguishes a valid 785-sample `ucomm` process census from an earlier invalid `comm` sampler. The observed valid interval stayed at at most three local Cargo processes, with one remote build reserved. The unobserved earlier interval is not a resource-cap pass.

The Linux deployment source is `80905da3ac3658be1646cb2cb607a1502de03ea2`: the earlier deployment source plus reviewed worker compile repairs and the Rustdoc source-map underflow repair. Its locked metadata gate passed and its release build is running. No listening service, successful registry ingest, authenticated worker completion, cold-restart acceptance, or current Turso backup is claimed yet.

## Architecture contracts and structural repairs

### Local state does not need an index permission

The shelf, project addresses, settings, inbox, saved navigation, and onboarding are local application state. A failed or starting owner cannot replace these surfaces with a remote read failure. Selecting a saved local address is allowed offline; executing a content-dependent action requires the corresponding current producer observation.

`RouteDependencies` now derives a closed `ContentAdmission`: ready, pending, or a terminal failure attached to the exact required key. Required Tree/source/declaration failures win over optional metadata failures. Reader rendering and destination/action admission consume the same result; arbitrary scans for the first failed optional page were removed. Library home has no required catalog read. Settings and Inbox are rendered before underlying page failure handling.

This distinction must extend to every empty-state claim. An unread or failed relationship is not an empty relationship. A Library catalog does not currently contain aggregate dependency/dependent evidence, so its Rests on and Used by tabs cannot manufacture verified zero counts from a saved project list. A Sol slice is replacing those claims with typed resource admission and explicit unsupported/unavailable states.

### Startup discovery is a retryable producer

The application constructs its UI, actor, and read lanes before host discovery succeeds. They share one immutable `Binding` installed by the actual host discovery producer before owner Ready. A second distinct path installation is explicitly rejected, including concurrent losing installations. Actor/read clients lazily bind the same endpoint rather than each rediscovering a potentially different workspace.

Retry is a capability for one exact failed publication on one exact owner gate. A callback rendered for an earlier failure cannot retry a later failure, another gate, or a producer that no longer exists. The gate accepts Retry and transitions to Starting atomically; OwnerLink marks Starting only after acceptance. Cancellation callbacks run after releasing gate and queue locks, allowing legitimate reentrant state inspection.

Only explicitly app-owned durable leaves are repaired to private permissions. General caller-selected data roots retain strict ownership requirements. The late cold snapshot is merged once into already-created local state; edits made while startup was failing are preserved. Tests cover actual discovery failure, retry, immutable binding, no-producer refusal, stale retry tokens, reentrant cancellation, queued local requests, and restart. Most of these newly composed GUI tests remain unexecuted.

### Compilation admission is distinct from display progress

Adding a folder persists a queued local intent even while the owner is unavailable. It must not say Compiling before actor admission. Duplicate additions coalesce without resetting ready, in-flight, or unconfirmed work. Before dispatch, the exact owner attachment is rechecked and the submitted state is durably saved; a late preflight failure cannot overwrite a submitted or newer request.

The compiler preserves the existing Rust/IR foundations. The observed Rustdoc underflow was in pinned `ra_ap_hir_def` source mapping, not proof of a dead worker: an existing lane catch already handles panics. The vendor repair uses checked subtraction and preserves owned macro-expanded documentation. Publication is only reached after successful staging. Added same-lane tests require a typed failed attempt with no artifacts, followed by successful real Rust compilation and readable macro documentation. They do not prove panic-poison scratch recovery without an actual panic-injection seam.

### Native focus owns activation

FACET controls share GPUI's native `ClickEvent` activation for pointer, Enter, and Space. Presentation listeners do not independently execute the action. NativeControl context prevents competing shell activation, and key-down propagation is stopped only after GPUI has observed the event. Tests include press/release, repeat, disabled/busy controls, focus changes, and parent action conflicts.

The explorer reproduced a fatal process abort twice: left-click then right-click a shelf tile caused GPUI accessibility to claim the focused node as its own active descendant. Shelf focus now distinguishes Native, Descendant, and None using stable actual handles at prepaint. Virtualized handle retention is bounded to visible rows and a small spine set. Meaningful native owners are retained; accessibility is not globally disabled as a workaround. Forced-accessibility pointer regression tests still need execution on the composed source.

Hint mode owns its letters ahead of plain F/G/H/S shortcuts. Failed search submission retains its exact draft, focus, and visible refusal inside Ask. It does not silently close the query to expose a page-foot notice. Existing refusal tests were updated to use real Enter and preserved draft assertions.

### Transient composition is separate from route history

Four closed transient kinds have a bounded session-only underlay structure. Reopening a kind coalesces; Settings refinements replace the same layer; Escape/Back dismiss the top layer; committed navigation clears covers. Reader retains the nearest Settings or Inbox page beneath Ask/Add, while input admission uses only the top overlay. This prevents a temporary Add or Ask flow from removing its initiating destination or polluting durable history.

The next composition slice unifies focus return, top-layer input, drawer backdrop propagation, and CE focus traps. Hand controls use native tab stops and exact held membership; shell shortcuts must step aside for their native Tab order. Pointer Cancel, Escape, successful submission, and backdrop dismissal must use the same return contract.

### Presentation is an observable outcome

Historical background launch followed by first activation sometimes mounted Add/Settings/Ask accessibility and accepted input while screenshots retained old Orbit pixels. Resize, a later key, or a second click could recover painting. Foreground Finder launch was a counterexample: first Add painted correctly. A temporary system Reduce Motion comparison was restored exactly and did not establish motion causality.

Source review found two possible shared lifecycle gaps: macOS display-link registration/first activation did not necessarily rearm delivery, and Metal may return early when no drawable exists while GPUI unconditionally clears `needs_present`. The latter is a concrete lost-presentation contract. The repair introduces an explicit draw outcome so deferred presentation retains the existing rendered scene and retries without cloning the scene or rebuilding every UI child. Visibility, backoff, hard failure, headless capture, and external platform compatibility all need explicit treatment. Native cold-launch and occlusion tests are required before attributing the historical defect to this repair.

### Metadata and status retain their evidence

Package licence declarations now use one canonical `Known<LicenseDeclaration>`: expression, declared file, observed absence, or a reasoned gap. An unread manifest never becomes No licence or inferred permissions. Workspace inheritance is accepted only when explicitly declared; oversized manifests fail the complete read rather than parse a misleading prefix. Registry records do not inherit local Cargo facts merely because names match.

Status feedback uses a shared measured layout: a bounded three-line summary, intrinsic button widths from FACET's actual font/padding, full native Status semantics, and an explicit Details surface. Held marks occupy a separate row when status speaks. Details and Retry re-admit the exact visible visit, message, owner attachment/failure, and transient ownership at activation. Hand cards and graph outcomes gain real native semantics. Source-only matrix tests cover four widths, three text scales, both appearances, and pointer membership races; they remain acceptance work until run.

### Read work and retained producer state are bounded

ReadPool has lifecycle permits retained through queued, running, completed, and cloned drained results. It bounds admissions rather than only active threads, coalesces intermediate results without dropping terminal results, favors visible work, and updates landing priority from the latest focus. Cancellation is invoked after queue unlock. This is a lifecycle-count limit, **not** a measured heap-byte limit; AST/index byte accounting remains necessary.

Owner subscriptions now have finite lease duration, bounded active count, absolute reset-hydration lifetime, and a nonzero remaining-page budget. Cursor/token continuation is one type. Encoding and all validation complete before a page consumes or replaces retained state. Failed encoding, replay, rejected credit, or an expired page cannot revive a root or mutate the continuation. Owner polling reclaims abandoned leases and shutdown releases roots. An arbitrary blocking `serve_stream` and long blocking owner work can still delay reclamation; the tests do not erase that limitation.

### README actions carry complete scope

Cargo-selected README reads are independent of optional semantic dossiers and require the exact current local Tree/package receipt. Relative links carry original package, full requested/effective binding, root scope, README selection/path/content digest, and authored href. Saved addresses remain addresses; they never restore authority. A cold authority hint permits one identical retry after exact Tree revalidation, with cancellation and terminal refusal stopping the path.

Package-relative files and inherited workspace README targets are different variants even when relative spelling matches. Native row identities include full origin and exact endpoint, with duplicate occurrence as a tie-break rather than ordinal authority. The next reviewed slice replaces the 512-row cutoff with a compact byte-arena navigation index over the complete producer-accepted README and 32-row UI pages. It prepares parsing on workers, shares repeated href destinations, checks allocation/cancellation, and keeps later authored links actionable. Paint-bound heading/source identity must use the same full document scope; stale rectangles cannot cross documents, contents, targets, or visits.

## Remote deployment and evidence contract

The SSH aliases supplied by the user are `ilo` and `h16001mac`, resolved through the Nix-managed external SSH config. No SSH config or unrelated services are replaced. Ilo has dedicated index/worker users, private mode-0700 data roots, private authority material, and separate service identities. Its existing firewall admits the selected public UDP port 60023. Iroh identity and signed, bounded scope grants are required before any package data is served; a worker explicitly closes unsupported remote-index ALPN connections instead of accepting another peer role.

Builds run through the pinned Nix shell/toolchain, one Cargo job and no incremental compilation. The existing stamped source/build root is updated only while Cargo is stopped and after preserving the earlier source evidence. The exact tracked manifest, lock hashes, toolchain, command, binary hashes, service command, and running process image must agree. Mutable target stamps are never forged to make worktrees share graphs.

The first valid deployment is a Linux index with an independently identified Linux compiler service. Cross-host target support remains a structural blocker: current `target_platform_identity` hashes the execution host and is folded into semantic recipe/session identities; there is no explicit requested-target configuration. Strict grant/input/result equality checks are correct and must remain. Requested compilation target and verified target-support/toolchain capability must be separated from execution-host provenance before a Mac compiler can participate in a Linux owner's target pipeline. Changing a hash or accepting a boolean advertisement is not sufficient. The Mac service is not represented as deployed or Linux-capable.

Required deployment acceptance:

1. Verify installed process/binary provenance; start isolated index and compiler identities; reject an unauthorized remote session before real ingest.
2. Ingest pinned real packages from Cargo, npm, PyPI, Maven, NuGet, Go, and Conan. Check each exact package/profile/version/search answer and actual source/checksum/download/advisory coverage. Unsupported downloads remain explicit gaps, not zeros.
3. Compile a real source package locally, then edit it and delegate background compilation through a real Iroh assignment. Compare actual assignment, peer, source/input/profile/closure commitments, selected complete semantic generation, and symbol search before/after.
4. Keep one real MCP process/stdin session alive through owner restart; verify initialization, tool listing, CLI/shared-surface results, reconnect, and subsequent tools without substituting new sessions.
5. Stop the canary owner safely, back up both actual Turso stores from running-ingest state, integrity-check and hash their logical contents, restore only disposable canary state, and prove package set, semantic heads, and search continuity after cold restart.
6. Exercise the GUI against the live owner and with remote access absent. Local project selection, editing, settings, and previously persisted navigation remain usable; unverified remote content/actions visibly lose admission.

Pending-empty is not a durable proof of completed worker publication. The current exact pending-ACK journal validates stored results against immutable selected semantic metadata, but deletes the row after authenticated retirement application. A bounded typed completion receipt is being added to join the worker closure to actual selected generation/manifest/dependency commitments through the existing SemanticVersions surface. Completion must be durable before pending deletion; failure retains pending; replay is idempotent; superseded/rejected results emit no success receipt. Until that lands, capture available evidence and report the missing durable link. There is no real logger oracle named `result owner ACK: Stored` and no generic `cluster inspect/status` CLI command to invent.

Verified deployment commands, peer IDs, service paths, invite/grant steps, and stop/restart procedures will be appended from the actual installed setup, after build/startup succeeds. A staged account or metadata pass is not an installed index.

## Native acceptance matrix and stopping rule

The explorer remains the sole computer-use owner. Sol workers implement GUI slices; Luna handles small non-GUI implementation, audits, deployment, and producer testing. Source slices use disjoint worktrees and path ownership. Root reads changed production code and tests, composes required-content and focus boundaries, commits reviewed integrations, inspects full screenshots and crops, and retains independent evidence.

The complete native matrix includes clean and saved-state startup; foreground/background activation; first action before resize; owner starting/failed/recovered; real folder picker/add/compile/local edit; restart/re-admission; all Library tabs, package releases, README links, source/copy, Compare, graph/tour/Hand, Find/Ask, Settings pages, native menus, Back/Forward; pointer and keyboard focus; widths 360/480/663/1440/wide; text 100/150/200%; both appearances/contrast; reduced/full/system motion; interruption, occlusion, and resize during transitions. Blocked flows stay on the ledger and cannot disappear from the coverage denominator.

Screenshot films produced from computer-use calls currently deliver their first frame about 500–700 ms after input. They prove endpoint consistency, not the first 400 ms of animation or frame pacing. The native GPUI test-support/ffmpeg harness must cover actual frame timing, geometry, focus, accessibility, and interruption; real-server interaction and native input remain separate gates from seeded component renders.

Resource enforcement uses actual executable-name process censuses and ownership-safe cancellation. At most four builds are permitted; one remote build reserves capacity, leaving at most three local Cargo processes. Unrelated processes are never killed. Invalid samplers, withdrawn dependency-only builds, and uncovered time intervals are recorded explicitly. No source edits occur in a compiling worktree.

The campaign can stop as ready only after fresh binaries pass the above flows, failures have been classified and closed by their shared contracts, installed index/compiler/MCP/CLI restart canaries succeed, and actual screenshots/benchmarks substantiate the result. Remaining large architecture gates from the previous brief—including durable IR lineage/locality, cross-target capabilities, embedding configuration/performance, real remote storage, and measured search quality—remain open unless independently demonstrated. Clean source, ambitious types, or successful isolated tests alone cannot close them.
