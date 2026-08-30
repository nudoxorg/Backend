# Pre-edit packet — Wave C.7 (rationale-free review input)

Capability: wave-c7-application-service
Baseline: f2565a9fb33af06053bd19721d4dc2753ec09ed5
Allowed production paths: workspace2/crates/nudox-application/src/**
Allowed evidence path: workspace2/.codex/evidence/capabilities/wave-c7-application-service/**
Concurrent owner: Sol — Cargo manifest, lockfiles, top-level tests, adapters.

Required behavior: concrete typed service for generate/status/search/graph/vector/locality/health
and bounded progress; exact structured errors; stable order; explicit complete/partial/degraded/
cancelled/failed terminals; replay cursors; local usefulness while remote unavailable.

Must prove: malformed semantic input, limit+1, constant-body, outage-to-empty-success, lost terminal,
duplicate terminal, replay with two cursors, cancellation, disabled Probe laziness, fixed resource
bounds, and dependency/adapter negative space.

Out of scope: CLI, MCP, GPUI, filesystem/network/runtime SDK, serde, cache, dyn, boxed future/stream,
new graph/vector backend, or changes to existing crates.

Reviewer must inspect this packet and the source snapshot independently before any production writing.
