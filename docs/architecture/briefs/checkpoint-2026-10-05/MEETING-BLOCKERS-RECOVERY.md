# Meeting blockers and durable recovery

Updated 2026-10-06 after the local restart. This is an acceptance ledger,
not a declaration of production readiness.

## Release blockers

The meeting's Plural checkout reportedly contains about 84 MiB of source.
The distributed preview refuses the complete checkout at the 64 MiB admission
boundary, overflows a per-file record for `TaxRunModal.tsx`, relies on explicit
TypeScript setup with a build-machine checker path, and reports an unhelpful
`Fragment(Prepare)` failure for `libs/utils/server/cache.ts`. The successful
19-declaration client subtree is not full-project acceptance.

The exact checkout path or URL is still requested. Equivalent corpus cases
can validate mechanisms, but cannot close the original reproduction.

Required acceptance:

- Capture the complete admitted source frontier with bounded working memory;
  page large inventories and file facts instead of increasing one canonical
  row's capacity. Preserve typed budget/refusal evidence for excluded inputs.
- Discover and admit ordinary project-local npm/pnpm/Yarn toolchains without
  a path into Nudox's build checkout. Bind all compiler configuration and
  input identities into the recipe, and revalidate before publication.
- Resolve distinct TypeScript programs, config inheritance/references,
  imports, libraries, ambient packages and triple-slash inputs. TSZ remains
  semantic authority; a `no_lib` source-frontier experiment is not this gate.
- Preserve concrete typed prepare/write/validation causes through the wire
  and all three client surfaces. Better diagnostics do not themselves fix
  the lowering failure.
- Index the exact full Plural checkout, then verify meaningful search,
  outline, graph and exact source reads through matched CLI/MCP/GUI binaries.
  Repeat after a controlled edit, a cold restart and an interrupted operation.

## Recovered source

The canonical checkout is `/Users/mileswirht/Downloads/backend` at
`6ac9f5438b375f3dfc8985cc867046dbcf993abd`. Its user modification to
`.config/nix/tools.nix` was left untouched. Long slices and receipts now live
under `/Users/mileswirht/Downloads/nudox-active-20261005`, rather than `/tmp`.

The integration worktree retains canonical plus these source checkpoints:

| Checkpoint | Purpose | Acceptance still needed |
| --- | --- | --- |
| `9579cea07af69cc1840016f2e4c94419b4cd5fb5` | Native Settings return ownership | Candidate native suite and real app interaction |
| `b85baff0b701152a26ab971116e5a88a7c2080b6` | Wake retained Graph after owner change | Native automatic-wake regression and real retry flow |
| `1a60a10dc8868949fd24d61618670f8f9c761908` | Preserve verified bootstrap root after acknowledged socket retirement; proof-gated recovery | Recovered integration tests and cold native startup |
| `19ddfd03ccbd1954aa00de7051a92f38251bcff8` | Reauthenticate expired renewal with one bounded resume | Exact renewal/rejection tests and live reconnection |
| `fa36560d5227b7e12d008c49352b945860d74fb8` | Retain complete current proof through readmission | Exact fresh/stale/withdrawn-proof tests |

Additional publication and native-focus repairs are being isolated into
atomic commits. They must not be treated as tested merely because the source
was recovered or merged into an integration branch.

## Evidence boundaries

The restart removed local temporary worktrees, processes and receipts.
Surviving commit objects are source evidence; previous reported local results
are historical results until rerun or recovered with their exact receipts.
No old local process is credited as still running.

The ILO host survived. Its recovered matched-production CLI/MCP corpus
receipts are saved under `cli-mcp/recovered-2328`. The changed-file reindex
correctly refuses the old cursor as `stale_cursor` and admits a fresh query.
The earlier unchanged-fixture result was a harness mistake: a no-op
publication may legitimately retain its recipe. Large aggregate MCP context
use and ordinary TypeScript setup remain open acceptance gaps.

The preserved r3 bundle is at
`/Users/mileswirht/Downloads/Nudox-preview-20261005-9d29-r3`.
The byte-preserving Homebrew repair was published to tap `main` at
`310fd41e1033bc33d980c4fe6d8659e65f3b623e`. A real Homebrew install completed
with exit zero; all six installed Mach-O hashes match the preserved bundle,
strict deep signature verification passes, the three executable help commands
launch, and rerunning post-install is idempotent. The receipt is
`delivery/brew-installed-payload-verification-20261006.json`.

This is installer acceptance, not compiler or GUI acceptance. The completed
stock CLI/MCP acceptance fails even after installing project-local TypeScript
5.9.3 with an ordinary Homebrew Node runtime: `tsc --noEmit` passes, while
both installed surfaces refuse the tiny project with
`TypeScriptCompiler configured: None`. Twelve paired DTO checks agree across
warm and cold owners, but agree on failures and empty results. A real new owner
starts after controlled shutdown; no semantic generation was published, so
semantic persistence is unproven. The exact wire, source, binary hashes, and
receipt are in `cli-mcp/homebrew-acceptance-20261006`. Its eight troubleshooting
packets total 25,185 response bytes; the catalog alone is 18,855 bytes. Byte
counts are measured; byte/4 token counts are estimates.
`brew test` is separately blocked by this Nix-provided Homebrew's read-only
vendored Gem marker, although its three help assertions were exercised directly.

GitHub release ID `404084754` became draft three times. The third update at
01:00:09 UTC followed no further asset upload; republish at 01:05:51 restored
an anonymous ranged tarball GET (206), and the subsequent checks remain public.
The exact CLI upload code has no release-state mutation; the actor causing
these changes is not established. Never infer public
availability from authenticated upload success: recheck the anonymous tag and
asset URLs after all release mutations. The tarball hash remains
`09ba4990d2b0bf45d95d341d2ddd546ffe63224bbfa609715ce7dfe9212ed34b`.

The remote Mac's SSH attempt timed out while local Tailscale was stopped.
Its process state and unrecovered install receipts remain unknown.

Two user-restored GUI instances are running. Native computer-use input is
held while the user is active; source reviews, owned fixtures and GPUI tests
continue. Screenshots of the running r3 Settings page are observations, not
acceptance of the new candidate.

## Current native GUI results

The complete pinned baseline desktop library suite at
`4b1e6af973e7d34eadc368f1e47b3efc63ce40fb` finished with **1,248 passed,
131 failed, and 10 ignored** (exit 101). Its log and every failure body are
preserved in `gui-audit/builds/desktop-native-recovery*`. The failures include
real publication/focus/animation defects, invalid test assumptions, and missing
Cargo-cache inputs. Classification does not close the runtime gate.

The joined native baseline `6f38cd7cc` passes eight Reader tests with three
ignored capture tests, eleven host lifecycle tests, seven index-preflight tests,
eleven owner-publication tests, and the failed-outline reread regression.
The close selector passes fourteen and fails one checkpoint-ack timeout; an
isolated retry passes, leaving the intermittent failure open. The body selector
passes 146 and fails eighteen. Fixture repairs and product repairs have separate
source commits; those failures are not closed by classification.

The actual native pending/resize film passes its existing assertion, but root
inspection of all seven PNG crops rejects it: prose/caption duplication and
misalignment after resize, a mostly blank pending plate, and incomplete ink
tracing remain visible. `root_visual_acceptance` is **FAIL** in
`gui-audit/captures/reader-native-6f38-20261006/root-visual-review.json`.
The compositor packet now scopes deferred motion below later plates, retains
one readable departure while awaiting data, and keys cached native text on
trace state. A first-resize negative control and native cache/clip tests are
source checkpoints; the joined runtime and every capture still need checking.

The retained-local Graph/Source worker now uses an actual production owner and
certified assembled rows. Its full native production-composed proof passes at
`37b376afdb`: owner withdrawal, later actual stale reply, Settings return,
current/stale wheel input, pinch, drag, blur denial, reconnect, Find to an exact
symbol, Graph Return, Code and retained Source typing. GPUI hover state was the
wrong wheel/pinch admission predicate after keyboard use; scroll hit-testing
repairs that input boundary. This is one exact fixture test, not installed-app
acceptance. A later source checkpoint also covers attachment loss while the
service registry remains present; that rerun remains pending.
The dependency Back packet targets one native card owner; its joined native
keyboard/accessibility checks still need running.

Native hint actions now use continuously painted mount receipts, the original
typed payload, exact visit and window-bound input leases. Tree requests now
separate exact submitted identity from canonical physical invocation and
refuse alias retargeting. The joined desktop test compilation passes at `3e4d2c208f`;
their native and cold-start acceptance remains pending. No candidate application has passed live GUI
computer-use acceptance after this restart.

## Current compiler/index gates

Paged source facts and lazy verified reads are joined with bounded compiler
source handles. The source-admission checkpoint `4f10910d5` passes eleven
focused checks, including an 85 MiB structural scan/reopen and selected source
materialization. This is not an 85 MiB semantic index, publication or GUI gate.
The original 128 MiB compact-row ledger did not account for complete fact
pages. Root's `549428bda` joins root-alias/cancellation fencing, charges complete
encoded file manifests/pages before coordinator output retention, and stripes
workers across consecutive path positions. Contiguous chunks under ordered
acknowledgements had serialized useful work. The new policy identity is v3;
Rust test compilation passes, runtime regressions are pending. At most one
unacknowledged result per worker is retained, but page construction and parser
heap expansion happen before that charge: this is not a hard peak-RAM bound.

The actual 900-function TSX cold-capture test exposed another overflow at
`0c5ff1a8f`: attaching source identity after compaction changes both the header
and every declaration's containment encoding, producing a row 14 bytes beyond
capacity. `f6a5701bb` constructs and probes the final identified encoding and
strictly refuses an oversized post-construction identity attachment. Its
local-service test compilation passes. Its runtime reached committed source
facts and terminal capture, then exposed a cold-only no-op rejection. The new
`4d2d0d46c` shares live/recovery work counting and exact capture state derivation.
Root review caught its initial immediate-before assumption: a valid Pending
capture can survive unrelated commits. The follow-up retains a typed before
capture-root witness and tests Pending A, unrelated B, terminal A, cold reopen.
Its frozen metadata/runtime gate remains pending. Cold fact comparisons now
use the parser's exact output, with absolute line reconstruction and all 900
function docs/excerpts required. Full file facts remain in separate pages; this repair does not raise
the canonical row limit or claim that compact summaries contain every fact.

The TypeScript host checkpoint `4be9e6e323` passes Rust test compilation,
twenty-one host tests and five path tests. It embeds the relocatable driver,
admits project-local TypeScript, binds ordinary config/import/library closure,
and distinguishes discovered package roots from explicitly pinned module
roots. The launcher no longer forces bundled checker, module, compiler or Node
paths over project admission. All 32 joined launcher/package contracts pass at
`3e4d2c208f`; the joined bundle has not been built or installed.
TSZ's immutable options and exact resolver bindings pass focused frontend
checks, but full configured production dispatch, bounded shared queries and
cross-file reference fidelity are still open. No native semantic fallback is
credited as the authority.

The typed compiler refusal producer `a02e2ac8b` passes Rust test compilation.
Fragment kind and operands form one checked pair at construction and strict
wire decoding; human detail uses closed phase/kind labels and preserves source
paths. Projection admission errors propagate instead of silently erasing the
failure. The complete library unit suite at `1c764d19b` passes 282 tests, with one
ignored measurement. Shared CLI/MCP consumer `373cb11833` retains the exact
bounded failure and getter-derived tool setup facts while excluding raw legacy
detail. Later shared presentation and MCP adapters pass 87 and 90 unit tests
respectively at `bcd805338a`. Protocol strings, including valid serialized
compiler JSON, remain unproven instead of being guessed into typed failures.
Three additional real error adapters have a source-only follow-up awaiting
checks. Installed acceptance remains pending; diagnostics alone do not close
`Fragment(Prepare)` itself.

Python `f8321dbeab` and `fd1d2c5382` pass five focused checks. A real empty
Requests module compiles with admitted Pyrefly authority; class-source lookup
retains the exact path/digest/kind. Full Requests publication and cold-owner
retrieval are unproven, and an unrelated indexed-path punctuation test fails.
Go `e4576da2dc` passes 36 protocol tests and focused Go closure checks, including
child/descendant timeout cleanup. The corrected cgo fixture at `04fc616614` models the actual special `C.Row`
package. Full helper suites pass with CGO disabled and enabled on Go 1.26.4.
The previous fixture failure is preserved; complete Mux CLI/MCP publication
and cold retrieval still await a matched candidate.

The Luna provider later recovered. Ingest, TSZ dispatch, projection and
reference workers resumed from preserved source; unfinished work is not
credited as passing. The exact full-context ProjectCheckerSession improves
cross-file member resolution: alias/reexport and overload tests pass, as does
Nest controller-to-service target resolution. A spec invocation still loses
its MethodCall, and mapped/operator/depth/cycle/width projection regressions
remain red. The unmetered session constructor is exposed only to dev tests;
production requires the shared cancellation/deadline/work budget.

Npm's post-integrity live run streamed 25 packages with zero failures and 31,288
selected version rows. Ten npm tests, bounded-manifest and chunk-integrity
regressions passed in the later exact receipt. The bounded version window is
explicitly partial; this does not establish complete all-version ingestion.
The legacy Zustand report refusal measures the first over-limit read,
16,785,408 bytes (16 MiB plus one 8 KiB buffer), rather than complete report size.
The native TSZ path avoids the JSON type report, but currently requires the
source-frontier experiment. Configured production cutover needs the exact
program closure, cancellation/work bounds, reuse of checked program queries,
and assignment-narrowing parity. Per-file projector lanes currently allocate
about 1.69 MiB before facts; reducing that allocation is separate from proving
semantic fidelity.

CLI/MCP parity by itself also does not establish semantic correctness: the
recovered Nest and Requests runs agree on missing concrete reference edges.

## Joined review checkpoint

Root joined the coworker's `origin/canonical` at `6c98128cc5` into the recovery
candidate; the original checkout and user Nix edit remain untouched. Draft
Forgejo PR 24 is https://dev.nudox.org/git/Nudox/Backend/pulls/24. Desktop test
compilation and 32 packaging contracts pass at `3e4d2c208f`. Subsequent typed
consumer and native compositor packets are still undergoing joined checks.
No new bundle, installed journey, exact Plural acceptance or 14-frame visual
acceptance is claimed. The old released preview remains a confirmed semantic
index failure despite its successful installer.

The first six native vendor clip/cache checks passed three and failed three.
Those three failures came from a test wrapper adding the child's layout origin
twice; the corrected wrapper uses the same offset contract as production Flow
without changing comparisons. A separate real defect was found: local deferred
hitboxes used global prepaint order while their pixels used local paint order.
The new seam prepares local motion at its native owner and adds actual cached
occlusion/input-delivery regression. Joined test compilation passes; all seven
native vendor checks and the 14 captured frames remain required.

## Build discipline

Use the saved realized Nix shell and pinned Rust 1.97.1, with a persistent
worktree-specific Cargo role graph and the tracked provenance/cache wrapper.
Host capacity permits must not choose the role graph's identity: moving
between pooled slots caused avoidable cold rebuilds. Start with at most four
local builds, two compiler jobs each, and check memory headroom. Remote work
is admitted from a live process census, not from an assumption that an SSH
timeout killed it. Preserve receipts and source commits before long tests.

## Linux report 7 and latest measured gates

This section supersedes pending statuses above only for the exact revisions
and selectors listed here. The report was read in full on 2026-10-06. Its
Automatic-Schedule-Planner checkout is Next.js 15 / React 19, approximately
30 source files, without `node_modules`. At canonical `6c98128cc`, the report
demonstrates a configured 850-row publication and working source/graph reads,
but no-environment headless TypeScript admission, every lexical search, and
automatic GUI visibility still fail. Successful configured indexing does not
close those separate paths.

- **Headless toolchain gap:** bundled Node admission already exists in the
  shared engine host. `EmbeddedCompilerEnvironment` does not forward the
  process search path, while desktop has a separate global TypeScript finder.
  The repair must unify paired toolchain discovery below both entry points,
  preserve project-local and explicit pin precedence, and run a cold CLI/MCP
  index with no overrides. Source tracing is complete; runtime acceptance is
  pending. Companion `locald` installation and helpful missing-helper errors
  belong to the same installed gate.
- **Search:** thirteen provider/composition errors and an outer string-only
  query boundary discard distinct causes. The exact publication/search
  mismatch is not yet reproduced or diagnosed. Cause-preserving typed errors
  must accompany a real warm/cold search repair, not replace it. Cross-file
  alias references, source byte-offset presentation and `outline .` also
  remain open.
- **Automatic GUI publication:** an actual completed index must appear
  without navigating away and back. Retained-local Graph/Source tests and
  index-preflight tests do not certify this subscription. Cold launch,
  held completion, retry and owner replacement need explicit production
  fixtures and installed GUI observation.

At joined `19618e7ea6`, all seven native vendor deferred clip/cache/accessibility
tests pass. Native desktop test compilation also passes. The primary film
produced all fourteen full PNGs, Reader crops and native ledgers under
`gui-audit/captures/reader-native-19618e7e-20261006`, then failed its final
zero-requested-frame assertion. Root inspected every full PNG: prior departure
prose duplication is absent, but breadcrumbs overlap the current title on
same-clock expansion, and the ready incoming plate is blank at the sampled
opening frame. Visual acceptance is **FAIL**. The harness advanced time without
rendering intermediate platform frames; the repair will draw actual continuous
frames, inspect destination ink as well as departure ink, and retain a bounded
strict idle assertion. Header topology is a separate product repair.

The same exact desktop binary's focused joined selectors pass 21 and fail 5:
seven preflight, six queue, three native hint and four native receipt checks
pass; deferred admission passes one and fails three; dependency Back and
Reader native-hint selectors each fail one. The Reader fixture lacks native
accessibility and three admission fixtures request cards before actual module
disclosure; those premises are being corrected without planted registrations.
Dependency Back loses real native focus and requires a product repair at the
child's painted mount boundary. Logs and binary hash are in
`gui-audit/builds/native-joined-19618e7e-slices-20261006/summary.json`.

The Graph worker's broader checkpoint `ba4702209b` passes 32 and fails 16;
its new real unavailable-resource focus/Tab-boundary law passes. The remaining
failures are still open, including stale-frame temporal premises and a
nonunique `keep_local` fixture coordinate. No full Graph-suite or installed
GUI success is claimed.

Root source admission at `549428bda` passes nine budget laws and eight runtime
selectors, including a 10,880-file, 85 MiB cold/warm structural scan. Policy v4
at `2b779a5d9` also passes the actual warm-frontier aggregate-fact-page charge
regression: an edited file fits alone but is correctly refused when unchanged
files' retained facts exhaust the project allowance. These are parser/storage
gates, not full semantic compilation of that corpus.

Review found two additional production commit defects: cached files lost their
complete facts during warm commits, and shrinking files left obsolete pages in
the selected relation. At `02c0e017d`, the production preparation/owner commit
and cold-open regression passes with all 900 functions and exact absolute
source lines retained, then proves shrinking, unavailable and deleted files
retire old rows while preserving other files. A separate exact-provenance
refusal test at `797688c9c` is checkpointed and awaiting runtime validation.
Fact construction precedes coordinator charging; these limits still do not
constitute a hard peak-RAM bound.

The compiler diagnostic library at `1c764d19b` passes 282 tests, with one
ignored. The full scoped MCP library at `870cf402be` passes all 93 tests,
including actual typed-client-error adapter checks and human protocol text
that must never be promoted to a compiler fault. Neither suite demonstrates
that the original `Fragment(Prepare)` lowering cause is fixed.

Cold capture replay remains blocked on provenance: validating a caller-supplied
closure's contents does not prove that the claimed generation selected it.
The exact authenticated predecessor descriptor must bind the closure ID and
capture root, including a forged hybrid token with a current sequence and an
older valid closure sharing the same workspace manifest. That negative test
and repair remain pending. No cutover or release approval is requested.
