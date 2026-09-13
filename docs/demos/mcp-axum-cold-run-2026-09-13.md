# Axum MCP cold-run demonstration

> Historical first pass: this measured the initial Axum composition that
> submitted indexing during startup. The direct-command demonstration in
> `mcp-axum-command-demo-2026-09-13.md` supersedes that behavior and performs
> indexing through `backend.index` over MCP.

- Generated: `2026-09-13T14:47:43.692827+00:00`
- Cold service ready: **1197.917 ms**
- First indexed rows observed: **212.852 ms** after HTTP readiness (1 status polls)
- Advertised tools called: **14/14**
- Search result: **2 rows**, coordinate `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-axum-cold-xolu5h5v/project::src/lib.rs:4::DemoWidget`
- Trustfall result: **7 rows**, terminal `complete`
- Capability snapshot: **28 advertised; 9 structural frontends ready; 19 oracle/embedding capabilities unavailable** because the cold run supplied no manifests
- Readiness at first rows: `indexing` — exact coverage was complete while semantic coverage remained `0/1`

| Surface | HTTP | Latency (ms) | Outcome |
|---|---:|---:|---|
| `auth.rejection` | 401 | 21.528 | pass (expected rejection) |
| `initialize` | 200 | 0.473 | pass |
| `notifications/initialized` | 202 | 0.297 | pass |
| `ping` | 200 | 0.338 | pass |
| `tools/list` | 200 | 0.831 | pass |
| `backend.status` | 200 | 212.731 | pass |
| `backend.projects` | 200 | 4.551 | pass |
| `backend.index` | 200 | 8.253 | pass |
| `backend.search` | 200 | 46.619 | pass |
| `backend.names` | 200 | 13.287 | pass |
| `backend.document` | 200 | 10.893 | pass |
| `backend.source` | 200 | 12.948 | pass |
| `backend.outline` | 200 | 13.364 | pass |
| `backend.graph` | 200 | 13.431 | pass |
| `backend.related` | 200 | 13.378 | pass |
| `backend.diff` | 200 | 6.558 | pass (typed unavailable: no complete semantic publication) |
| `backend.query` | 200 | 21.226 | pass |
| `backend.surface` | 200 | 5.646 | pass |
| `resources/list` | 200 | 7.400 | pass |
| `resources/templates/list` | 200 | 0.311 | pass |
| `resources/read` | 200 | 219.634 | pass |
| `prompts/list` | 200 | 0.437 | pass |
| `prompts/get` | 200 | 4.084 | pass |
| `backend.remove` | 200 | 184.782 | pass |
| `session.delete` | 204 | 0.480 | pass |
| `session.after_delete` | 404 | 0.380 | pass (expected rejection) |

The run used a brand-new durable state directory and Unix socket. Compilation was completed before timing began. The bearer token is redacted; complete typed JSON-RPC responses, including ephemeral cold-run coordinates, are retained in the adjacent JSON artifact.
