# Meeting blockers and durable recovery

Recorded 2026-10-05 after the local restart. This is an acceptance ledger,
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
Its six Mach-O files pass strict signature verification before installation;
this does not establish compiler or GUI readiness. Public asset ranged GETs
succeed. The existing public Homebrew recipe still permits relocation to
rewrite the signed application. A byte-preserving install repair needs exact
local validation, publication and a fresh public Homebrew install test.

The remote Mac's SSH attempt timed out while local Tailscale was stopped.
Its process state and unrecovered install receipts remain unknown.

Two user-restored GUI instances are running. Native computer-use input is
held while the user is active; source reviews, owned fixtures and GPUI tests
continue. Screenshots of the running r3 Settings page are observations, not
acceptance of the new candidate.

## Build discipline

Use the saved realized Nix shell and pinned Rust 1.97.1, with a persistent
worktree-specific Cargo role graph and the tracked provenance/cache wrapper.
Host capacity permits must not choose the role graph's identity: moving
between pooled slots caused avoidable cold rebuilds. Start with at most four
local builds, two compiler jobs each, and check memory headroom. Remote work
is admitted from a live process census, not from an assumption that an SSH
timeout killed it. Preserve receipts and source commits before long tests.
