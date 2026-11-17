# Backend HTTP metrics

Start an existing `backend-locald` owner first, then run:

```sh
backend-metrics --endpoint /path/to/locald.sock --bind 127.0.0.1:9464
```

The socket must be accessible to the service's operating-system identity; the
service never starts an owner. Bind addresses must be loopback addresses. The
bind option defaults to `127.0.0.1:9464`.

`GET /metrics` exposes the HTTP requests handled by this service, including
scrapes and health probes. It does not count CLI, MCP, or other locald traffic.
The response being generated appears in the *next* scrape. All status labels
are fixed and start at zero; histogram buckets are cumulative seconds. The
scraper supplies the `job` label. Other owner, store, relay, and node metrics
are not inferred from the HTTP service.

`GET /health` performs a fresh, authenticated, deadline-bounded health RPC to
the specified owner. HTTP 200 means the owner returned an admitted health
report; 503 means it did not. This is an owner responsiveness check, not a
claim that every capability or indexed project is ready. A successful metrics
scrape alone establishes only HTTP surface availability.

The server accepts at most 32 connections, one request per connection, with an
8 KiB header limit, no request bodies, and a two-second connection deadline.
Responses are fixed or bounded by the fixed metrics series. HTTP parser refusals and connections that never produce an admitted request are not counted as handled requests. SIGINT and SIGTERM
stop acceptance and drain bounded in-flight requests.

For the real-process integration test, build both executables and run:

```sh
cargo build -p backend-locald -p backend-metrics
cargo test -p backend-metrics --test process
```

The test finds `backend-locald` beside the test profile's binaries; set
`BACKEND_LOCALD_BIN` to override that path. It creates a private temporary
workspace, runs a real owner and HTTP service, and cleans up both processes.

When a fresh authenticated snapshot is returned by an initialized owner,
`store_requests_total` and `store_request_duration_seconds` describe that
owner's view journal recovery, append and replacement calls. They distinguish
success and error and do not imply observations of other storage dependencies.
Each `/metrics` request obtains a new bounded snapshot; it does not cache the
previous one. `backend_dependency_snapshot_available` is 1 when that scrape
obtained an initialized snapshot and 0 otherwise. When it is 0, all store
series are omitted rather than reported as zero. Owner health and HTTP surface
availability remain independent checks.
