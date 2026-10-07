# Private workspace construction under ordinary umasks

Root read all nineteen changed files, including the complete constructor and
fixture changes. New workspace descendants are created relative to pinned
directory handles with mode 0700; anonymous OSV spool files use their newly
owned file descriptor to set mode 0600. Constructors validate ownership,
permissions and path identity before path-based consumers run. Existing unsafe,
foreign-owned or symlinked children are refused without changing their modes.

Root verified all 33 raw-file hashes in the corrected v2 archive, read its launch
records and checked actual test output. Exact final candidate
`dab7d3494f2b0310c005dfddc6364f2123cd1361` passed the acquisition (91), registry
(127), service registry (43) and discovery (69 passed, 8 ignored) filters.
Earlier source-equivalent package gates passed platform 62, store 275, Tantivy
96 and forge 28 with one ignored network smoke test. Child-process invocations
are not additional distinct tests; these filtered scopes are not summed.

The worker's first archive had a filename collision: its final service-registry
path contained the earlier 42/43 failure. Root caught this while independently
checking the actual log, before integration. That archive remains preserved
outside Git as a failed packaging artifact. The corrected v2 archive separates
the exact final 43/43 pass from the superseded failure. No tests were rerun to
repair that packaging mistake. Its SHA256 is
`1d554dd4c07315d98ad05b5b46ab02333e4f03dbd2d446fc73221b390c961689`.

Eighteen integrated file blobs match the tested candidate exactly. The platform
directory file additionally contains the separately reviewed and merged
exclusive-file/startup initializer repair from PR #43. Its combined current
native gate remains required. The b90 broad package logs lack per-run launch
stamps; their source-equivalent scope is stated in WORKER-SCOPE.md rather than
being represented as exact-current-source acceptance.

This checkpoint verifies fresh private-root creation. It does not repair
pre-existing group-writable caches or prove Windows behavior, a current
matched CLI/MCP/daemon run, or production readiness. Read `root-audit.json` and
the raw archive for the exact source, package, test and retirement scopes.
