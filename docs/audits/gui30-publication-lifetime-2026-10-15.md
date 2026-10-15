# GUI30 publication lifetime source audit

Source-only change based on canonical `66a07e52acd4b180ead00889a62d183d44794890`.
Actual GUI30 ran frozen source `3183000a7788d20c2d4bef362a68869a24500cac`,
ELF SHA-256 `af7d79ab988dc2cbe2991a2e8deda2a210d6d3184c03f0c6f6a0f48b5734c183`.
No execution of this change is claimed.

## Actual evidence and its limits

Personally read the bounded raw receipt, its operation/project observations,
and publication trace under
`/Users/mileswirht/Downloads/nudox-active-20261013/gui-execution-inputs/gui30-review01-actual-evidence01/`.

| Evidence | SHA-256 |
| --- | --- |
| `actual-run-classification.json` | `47b261e6bc81b9bd6254387045c1235b10b2f8dea63bce287cf6d24b4fa49ce3` |
| `runtime/focus-diagnostic/receipt.json` | `b5022d0c57056d089aa82ba1d719f95ee33345acca1a0335d9206bea47ae6122` |

Actual input typed `/work/excalidraw` and clicked enabled Add. The exact saved
operation was `c7fec4991d4497110907cc84cec969d6ee99916b06b6ee7b3e078078f7d17aa3`,
request digest `f9cc0a0011cbc576f8848af6735e83f7031a269f28015d6350f39ba05797a2b3`,
interactive local package `/work/excalidraw`, ticket 1. Its last admitted owner
receipt was `ACTIVE/scanning`. The project later changed from `Indexing` to
`Unconfirmed` at diagnostic monotonic time `1383941725506827`, retaining that
exact receipt and saying the interrupted request might still be running or
might have completed. The retained receipt is not current liveness after
observation degraded.

The raw AX project observation contains both "Outcome unconfirmed" and
"The index could not start ... publication Ordinary budget expired after 8 s".
There is no dedicated publication-error PNG. The trace records ordinary-budget
reconnects at `t_ms=39382.559` and `49942.922`, then failure at `59733.579`.
These observations support exhausted bounded publication recovery. They do not
establish that idle subscription age alone caused the actual failure.

The later administrative stop charged growth `369,713,363` bytes against the
unchanged `268,435,456` byte cap. That stop is distinct from publication
unavailability. It interrupts the owned scope and cannot establish application
index refusal, terminal index failure, publication, search/graph/history
acceptance, or complete project acceptance.

## Frozen and canonical source findings

`git diff 3183000a...66a07e52 -- apps/desktop/src/host/observation.rs
apps/desktop/src/runtime/owner.rs` is empty. Canonical therefore still had the
actual host/owner observation path. Client production observation code was
also unchanged between those commits; its differences were test-only.

* The [host observer](../../apps/desktop/src/host/observation.rs) used the last
  producer confirmation plus a 30-second ordinary recovery window. It created
  a fresh client control on each operation/reconnect against that absolute
  deadline. An ordinary error's reported allowance was the interval from that
  operation's entry to the remaining deadline. "8 s" is therefore compatible
  with a remainder, rather than a configured eight-second subscription TTL.
* The [client control](../../crates/client/src/subscription_observation.rs)
  already begins a new hydration for each Open/Resume and a new ordinary
  Renew operation. Reusing it does not inherently spend all future operations'
  work budget during idle time. No duplicate client reset fix was needed.
* [Reset hydration](../../crates/client/src/reset_hydration.rs) and
  [reset budgeting](../../crates/client/src/reset_budget.rs) check the
  provisional deadline before an authenticated descriptor may bind the larger
  row/page-derived allowance. Its deadline stays anchored to original work.
* The [producer lease host](../../crates/local-service/src/service/lease/host.rs)
  calls its source for Open/Resume. Renew only checks the retained exact lease
  cursor; it does not call producer Subscribe or establish latest-root freshness.
  Slow source/encoding work must still pass active lease admission at commit.
* [Owner failure](../../apps/desktop/src/runtime/owner.rs) withdrew attachment
  and mutation admission, but discarded the complete root. The
  [store](../../apps/desktop/src/runtime/store.rs) described every such failure
  as inability to start. [Host Retry](../../apps/desktop/src/host/owner.rs)
  dropped the host and called the starter even when only the publication
  subscription had failed. That could stop an embedded owner with an accepted
  operation whose outcome was still unconfirmed.

## Change and authority boundaries

`ObservationLifetime` separates idle subscription time, last producer
confirmation, and active ordinary recovery. Idle/poll intervals do not spend a
new work term; reconnects/reacquisition keep their existing active deadline.
Successful Renew ends its control work term without moving producer freshness.
Freshness suspension still fences stale reads/actions. No timing constant is
increased, and a producer that stops responding can still exhaust recovery.

Authenticated reset ceilings remain in the existing observer/client paths.
Reconnect does not clear them, bind them to a new clock, or authorize another
reset allowance. Only a completed publication clears its completed reset.
An explicit user Retry begins a new Open from the last complete retained
root/cursor; it does not continue or resurrect an expired partial reset.

Publication failure retains a complete root and live-host authority solely for
that explicit Retry. It still cancels the failed observation socket and old
mutation scope. Retry rotates attachment, cancellation, and action generation,
then requires a fresh correlated Open, complete proof, and any final Ack before
read/action admission reopens. The worker keeps the exact live host and never
replays Add or indexing. Existing `Unconfirmed`/exact-operation reconciliation
rules still require checking the outcome before another mutation.

`OwnerHostLifetime` is captured atomically when the starter publishes its host.
The observer validates it at entry and the worker validates it before host
reuse. An explicit source/state replacement invalidates it even when root bytes
match, so another source's pending subscription cannot inherit the old host.
Failed initial root admission without a retained complete root, genuine startup
failure, panic, or confirmed attached-owner loss still takes the cold path.

The UI has a distinct `Observing` phase and a pending notice while proof is
unavailable. It retains predecessor bytes without current action/read
authority. Exhaustion says "Index updates are unavailable"; it does not claim
the proven-started index could not start or invent a terminal operation result.

## Verification status and review targets

`git diff --check` passed. No native build/check, cargo test, Nix evaluation,
owner/index execution, or GUI execution was run. Added tests are unrun source:

* `host::observation::term::tests`: long idle lifetime, a fixed recovery term
  across reconnects, and 1,000 successful controls that never move producer
  confirmation.
* `host::observation::tests`: repeated Renew cannot retain stale read/action
  admission; exhausted slow publication waits for explicit Retry and keeps
  the complete root. Existing blocked-frame and close-interrupt tests remain.
* `host::owner::tests`: explicit publication Retry retains the exact host and
  its accepted operation marker; genuine startup panic still invokes the
  starter again.
* `runtime::owner::publication_tests`: retained root cannot serve without
  proof, old action generations stay rejected after recovery, and source
  replacement retires pending Retry even at the same root.
* `runtime::store::owner_failure_tests`: actual watcher path retains page
  bytes and an Unconfirmed exact operation, publishes truthful pending words,
  and sends no IndexProject during publication Retry.

Required existing client regression coverage includes
`observed_authenticated_reset_pages_and_ack_share_one_expanded_deadline`,
`a_reused_observed_control_starts_fresh_renew_and_resume_budgets`,
`observed_ordinary_admission_uses_the_minimum_fixed_deadline`, the provisional
descriptor-boundary tests in `reset_budget`, and observation cancellation tests.
They were neither modified nor run here. Root owns admission of subsequent
native validation and integration. No MCP/query-window/client-lib source was
changed. The main checkout's sole `.config/nix/tools.nix` edit remains at
SHA-256 `71d71b7dc7dd074db175118af6f74b8216c8a842ba745f7ae5707c991b1874d3`.
