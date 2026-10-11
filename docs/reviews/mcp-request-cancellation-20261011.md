# MCP request cancellation: source freeze, 2026-10-11

**Native status at the original source freeze: UNRUN.** This slice had not been compiled, linked, or executed.
The shared native broker has not admitted a fresh destination for this source.
Rustfmt parsing/checking and `git diff --check` pass; those are source gates,
not proof of Rust type checking or transport behavior. Root must complete the
composed exhaustive `ClientError` matches before native admission.

The status statements below describe that original freeze. The later native72
MCP result and its source correction are recorded at the end of this document.

## Change and scope

Previously stdio read, dispatch, and write ran synchronously on one thread,
and HTTP kept cancellation behind the same `Server` mutex as a blocking query.
An observer that always returned true could not cancel an in-flight request.

Each valid JSON-RPC request now acquires a session-local registry lease before
queueing, connecting, or waiting for the server. Its `RequestContext` carries
the cancellation reason, original read deadline, exact retained local socket
interrupt, remote notification, and bounded preparation observer. Numeric and
string ids are separate; duplicate active ids are refused. Completion removes
only the same lifetime and detaches its socket before id reuse. Unknown ids
create no cancellation tombstone. Equal ids in different sessions never share
authority. The first cancellation reason is preserved.

Stdio has one independent owned reader/router thread, a 32-item bounded actor
queue, and at most 32 registered request lifetimes. Valid cancellation
notifications bypass the queue. Framing retains the existing 64 KiB MCP request
limit and JSON depth admission. Queue saturation is a terminal intake refusal:
it closes the registry and interrupts pending requests, rather than blocking
the router behind work or building an unbounded refusal queue. No per-line
thread or Tokio task is created. EOF is only a read half-close: already admitted
finite piped requests drain and their replies are flushed. Failed output,
framing/intake failure, and actor teardown close the registry with Shutdown.

HTTP authenticates and routes cancellation before taking the `Server` mutex
and before claiming an ordinary blocking slot. Existing bounds are retained:
64 sessions, 64 ordinary blocking slots, and 32 registered requests per session.
Session deletion signals shutdown without waiting for a query mutex. A guard
in the response future cancels its exact blocking request on future drop; it
is disarmed after normal completion. A new real TCP disconnect law is required
to establish the Axum/Hyper drop behavior, in addition to the controlled
future-abort law. Neither has run yet.

The existing explicit read/mutation classification activates the read deadline
before owner startup and outer preselection reconnect. Replacements use the
same absolute deadline; selection, writes, replies, and preparation consume its
remaining budget. Mutation contexts acquire cancellation without installing
a read deadline. The previously pinned preparation disconnect law remains:
the same selected Search/GraphQuery cannot reconnect and select a new basis.
Explicit cancellation is `RequestCancelled`, overrides the induced socket
error, retires a poisoned lease, and never enters MCP reconnect/replay.

Local dials use bounded slices with cancellation checks and the remaining read
budget. A socket is bound before peer authentication. Remote transport selects
against the same request notification while connecting, handshaking, sending,
or waiting for the response, and retires cancelled active authority. Startup
cancellation stops this caller's wait and hands the owner candidate back to
its existing shared lifecycle. Cancellation sends no preparation worker or
GraphQuery control command. It does not cancel another caller's shared lane.

MCP preparation observation uses `try_send` into one process-wide queue of
eight events. One stderr logger thread can block on a host that does not drain
stderr; query work never waits for that logger and stdout remains JSON-RPC.
The public arbitrary FnMut observer remains synchronous and must not block.
Filesystem metadata, spawn, and kernel process retirement are individual OS
operations and are not claimed to be preemptible.

## New native laws awaiting execution

- `request_context::{cancellation_before_connect_and_during_exchange_has_exact_reason,
  completion_retires_socket_and_late_bind_observes_prior_cancellation,
  one_read_deadline_survives_replacement_and_progress_backpressure,
  cancellation_wakes_remote_select_even_when_cancel_precedes_wait}`.
- `remote_command::tests::request_context_cancels_real_remote_handshake_and_withheld_response`.
- `caller_context_cancels_preparation_without_sending_shared_worker_control`.
- `jsonrpc::requests::tests::{exact_ids_unknown_ids_id_reuse_and_sessions_cannot_cross_cancel,
  malformed_controls_never_acquire_cancellation_authority,
  registry_capacity_shutdown_and_disconnect_guard_are_exact_and_bounded}`.
- `stdio_reader_cancels_an_active_call_then_allows_completed_id_reuse` and
  `stdio_finite_pipe_half_close_drains_owner_reads_and_flushes_replies` use
  actual Unix socket pipes. The latter closes stdin before permitting output.
- `http::tests::{cancellation_bypasses_locked_server_and_full_http_query_capacity,
  dropped_http_response_future_cancels_its_admitted_blocking_request,
  actual_http_client_socket_drop_cancels_its_pending_blocking_request,
  deleting_http_session_cancels_pending_requests_without_waiting_for_server}`.
- `request_cancellation_survives_socket_disconnect_without_replay`,
  `original_read_deadline_survives_preselection_replacement_dial`, and
  `mutation_cancellation_context_does_not_install_a_read_deadline`.
- `cancelled_before_startup_dispatch_creates_no_state_or_candidate` and
  `cancelled_readiness_wait_hands_off_shared_owner_without_killing_it`.

After broker admission, the initial composed native gate is:

```text
cargo test -p backend-client -p backend-runtime -p backend-present -p backend-mcp --lib
```

Run it only in the broker's admitted fresh destination and retain its complete
result. It covers the listed laws and existing preparation/reconnect/process
suites; it has not been executed for this slice.
An installed MCP process must also be exercised with a real preparing owner,
explicit notification, finite `communicate(input)`, closed stdout, and HTTP
socket disconnect. Preserve the failing first receipts; do not reindex or
alter the scope to turn a failure into a passing demonstration.

## Brutal self-review and residual gates

The source audit checked the cancellation-before-bind race, bind-versus-finish
mutex serialization, first-reason stability, completed-id reuse, unknown ids,
session isolation, bounded overload, EOF draining, mutex/slot bypass, remote
future wakeup registration, selected-basis refusal, and mutation classification.
It found and fixed a remote test initially placed outside its test module,
missing budget tightening at dial/preparation boundaries, a missing replacement
post-call context check, and test mutex guards held across async waits. These
findings show why a rustfmt pass alone is insufficient. Luna review and actual
native compilation/execution remain required; this document claims no native
pass and no measured cancellation latency.

Prerequisite source: `d398be1c36adf6657ea40a139e3acf7cb32e1a93`,
`1529d9dfdb075f60316a7e54611405861ef23353`, and
`c5caa5b6d733c380ae8cc5a2db2324341a818f55`, based on
`edd2c9735e79b8ffe7d0add3bb5960f526da2968`. The cancellation commit is
an isolated slice on those postimages. The root checkout and its dirty Nix
configuration were not modified.

## Ownership correction after Luna source review

Luna found two early-exit holes in the frozen cancellation slice: the outer
reconnect took its previous product before a fallible context check, and
SessionEndpoint took continuation state before fallible cancellation/budget
checks. Either could drop authoritative certificates on a cancellation race.
The correction parks SessionEndpoint state before all fallible operations and
takes it only into a fully connected replacement. The outer transfer retires
the taken product through its endpoint on check failure. The same audit also
fixed replacement context admission failure, which now retires its newly
restored authority before returning.

New deterministic laws, all native **UNRUN**, cover cancellation and zero read
budget at the exact pre-dial transfer, cancellation during replacement
admission, and both previous-product and already-parked SessionEndpoint state.
The laws are `cancellation_at_pre_dial_transfer_retires_previous_before_returning`,
`cancellation_during_replacement_admission_retires_restored_authority`, and
`cancelled_endpoint_admission_preserves_previous_and_parked_continuation_proofs`.
The endpoint law obtains a retained query proof through the normal Session
page/certificate path, then checks byte-for-byte portable proof retention and
same-owner legacy continuation admission after the refused replacement.
No timing race or manufactured insertion into the private continuation map is
used. Formatting and whitespace checks pass; EOF and cancellation controls
are untouched.

## Native72 full-suite failure diagnosis and source correction

The broker ran the complete MCP library suite on
`452645a8141faa80abc4d07c8db0302f727d438d` (tree
`aa165ea2f984b163fc491f9ce59d454a507918cb`): **172 passed, 4 failed**, with
176 selected and no ignored tests. The raw receipt remains in
`/private/tmp/sol61-ilo-real-cli-mcp-20261011/native72`. Its stdout SHA-256 is
`d80f3993ff60e3cb0f70efcc169387d84f30a93e176f598ce7ba26e67163a5e3`;
stderr SHA-256 is
`f63cdc76d6e7d993040d497436a663be694c5f00d8108330bd2debae2ed5abbc`.
No failing receipt was replaced or hidden by a rerun.

The correction separates product failures from fixture-contract mismatches:

- The HTTP fixture expected capacity refusal during authentication Pending.
  Production correctly reports the first admission boundary before taking an
  ordinary slot. The revised real-wire law checks bounded, correlated Pending
  with both occupied and free capacity, then authenticates and tests actual
  Ready-session saturation and recovery. This exposed a product bug: ordinary
  capacity refusal lost the admitted id. Refusals now retain numeric/string id
  and session correlation through the whole-body budget encoder. Saturated
  notifications return HTTP 503 without inventing a JSON-RPC response. Both
  refused ids can subsequently be reused; metadata and empty ping semantics
  are checked independently. A release guard prevents an assertion failure
  from stranding the authentication initializer behind a disconnected barrier.
- The durable cancelled index-operation fixture incorrectly expected
  `isError:false`. The shared presentation contract already makes a failed
  indexing goal a refusal. The correction preserves accepted, partial, and
  cancelled cases on both started/status routes at summary/full detail. It
  checks exact key and request digest, the retained terminal state, cancelled
  `rejected/refused` cause, lack of a publication/source/compiler partition,
  and the exact fresh-attempt call without a consumed operation key. Existing
  partial receipt, source-digest partition, compiler remedy, and cause checks
  remain intact. No production error flag was weakened.
- The real stdio id-reuse fixture compared the ping result to `{}`, discarding
  legitimate `_meta`. Its replacement checks id 7, no error, no semantic
  payload, complete newline framing, the actual 171-byte line, and every
  wire-budget field against independently counted bytes. Initialize and
  explicit cancellation replies receive the same full-line accounting checks.
- The startup fixture already expected a typed tool refusal. The actual
  product failure was that missing companion discovery masked an existing
  invalid explicit private parent on Unix. Startup now reuses the platform's
  read-only private candidate admission before discovering a companion.
  Existing unsafe parents/children are refused without creation, permission
  repair, durability fences, or authority replacement. Absent parents/children
  and default application-owned repair remain deferred to the owner. The
  durable initializer repeats all admission checks; the snapshot conveys no
  filesystem authority. Windows retains its existing startup initialization
  order and uses the same read-only platform admission seam in the fixture.
  The real stdio law keeps its original fixture and planted credential, and
  now asserts the exact original cause, typed correlation, unchanged authority,
  no endpoint, and no durable setup side effects.

Additional source laws cover private-candidate absence, existing state,
public parent/child refusal, unchanged modes and credential, symlink rejection,
invalid explicit-parent priority over a forced missing companion, absent-state
deferral, and a genuine captured default layout requiring permission repair.
The resolver seam never executes a child and needs no global environment edit.
Existing endpoint attachment, pre-dispatch cancellation, EOF half-close,
request ownership, replay controls, and shared preparation workers are unchanged.

**Correction native status: UNRUN.** The repair worktree ran no cargo build,
native test, executable, or installed product command. Rustfmt parsing and
whitespace checks are source gates only. Rustfmt check passes on all changed
MCP/runtime files; comparison of platform formatter deltas proves that only
the eight inherited formatting differences remain. Unrelated pre-existing
platform formatting is preserved. The source review caught and corrected an
initial assertion that omitted the existing RuntimeError display prefix and
an initial default-state fixture outside its genuine captured layout. It also
checked exact request-lease retirement before refused-id reuse, notification
response suppression, first authentication admission, and repeated owner-side
private-state validation. Root must review this atomic correction and ask the
sole native broker to run the complete MCP suite plus the new platform/runtime
laws in an admitted fresh destination. Windows compilation/execution and real
installed preparing-owner receipts remain separate gates.
