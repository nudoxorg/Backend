# Dogfood telemetry relay v1

This is a new internal dogfood protocol. It is not a claim of compatibility
with the unrecovered F7 wire specification or the paid-billing system. A grant
is an operator-issued authorization to send telemetry for one tenant and node;
it does not attest a paid billing entitlement. Deployment requires review and
the NixOS VM, VictoriaMetrics, alert, and systemd custody proofs.

The relay binds only to 127.0.0.1. Its private JSON grant registry is an array
of at most 128 objects with exactly these fields:

```json
{"tenant":"nudoxorg","node":"dogfood-node","token_sha256":"<lowercase SHA-256 of a random bearer token>","activation_id":"<unique 32 lowercase hex digits>","scope":"telemetry:write","not_before":0,"expires_at":1800000000,"revoked":false}
```

The example is a shape, not a usable grant. `not_before` is also the
operator's expected-since baseline for per-grant missing-heartbeat alerts;
it is not emitted as a heartbeat. Set it to the time the operator expects the
node to start sending. Every activation also has a unique random 128-bit
`activation_id` and a unique random bearer token. Reactivation requires a
strictly later `not_before` that also exceeds every prior relay-received
heartbeat and goodbye, a new activation ID and a new token; neither ID nor
token may be reused. An activation's validity timestamps are immutable.
For rotation, stop the old relay first, choose a new baseline strictly after
its last accepted receipt (typically a near-future whole second), then install
the new registry and sender credentials and restart. Preserve the sender's
sequence state across the rotation; resetting or replacing it needs an
explicitly reviewed sequence migration. A backdated baseline fails closed
instead of allowing scrape-cache samples from the prior activation. Tenant and node are fixed
operator-provisioned label values, unique in the registry, with 1–48 ASCII
letters, numbers, hyphens, or underscores. Tokens must be unique random secrets;
the sender accepts 32–256 ASCII alphanumeric characters. The timestamps are
integer Unix seconds and the validity interval is `[not_before, expires_at)`.
Expired, not-yet-valid, revoked, unknown and invalid grants cannot ingest.
The relay holds an exclusive state lock and takes one immutable registry
snapshot at startup and validates every
ingest against it, including retries. Revocation or rotation requires a relay
restart with a new `LoadCredential` snapshot. An empty registry is valid but
makes ingestion not ready. An invalid or unreadable registry fails closed.
Durable history forbids activation ID or token reuse and limits the registry
to 128 historical node identities and 64 activations per identity. Exhaustion
requires an explicitly reviewed state migration. Provisioning and revocation
are operator responsibilities.

`relay.py GRANT_REGISTRY STATE_DB PORT` exposes `/health`, `/ready`, `/metrics`,
and authenticated `POST /v1/records`. It retains the node's receipt sequence,
last payload fingerprint, activation identity, liveness values, and counters
in durable SQLite. Every queued record carries its original operator-provisioned
activation ID; the relay checks it against the authenticated snapshot before
accepting. Old or unbound queued records are abandoned and counted, never
relabeled. Metrics for prior activations are omitted even if retained in state.
The latest identical retry is acknowledged without changing counters or
liveness; older sequences and records outside the 120-second window are
rejected. All heartbeat and goodbye values are relay receipt times, never the
sender's creation time. The persisted relay clock high-water mark makes a
backwards clock fail closed with 503 until it catches up. At most 128 distinct
node identities may ever enter one relay state database; retire or migrate
state only with a documented retention and replay policy.

The production topology runs locald directly as its systemd unit's MainPID,
independently of telemetry. Run a separate service as the same OS user:

`sender.py http://127.0.0.1:PORT TOKEN_FILE STATE_DIR --grant-activation ID --observe-unit backend-owner.service --systemctl /absolute/path/to/systemctl --`

The observer reads systemd manager state and never starts, signals, or owns
the node. It stores the boot ID, manager start timestamp and MainPID as a
bounded generation. A heartbeat requires a recent manager observation that
the unit is active or reloading with the same main process. A goodbye requires a matching
generation and manager-confirmed successful process exit. If a generation's
terminal state cannot be proved, the observer omits goodbye rather than
inferring a crash or a clean stop. Brief processes or transitions missed while
the observer is unavailable may have no reported goodbye. A stopped or failed
observer, missing credential, inaccessible registry, or spool failure does not
block or terminate the independently managed locald. A manager outage causes
pending heartbeats to be abandoned and counted rather than asserted as current
liveness. Queued observer lifecycle records carry their systemd generation
locally; before delivery, the sender rechecks that generation and the relevant
active or clean-terminal state. Superseded records are abandoned and counted.
An observer-to-manager sample can still race an immediately subsequent owner
transition; this is bounded observation, not atomic systemd/relay coordination. The exact systemd lifecycle and observer-isolation behavior must be
proved in the NixOS VM before deployment.

The existing command-supervisor mode, `sender.py URL TOKEN_FILE STATE_DIR --grant-activation ID --
COMMAND [ARGS...]`, is retained for local process checks only. Under systemd
`KillMode=control-group`, it requires `--signal-mode cgroup` to avoid forwarding
the manager's signal twice; it is not the production owner topology because a
supervisor failure can terminate its child.

The optional `--interval-seconds` (1–60, default 60) and `--spool-capacity`
(1–256, default 256) occur before `--`; they make bounded overflow observable
in a short VM proof. A durable SQLite FIFO retains pending records and the
number actually abandoned by the sender. It reserves one extra bounded slot
for an orderly goodbye; replacing an older pending goodbye counts that older
record as abandoned. Queue overflow, expiry, or abandoning queued heartbeats
when current liveness is not proved increments that count; a subsequent
diagnostic reports it. The relay durably counts accepted diagnostics and their
reported dropped-record totals. An ambiguous acknowledgment may mean that a
record abandoned by the sender was already received; the count describes the
sender's abandonment, not a deduplicated end-to-end loss count.

The registry and token must be regular service-owned files with no group or
other access; state directories must be service-owned and private. Use systemd
credentials or equivalent private files, never environment variables or
command-line secret values. The two state directories must be persistent and
exclusive to their relay or node. Do not copy or reset sender sequence state
without a matching identity/replay migration. Neither data path logs tokens.

When the node is stopped, an independent systemd timer must invoke
`sender.py URL TOKEN_FILE STATE_DIR --grant-activation ID --drain-only true --observe-unit backend-owner.service --systemctl /absolute/path/to/systemctl --` to retry pending
records. Drain mode does not start a child or generate heartbeats or goodbyes;
it uses the same exclusive state lock as the live sender, rechecks the manager
before delivering a queued observer goodbye, and exits 75 if the
lock is busy or durable work remains after its five-second attempt. Treat 75
as a retryable timer outcome. A goodbye that cannot reach the relay inside the
freshness window is abandoned and counted; replaying it as current could mask
a real later failure.

Run locald and the observer in separate systemd units and control groups;
locald must have no `Requires=` or fatal credential dependency on telemetry.
Backups or rollback of relay or sender state can reopen replay windows and need
a separate recovery policy. The relay is single-process, local-only, and must
not be exposed to a hostile network. No production credential or grant is
included.

The listener bounds its accepted worker pool to 16, queue to 32, headers to
32 lines of at most 8192 bytes, record body to 1024 bytes, and each request to
an absolute three-second network deadline. Connections beyond the worker pool
are closed before HTTP admission. Active grants have independent token buckets;
invalid credentials have their own bucket and cannot spend a grant's quota.
Bounded connection shedding and a shared local disk still impose finite
capacity under deliberate saturation; these are not a public-network endpoint.
