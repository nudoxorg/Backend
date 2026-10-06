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

This is installer acceptance, not compiler or GUI acceptance. The stock CLI
acceptance already reports zero admitted oracles and refuses a tiny TypeScript
project with `TypeScriptCompiler configured: None`; adding project-local
TypeScript and testing actual MCP/cold-restart behavior remains in progress.
`brew test` is separately blocked by this Nix-provided Homebrew's read-only
vendored Gem marker, although its three help assertions were exercised directly.

GitHub release ID `404084754` became draft twice. Republish at 00:52:47 UTC
restored an anonymous ranged tarball GET (206). The second state change was
observed at the time of a formula asset upload; the exact CLI upload code has no
release-state mutation, so its cause is not established. Never infer public
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

The close integration at `ececf4d9` passed eight tests and failed six: the
remaining failures exposed gpui-component's focus trap dropping its base's
accessibility hooks. The general wrapper repair forwards native roles,
properties/actions, synthetic children and inspector/source metadata; a real
dialog/remount/Tab/Shift-Tab regression was added. Its warm rerun is pending.
The lifecycle integration passed ten and failed one before its retained-library
precondition; the fixture now delivers the actual owner publication before
checking fresh failure and Retry. Its rerun is also pending.

Publication, graph selection/camera continuity, Settings native ownership,
reader plate lifetime, and responsive Shelf repairs remain distinct source
packets until their exact native tests and image captures pass. No candidate
application has yet passed live GUI computer-use acceptance after this restart.

## Current compiler/index gates

Paged source facts and a lazy verified consumer have source checkpoints; the
local-service check passed before the typed compiler-failure DTO was added.
Capture-before-compilation, typed terminal refusal persistence, complete
overflow-file reads, and actual 84+ MiB ingestion still need one integrated
matched-binary run. Raising a quota alone is not storage or memory acceptance.

The TypeScript host has an embedded relocatable checker driver and typed
project-local toolchain admission. A relocation selector passed once; newer
host tests exposed production type errors that were fixed and are being rerun.
TSZ's immutable per-program options are vendored in a separate packet. Complete
config/import/library/ambient/reference closure, including negative resolution
witnesses, is required before its production dispatch cutover.

Typed fragment failure DTO v17 has a source checkpoint, but its first compiler
run failed before tests. Closed nested operands and bounded wire serialization
are being reworked; this packet is not a passing API or an installed fix.
Requests class-source reopening, checked empty Python modules, Go closure
authority, and concrete cross-file TypeScript call/reference joins also remain
under implementation and native validation.

Npm's post-integrity live run streamed 25 packages with zero failures and 31,288
selected version rows. Ten npm tests, bounded-manifest and chunk-integrity
regressions passed in the later exact receipt. The bounded version window is
explicitly partial; this does not establish complete all-version ingestion.
CLI/MCP parity by itself also does not establish semantic correctness: the
recovered Nest and Requests runs agree on missing concrete reference edges.

## Build discipline

Use the saved realized Nix shell and pinned Rust 1.97.1, with a persistent
worktree-specific Cargo role graph and the tracked provenance/cache wrapper.
Host capacity permits must not choose the role graph's identity: moving
between pooled slots caused avoidable cold rebuilds. Start with at most four
local builds, two compiler jobs each, and check memory headroom. Remote work
is admitted from a live process census, not from an assumption that an SSH
timeout killed it. Preserve receipts and source commits before long tests.
