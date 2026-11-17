# Dogfood relay prototype

This is an isolated prototype, not a deployed service or an implementation of
the unrecovered F7 wire specification. It provides telemetry authorization for
explicitly provisioned identities; it does not determine or assert paid
license entitlement. Production use requires a documented authoritative
provisioning and revocation policy, review, and the VM/metrics proof.

The relay binds only to 127.0.0.1. Its private JSON identity file is a bounded
array of `{ "tenant", "node", "token_sha256" }`, mapping a random, unique
node bearer token to fixed metric labels. Provisioning and revocation are an
operator's responsibility; changing the file requires a relay restart. The
sender's token file contains the corresponding secret. Both identity and
token files and each service's state directory must be owned by the service
user and inaccessible to other users. No identity or token is supplied by the
request body. Keep the state directories persistent and exclusive to their
respective relay or node. Never copy them between identities.

`relay.py IDENTITIES_JSON STATE_DB PORT` exposes `/health`, `/ready`,
`/metrics`, and authenticated `POST /v1/records`. The sender is a process
supervisor: `sender.py http://127.0.0.1:PORT TOKEN_FILE STATE_DIR -- COMMAND
[ARGS...]`. It emits heartbeats while its child is running and a goodbye only
if the child exits successfully. It forwards SIGINT and SIGTERM. This is not
integrated with the locald entrypoint; a forcibly killed supervisor may leave
its child running, so supervision and process-custody behavior need further
work before deployment.

Records have a monotonically increasing per-identity sequence, creation time,
and kind. A diagnostic also carries a positive count of records the sender
actually failed to retain or abandoned after exceeding the 110-second replay
window. The sender persists a bounded FIFO and loss accounting in SQLite. The
relay commits its receipt and metric state in SQLite before acknowledgment;
the latest identical retry is acknowledged without changing counters or
liveness. Old sequence numbers and records outside the 120-second window are
rejected. This preserves ambiguous-ack retries without making stale heartbeat
replay look live. The relay timestamps heartbeat and goodbye values at receipt;
event creation time never supplies the metric value. Counter values include
actual accepted records and rejections and the aggregate reported loss count.
Neither data path logs the bearer token.

The prototype has not been hardened for hostile network exposure, a multi-node
or highly available relay, backup/restore of identities, clock anomalies,
process custody after supervisor failure, or durable configuration rotation.
It must not be exposed outside loopback. Operator-issued identity is not itself
evidence that a node has a paid license. No production credential or real
licensed-node identity is included.
