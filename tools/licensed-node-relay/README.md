# Dogfood telemetry relay v1

This is a new internal dogfood protocol. It is not a claim of compatibility
with the unrecovered F7 wire specification or the paid-billing system. A grant
is an operator-issued authorization to send telemetry for one tenant and node;
it does not attest a paid billing entitlement. Deployment requires review and
the NixOS VM, VictoriaMetrics, alert, and systemd custody proofs.

The relay binds only to 127.0.0.1. Its private JSON grant registry is an array
of at most 128 objects with exactly these fields:

```json
{"tenant":"nudoxorg","node":"dogfood-node","token_sha256":"<lowercase SHA-256 of a random bearer token>","scope":"telemetry:write","not_before":0,"expires_at":1800000000,"revoked":false}
```

The example is a shape, not a usable grant. `not_before` is also the
operator's expected-since baseline for per-grant missing-heartbeat alerts;
it is not emitted as a heartbeat. Set it to the time the operator expects the
node to start sending. Tenant and node are fixed
operator-provisioned label values, unique in the registry, with 1–48 ASCII
letters, numbers, hyphens, or underscores. Tokens must be unique random secrets;
the sender accepts 32–256 ASCII alphanumeric characters. The timestamps are
integer Unix seconds and the validity interval is `[not_before, expires_at)`.
Expired, not-yet-valid, revoked, unknown and invalid grants cannot ingest.
The registry is read and validated on each ingest, including retries. An
empty registry is valid but makes ingestion not ready. An invalid or unreadable
registry fails closed. Update a mutable registry by atomic replacement. A
systemd `LoadCredential` snapshot requires a relay restart to apply revocation
or rotation. Provisioning and revocation are operator responsibilities.

`relay.py GRANT_REGISTRY STATE_DB PORT` exposes `/health`, `/ready`, `/metrics`,
and authenticated `POST /v1/records`. It retains the node's receipt sequence,
last payload fingerprint, liveness values, and counters in durable SQLite.
The latest identical retry is acknowledged without changing counters or
liveness; older sequences and records outside the 120-second window are
rejected. All heartbeat and goodbye values are relay receipt times, never the
sender's creation time. The persisted relay clock high-water mark makes a
backwards clock fail closed with 503 until it catches up. At most 128 distinct
node identities may ever enter one relay state database; retire or migrate
state only with a documented retention and replay policy.

`sender.py http://127.0.0.1:PORT TOKEN_FILE STATE_DIR -- COMMAND [ARGS...]`
supervises the actual node process, emitting a heartbeat while it is alive and
a goodbye only for successful exit. It forwards SIGINT and SIGTERM. Under systemd `KillMode=control-group`, pass `--signal-mode cgroup` before
`--`: systemd delivers the stop signal to both sender and child, so the sender
must not deliver it a second time. In this mode stop the service through
systemd, which signals the entire cgroup; signaling the sender PID alone does
not stop the child. The default `--signal-mode forward` is for callers that
signal only the sender and need it to forward termination to the child. The
optional `--interval-seconds` (1–60, default 60) and `--spool-capacity`
(1–256, default 256) occur before `--`; they make bounded overflow observable
in a short VM proof. A durable SQLite FIFO retains pending records and the
number actually abandoned by the sender. It reserves one extra bounded slot
for an orderly goodbye; replacing an older pending goodbye counts that older
record as abandoned. Queue overflow, expiry, or abandoning
queued heartbeats when the child exits increments that count; a subsequent
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
`sender.py URL TOKEN_FILE STATE_DIR --drain-only true --` to retry pending
records. Drain mode does not start a child or generate heartbeats or goodbyes;
it uses the same exclusive state lock as the live sender and exits 75 if the
lock is busy or durable work remains after its five-second attempt. Treat 75
as a retryable timer outcome. A goodbye that cannot reach the relay inside the
freshness window is abandoned and counted; replaying it as current could mask
a real later failure.

The service must use a systemd control group with `KillMode=control-group` and
stop remaining processes when the supervisor exits, so killing the supervisor
also tears down its backend child. This required custody has not yet been
proved in a NixOS VM. Running the wrapper outside a service manager with that
property can leave its child alive after the wrapper is forcibly killed.
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
