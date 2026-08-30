# Borrowed canonical-root checkpoint

## Custody and source receipts

- Controller writer: `/root`, real runtime `gpt-5.6-sol` / `xhigh`, isolated worktree `/Users/mileswirht/.config/codex/worktrees/ab6a/backend`, branch `codex/performance-data-structure-closure`.
- Registered manager: `/root/canonical_byte_local_closure_terra` (Carson), real `gpt-5.6-terra` / `xhigh`; interrupted before controller takeover to keep one writer.
- Luna receipts preserved in `salvage-ledger.md`: `luna_vertical_1` unresponsive/interrupted; `luna_vertical_1_retry` source-only/completed; `luna_vertical_2` partial writer/interrupted; `luna_root_checkpoint_1` source-only/interrupted. None is represented as approval.
- Legacy evidence was consulted read-only at `/Users/mileswirht/Downloads/backend/workspace2/crates/nudox-root/src/root_view.rs`, 398 lines, SHA-256 `37e4c839dfb3f44974a66beb6d5459da5a57e22520b9cb67b959e32380e572e9`. No file or build in the saved checkout was changed or run.
- No source-isolated approval is claimed at this checkpoint. The next review must use the corrected dispatch-only Sol/low -> registered Terra/xhigh custody in `review-custody.md` and must record a nonempty runtime reviewer task/receiver ID.

## Retained and rejected mechanisms

- Retained: one 8-byte typed header and a single borrowed slice of exact 63-byte `RootWireRecord` rows; each row nests the object-owned 46-byte descriptor wire type.
- Retained: closed typed `ParentWire::{Absent, Present}`; invalid tags and nested schemas receive an error-only diagnostic rescan after the full typed cast rejects.
- Retained: every populated-row authority byte is checked; the witness retains one `ContentAuthority<DomainTag>` and projects the 31-byte payload with `authority.bind`. The empty state is a distinct closed variant.
- Retained: exactly one fallible transient `Vec<u32>` parent-coordinate lane, logical high water `4 * declared_rows`, released before witness return. Floyd detection plus destructive path completion bounds the forward-chain control below four hierarchy steps per row.
- Retained: caller-owned `ClosureScratch` and the monotone existing locality cursor for both complete and selected scans.
- Rejected: retained native `RootRow`, descriptor copies, a second row arena, `Box`/`Arc`/`dyn`, unsafe projection, post-validation descriptor reparsing, digest reconstruction, warmed allocation, and silent parent-authority defaults.

## Exact structure and work evidence

- Header: 8 bytes, row: 63 bytes, row alignment: 1, descriptor offset: 17, descriptor content/length/schema/kind offsets relative to row: 17/49/57/61, parent tag/key offsets: 8/9.
- Retained witness state: borrowed input facts plus one closed borrowed-row state; no row, parent, descriptor, or body allocation survives ingress.
- 100,000-row forward-parent adversary: authority reads `100000`, adjacent-order comparisons `99999`, parent resolutions `100000`, scratch high water `400000` bytes, hierarchy steps asserted `<= 400000`.
- Independently executed constant-memory control at 2,048 rows matches the exact forward-chain sum; the exact 100,000-row projection is `5,000,050,000` parent steps. The one-row endpoints are 4 transient bytes / 2 hierarchy steps versus 0 transient bytes / 1 constant-memory step.
- Strongest counterexample retained: a constant-memory per-start parent walk is attractive for a one-row artifact but becomes quadratic for an arbitrary forward-parent chain; zero transient allocation would cost over five billion parent reads at the required 100,000-row adversary.

## Gates and remaining public red

- `cargo test -p nudox-root --locked --offline`: pass; 26 unit tests, 1 canonical-writer integration test, 4 locality-validation integration tests, 0 failures.
- `cargo clippy -p nudox-root --all-targets --locked --offline -- -D warnings`: pass.
- `cargo fmt -p nudox-root -- --check`: pass. The workspace-wide formatter has a pre-existing chief-test formatting delta outside this frozen root-only card, so this checkpoint does not rewrite that chief-owned file.
- Chief red now reaches the intended next boundaries only: missing `nudox_hydration::plan_borrowed`, missing `Need::bind_borrowed`, and missing `LocalObjectProvider::bind_verified`. Root symbols and borrowed view construction compile through the public journey.

This closes only the borrowed root/locality checkpoint. It does not claim borrowed hydration, sealed verification binding, operation execution, hostile review, Miri, or full-workspace closure.
