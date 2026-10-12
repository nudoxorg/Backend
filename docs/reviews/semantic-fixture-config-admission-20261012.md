# Semantic protocol fixture config admission

The actual 18ed local-service cohort compiled, then selected 31 laws across
local recipe, semantic query, Unix transport, and shared-memory ownership.
Twenty-four failed with `InvalidConfig`; seven passed. The tested source was
`18ed5c7de1db5a84f5db88d8e8b481cd202b398f`, tree
`d06764fac95b37dbc9acc8f1609aa88cfb3d7ab8`. This repair starts from
`fd856a290d67553d3d590f59ddb2565b5b2ebf95`; the installer and both fixture files
are unchanged between those two revisions before this repair.

Both real protocol fixture constructors choose a config below
`<private-temp>/compiler/embedding.config`, but do not create `compiler`.
`install_embedding_runtime_inner` first calls `validate_config_path`, which
rejects an absent parent as `InvalidConfig`. Its later private-directory
admission is never reached. The rejection precedes interpreter/dependency
inspection, runtime activation, and inference. This is a shared fixture setup
failure, and does not establish that the downstream runtime laws pass.

The two constructors now call the existing
`backend_platform::durable::ensure_private_directory` for that compiler parent
before passing the config to the installer. That helper creates only the final
component beneath an existing private parent, admits ownership and permissions,
refuses links/unsafe directories, and flushes the admitted directory. Production
config validation, activation, dependency identity, resource limits, and error
projection are unchanged. The real interpreter and exact protocol script remain
the execution closure; these deterministic protocol fixtures do not claim
genuine model inference.

No test assertions, scripts, envelopes, root/selection identities, cancellation
controls, or expected outcomes changed. The existing controls continue to require
actual activation, identical admitted `Arc`, no second probe, exact descriptor
and interpreter/dependency identity, complete canonical rows, first-completion
ownership, selection/window retirement, bounded cancellation/deadlines,
whole-projection refusal, and shared-credit release. All 31 named laws remain.

Actual raw output is retained under
`/private/tmp/sol61-ilo-real-cli-mcp-20261011/core18ed-terminal-minimal-03/current18ed-focused-controls-01`.
The receipt SHA-256 is
`7ca568cc269cab2ba9467d93ecdb23e92f9ae6fc7fe1c92a0e321a8ef789a00e`.
The packet separately pins the four raw logs and the custody manifest. The
actual 18ed failure remains evidence; this source repair does not relabel it.

Source verification: Rust formatting with edition 2024 and `git diff --check`.
Compile, native tests, runtime activation, and inference after this repair are
**UNRUN**. Sole-broker native gates should select these existing groups against
the composed exact source, with nonzero counts:

| Local-service selector | Expected selected laws | Actual 18ed result |
| --- | ---: | --- |
| `builtin::query::local_recipe::tests::` | 4 | 3 pass, 1 fail |
| `builtin::query::local_semantic::tests::` | 22 | 4 pass, 18 fail |
| `builtin::commands::adapter::tests::local_semantic_transport_tests::` | 1 | 0 pass, 1 fail |
| `builtin::commands::adapter::tests::local_semantic_memory_tests::` | 4 | 0 pass, 4 fail |

The original publication-worker 6000-row/30-second failure is separate and is
not repaired or relaxed by this atom.
