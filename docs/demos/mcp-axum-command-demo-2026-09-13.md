# Direct MCP commands against Axum

This is a command transcript from a brand-new durable workspace. Axum started without indexing; `backend.index` was the only operation that submitted the project.

- Axum/locald ready: **773.499 ms**
- Before `backend.index`: readiness `ready`, rows `0`
- `backend.index`: `{"accepted": true, "completion": "pending", "intent": "a465a4914f91114a6bff61262011428fea4d53836ad4ade98bbc419d81155198", "message": "Index request accepted. Check backend.status …`
- Indexed rows visible after submit: **328.309 ms**
- After indexing: readiness `indexing`, rows `37`
- `backend.search("Beacon")`: **31 rows**
- Languages with direct search hits: **8/8**
- Trustfall: **36 rows**, terminal `complete`

## Search results

| Language | Name | Kind | Coordinate | Signature |
|---|---|---|---|---|
| Rust | `rust_beacon_entry` | `function` | `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/lib.rs:11::rust_beacon_entry` | `pub fn rust_beacon_entry() -> u64` |
| Go | `GoBeaconEntry` | `function` | `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.go:6::GoBeaconEntry` | `func GoBeaconEntry() uint64` |
| Python | `illuminate` | `function` | `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.py:5::illuminate` | `def illuminate(self) -> int:` |
| TypeScript | `constructor` | `method` | `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.ts:3::constructor` | `constructor(readonly intensity: number)` |
| Java | `javaBeaconEntry` | `method` | `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/JavaBeacon.java:6::javaBeaconEntry` | `public static long javaBeaconEntry()` |
| C# | `CSharpBeacon` | `class` | `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/CSharpBeacon.cs:2::CSharpBeacon` | `public sealed class CSharpBeacon` |
| C | `c_beacon_illuminate` | `function` | `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/c_beacon.c:3::c_beacon_illuminate` | `unsigned long c_beacon_illuminate(CBeacon beacon)` |
| C++ | `CppBeacon` | `class` | `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/cpp_beacon.cpp:2::CppBeacon` | `class CppBeacon` |

## Composed reads

Each row below passed an exact coordinate returned by `backend.search` directly into `backend.document` and `backend.source`.

| Language | Document signature | Source state | Source excerpt |
|---|---|---|---|
| Rust | `pub fn rust_beacon_entry() -> u64` | `captured` | `pub fn rust_beacon_entry() -> u64 { RustBeacon::new(8).illuminate() }` |
| Go | `func GoBeaconEntry() uint64` | `captured` | `func GoBeaconEntry() uint64 { return GoBeacon{Intensity: 7}.Illuminate() }` |
| Python | `def illuminate(self) -> int:` | `captured` | `def illuminate(self) -> int:         return self.intensity` |
| TypeScript | `constructor(readonly intensity: number)` | `captured` | `constructor(readonly intensity: number) {}` |
| Java | `public static long javaBeaconEntry()` | `captured` | `public static long javaBeaconEntry() { return new JavaBeacon(4).illuminate(); }` |
| C# | `public sealed class CSharpBeacon` | `captured` | `public sealed class CSharpBeacon {     public ulong Intensity { get; }     public CSharpBeacon(ulong` |
| C | `unsigned long c_beacon_illuminate(CBeacon beacon)` | `captured` | `c_beacon_illuminate(CBeacon beacon)` |
| C++ | `class CppBeacon` | `captured` | `class CppBeacon { public:     explicit CppBeacon(unsigned long intensity) : intensity_(intensity) {}` |

## Other MCP results

- `backend.names("Beacon")`: 24 rows.
- `backend.graph`: 2 rows for `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/lib.rs:11::rust_beacon_entry`.
- `backend.related`: 2 rows for the same coordinate.
- `backend.outline`: extent `complete`, 8 roots.
- `backend.surface(subscriptions)`: `{"surface": {"data": [], "result": "subscriptions"}}`
- `resources/list`: 2 resources; `resources/read` returned the pinned workspace status.
- `prompts/list`: 1 prompt; `prompts/get` rendered `backend.explore`.
- `backend.diff`: `isError=true` — `command failed: invalid query: older package has no complete semantic publication`. A cold single version has no complete semantic publication pair to compare.

## JSON-RPC exchanges

### `initialize`

Request: `{"id": 1, "jsonrpc": "2.0", "method": "initialize", "params": {"capabilities": {}, "clientInfo": {"name": "direct-axum-demo", "version": "1"}, "protocolVersion": "2025-11-25"}}`

HTTP 200 in 27.422 ms. Full response is in the JSON artifact.

### `notifications/initialized`

Request: `{"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}`

HTTP 202 in 0.677 ms. Full response is in the JSON artifact.

### `tools/list`

Request: `{"id": 2, "jsonrpc": "2.0", "method": "tools/list", "params": {}}`

HTTP 200 in 1.507 ms. Full response is in the JSON artifact.

### `backend.status.before_index`

Request: `{"id": 3, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {}, "name": "backend.status"}}`

HTTP 200 in 360.313 ms. Full response is in the JSON artifact.

### `backend.index.submit`

Request: `{"id": 4, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {}, "name": "backend.index"}}`

HTTP 200 in 645.745 ms. Full response is in the JSON artifact.

### `backend.projects`

Request: `{"id": 6, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {}, "name": "backend.projects"}}`

HTTP 200 in 4.581 ms. Full response is in the JSON artifact.

### `backend.search`

Request: `{"id": 7, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"limit": 100, "query": "Beacon"}, "name": "backend.search"}}`

HTTP 200 in 90.532 ms. Full response is in the JSON artifact.

### `backend.names`

Request: `{"id": 8, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"limit": 100, "query": "Beacon"}, "name": "backend.names"}}`

HTTP 200 in 32.295 ms. Full response is in the JSON artifact.

### `backend.document[.c]`

Request: `{"id": 9, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/c_beacon.c:3::c_beacon_illuminate"}, "name": "backend.document"}}`

HTTP 200 in 13.883 ms. Full response is in the JSON artifact.

### `backend.source[.c]`

Request: `{"id": 10, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/c_beacon.c:3::c_beacon_illuminate"}, "name": "backend.source"}}`

HTTP 200 in 12.784 ms. Full response is in the JSON artifact.

### `backend.document[.java]`

Request: `{"id": 11, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/JavaBeacon.java:6::javaBeaconEntry"}, "name": "backend.document"}}`

HTTP 200 in 12.329 ms. Full response is in the JSON artifact.

### `backend.source[.java]`

Request: `{"id": 12, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/JavaBeacon.java:6::javaBeaconEntry"}, "name": "backend.source"}}`

HTTP 200 in 12.658 ms. Full response is in the JSON artifact.

### `backend.document[.go]`

Request: `{"id": 13, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.go:6::GoBeaconEntry"}, "name": "backend.document"}}`

HTTP 200 in 13.572 ms. Full response is in the JSON artifact.

### `backend.source[.go]`

Request: `{"id": 14, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.go:6::GoBeaconEntry"}, "name": "backend.source"}}`

HTTP 200 in 12.369 ms. Full response is in the JSON artifact.

### `backend.document[.py]`

Request: `{"id": 15, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.py:5::illuminate"}, "name": "backend.document"}}`

HTTP 200 in 13.045 ms. Full response is in the JSON artifact.

### `backend.source[.py]`

Request: `{"id": 16, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.py:5::illuminate"}, "name": "backend.source"}}`

HTTP 200 in 13.569 ms. Full response is in the JSON artifact.

### `backend.document[.cpp]`

Request: `{"id": 17, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/cpp_beacon.cpp:2::CppBeacon"}, "name": "backend.document"}}`

HTTP 200 in 12.441 ms. Full response is in the JSON artifact.

### `backend.source[.cpp]`

Request: `{"id": 18, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/cpp_beacon.cpp:2::CppBeacon"}, "name": "backend.source"}}`

HTTP 200 in 13.473 ms. Full response is in the JSON artifact.

### `backend.document[.ts]`

Request: `{"id": 19, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.ts:3::constructor"}, "name": "backend.document"}}`

HTTP 200 in 12.846 ms. Full response is in the JSON artifact.

### `backend.source[.ts]`

Request: `{"id": 20, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/beacon.ts:3::constructor"}, "name": "backend.source"}}`

HTTP 200 in 13.037 ms. Full response is in the JSON artifact.

### `backend.document[.rs]`

Request: `{"id": 21, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/lib.rs:11::rust_beacon_entry"}, "name": "backend.document"}}`

HTTP 200 in 12.960 ms. Full response is in the JSON artifact.

### `backend.source[.rs]`

Request: `{"id": 22, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/lib.rs:11::rust_beacon_entry"}, "name": "backend.source"}}`

HTTP 200 in 13.156 ms. Full response is in the JSON artifact.

### `backend.document[.cs]`

Request: `{"id": 23, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/CSharpBeacon.cs:2::CSharpBeacon"}, "name": "backend.document"}}`

HTTP 200 in 12.669 ms. Full response is in the JSON artifact.

### `backend.source[.cs]`

Request: `{"id": 24, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/CSharpBeacon.cs:2::CSharpBeacon"}, "name": "backend.source"}}`

HTTP 200 in 12.223 ms. Full response is in the JSON artifact.

### `backend.outline`

Request: `{"id": 25, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {}, "name": "backend.outline"}}`

HTTP 200 in 15.735 ms. Full response is in the JSON artifact.

### `backend.graph`

Request: `{"id": 26, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/lib.rs:11::rust_beacon_entry"}, "name": "backend.graph"}}`

HTTP 200 in 11.552 ms. Full response is in the JSON artifact.

### `backend.related`

Request: `{"id": 27, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"coordinate": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot::src/lib.rs:11::rust_beacon_entry"}, "name": "backend.related"}}`

HTTP 200 in 14.759 ms. Full response is in the JSON artifact.

### `backend.query`

Request: `{"id": 28, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"limit": 100, "query": "{ Declaration { coordinate @output name @output kind @output signature @output } }", "variables": {}}, "name": "backend.query"}}`

HTTP 200 in 24.877 ms. Full response is in the JSON artifact.

### `backend.surface`

Request: `{"id": 29, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"command": {"operation": "subscriptions"}}, "name": "backend.surface"}}`

HTTP 200 in 4.813 ms. Full response is in the JSON artifact.

### `resources/list`

Request: `{"id": 30, "jsonrpc": "2.0", "method": "resources/list", "params": {}}`

HTTP 200 in 7.428 ms. Full response is in the JSON artifact.

### `resources/read[current]`

Request: `{"id": 31, "jsonrpc": "2.0", "method": "resources/read", "params": {"uri": "backend://workspace/current"}}`

HTTP 200 in 260.175 ms. Full response is in the JSON artifact.

### `prompts/list`

Request: `{"id": 32, "jsonrpc": "2.0", "method": "prompts/list", "params": {}}`

HTTP 200 in 0.484 ms. Full response is in the JSON artifact.

### `prompts/get[backend.explore]`

Request: `{"id": 33, "jsonrpc": "2.0", "method": "prompts/get", "params": {"arguments": {"query": "polyglot Beacon API"}, "name": "backend.explore"}}`

HTTP 200 in 3.634 ms. Full response is in the JSON artifact.

### `backend.diff`

Request: `{"id": 34, "jsonrpc": "2.0", "method": "tools/call", "params": {"arguments": {"from": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot", "to": "/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/backend-mcp-command-demo-o8_k9k9l/polyglot"}, "name": "backend.diff"}}`

HTTP 200 in 7.438 ms. Full response is in the JSON artifact.
