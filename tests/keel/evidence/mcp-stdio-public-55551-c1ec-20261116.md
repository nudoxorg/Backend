# MCP stdio process laws

`mcp-stdio-public-55551-c1ec-20261116.json` records seven cases and 12 passing
process steps against the matching official T1 applications from Backend
`55551f420a870ddf55e93ed0117bbabc815ec612`. Each process ran in a required
cgroup-v2 boundary. The driver came from the published Keel Nix package at
`c1ec0936f7473319992d5227c6b23d7cc836a45d`; its direct executable SHA-256 is
`3ea59a7c4dcd7638f88f5ff93a622e9c65f487f3bf1eceba2071137eacd0ba1d`.
Report SHA-256 is
`94235bdc2b8159b25492795fd450f2c8009fdd1d96a0705d75dc3fa1fe138f8d`.

The independently authored streams exercise initialization ordering, supported
and unsupported version offers, retry after invalid initialization, discovery
and malformed routes, CRLF and final unterminated input, silent notifications,
numeric and string request-ID correlation, and nesting refusal followed by
recovery. One separate notification-only process must emit no replies at all;
the entire parsed reply map must be empty. An unsolicited initialized
notification cannot authorize a resources/read request before initialization.
These exact observations replace only
`initialized_notification_cannot_bypass_initialize`.

`mcp-stdio-oracle-failure-55551-6764-20261116.json` preserves the earlier
failed draft. Six cases reached a ping assertion that incorrectly required
the result object to be empty. Actual replies correctly included the canonical
wire-budget metadata. The corrected expectations require successful correlated
replies, no error and the public wire-budget fields. They keep the original
protocol expectations; they do not accept arbitrary success or erase framing.

The remaining legacy tests retain obligations not yet witnessed here: owner
call counts, real owner-backed search after cancellation, cancellation races,
oversized frames and detailed handshake instructions. Discovery here runs
without a provisioned owner; it does not independently count IPC attempts.
The exact manifests and raw replies remain on ilo-prod under
`/root/nudox-keel-mcp-static-laws-6764-555-v2-20261116` and
`/root/nudox-keel-mcp-stdio-c1ec-555-public-v5-20261116`. This proves these
identified artifacts and contracts, not later source or the full release.
