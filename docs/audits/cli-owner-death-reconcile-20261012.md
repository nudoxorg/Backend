# CLI owner-death recovery and exact saved-key reconciliation

Linux report 18 describes six immediate commands failing after owner death on
historical f30aae099. This source atom does not claim that report was rerun.
The separate N16 wait atom is the parent 7eabd015; its eight controls remain
unrun here. No build, local daemon, compiler, or remote runtime was launched.

The process can authenticate one replacement connection per invocation, shared
across initial dial, command execution and optional waiting. Runtime readiness
is only a probe: `Session::connect_with_context` must admit the actual private
endpoint peer before any replacement command. Startup and authentication share
one original absolute startup deadline. Mutations detach that startup scope;
the startup context then retires. Each journaled Start installs its own absolute
mutation-derived 900-second reconciliation scope before the original send; it
does not bind over that first mutation's existing transport policy. Replacement
startup fits both its shorter runtime bound and this retained total. It cannot
reuse an expired startup scope or mint another 900 seconds after peer loss.

Only bounded Health, first-page Search before pinned exchange, and an exact
durable operation Status read can repeat after typed peer teardown/absence.
The read's original RequestContext survives replacement, including a default
Search's existing deferred-ACK budget policy. Imported continuations and pinned
query loss cannot change revision. Timeouts, would-block, invalid input, protocol,
authority, explicit cancellation and other failures do not trigger replacement.
Non-index mutations do not replay; their disconnect remains an unknown outcome.
Non-Unix recovery authenticates an already available owner; it does not promise
a cancellable bootstrap on the existing synchronous platform startup path.

For an ambiguous caller-journaled Start, replacement first reads the saved key.
The shared client checks key/package/intent/digest, the actual private saved claim
and frozen authority attachment. The original index invocation retains its
request lock. The internally produced `JournaledIndexStart` borrows that actual
locked File, claim, admitted root and directory. Its fields are private, it has
no public constructor or clone API, and its higher-ranked callback lifetime
cannot escape the held journal scope. The simple `index_with_journal` API remains
an adapter over the same implementation; recovery receives `IndexJournalDispatch`
instead of a boolean. Reconciliation derives its original payload from the
capability and accepts no alternative command. The CLI also admits its frozen
connection attachment against the capability before the original send. Known receipts, including Failed, return unchanged. Other unresolved
or outside-window evidence stays uncertain. Only the journal's existing Unknown
or interrupted-capture policy can repeat the identical Start payload/key; it does
not rerun Add or allocate a different identity. Claim/attachment checks bracket
the exchanges. A failed replacement or receipt admission keeps the original claim
and reports its exact key with the original typed cause and resumable progress call.
Cancellation retains its original error classification.

Waiting reuses its existing absolute context and exact last observation. A second
peer loss cannot create a reconnect loop. Startup/search progress uses the wait
atom's bounded `try_send` stderr reporter; callbacks cannot block on redirected
stderr, and no progress enters machine stdout. Generic user callbacks are not
claimed preemptable.

Eight runtime source controls are provided:

- CLI actual private Unix listener/authentication/frame path, correlated Health
  replacement, separate passive probe, and new session request sequence.
- CLI shared driver and real private caller journal with ambiguous Start followed
  by exact Accepted/Failed/Unknown evidence; known outcomes do not start again and
  Unknown uses every original field. A deterministic expired startup scope and an
  actual 23-second scripted original send distinguish both old-scope expiration
  and renewing the mutation total after loss. No fake native publication is asserted.
- CLI exclusion controls for pinned loss, timeout/would-block, invalid input,
  cancellation, protocol/authority refusal, and unkeyed mutation; cancellation
  before exchange admits neither a command nor a replacement.
- CLI replacement refusal retains saved claim/key/cause and exact progress call;
  original waiting deadline survives and replacement is at most once; a supplied
  total that expires during the first send admits no replacement and keeps its key.
- Client exact same-key reconciliation shares the existing unknown/interrupted
  policy; Failed, unresolved publication and outside-window controls only observe.
- CLI a foreign connection attachment cannot consume the held Start capability,
  send a command, authenticate replacement, or alter either caller journal.
- Client foreign same-key status/start evidence, changed private authority and
  replaced claim are refused before replay; external evidence is not overwritten.
- Client a concurrent same-request invocation is refused by the actual held File
  lock before transport, while another project dispatches independently. The
  capability then reconciles without reacquiring its own lock.

The new controls use admitted constructed owner DTOs except the explicit socket
control. They prove policy/identity/projection if run, not genuine locald/compiler
publication or a live owner-kill acceptance. All eight are native **UNRUN**. Two
compile-fail doctests specify that a capability cannot escape the callback or be
manufactured from a caller command, paired with a positive scoped API example;
all three compiler controls are also **UNRUN**. Required broker selectors are
`backend-cli owner_recovery::tests::` (5), `backend-client
index_operation::tests::reconnect_reconciliation_` (2), and `backend-client
index_operation::tests::held_start_capability_` (1), plus the client
`JournaledIndexStart` doctests (3), followed by composed full
CLI/client/present/MCP gates and same-source real CLI owner-death and Add/wait runs.
Original cancellation, N15 overlap, and 6000/30-second publication controls remain
unchanged; their historical failures are not converted into passes by this atom.
The existing present fault-class control also includes the new keyed delivery
variant across all details/budgets; nested startup recovery is cleared because
replacement readiness cannot authorize retrying the original command line.
