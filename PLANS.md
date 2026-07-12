Plan index
==========

Design documents at the repo root. Do not delete or edit the plan files
themselves; this index only. Banners on superseded/stale files note their
status inline.

---

| File | Title | Scope | Status |
| ---- | ----- | ----- | ------ |
| [STREAMLINE-PLAN.md](STREAMLINE-PLAN.md) | Streamline — delete dead subsystems, unify duplicates, type-driven robustness | Phases 0–4 landed: guardrail lints + orphan script; ~1,440 LOC pure deletion; ranked search wired (2a); admin endpoints (2b); heuristics wired (2c); enums/version/pagination/sandbox/CAS/hash unified (3a–3f); capability-witness authz (4a), parse-don't-panic (4b), secrets+config guard (4c), producer honesty (4d), Store::list_cas + blob-GC observability (4f). Deferred: 4e typestate extension (opportunistic), forge L3 activation (needs ForgeContext generic), deny-in-CI lint promotion | **MOSTLY LANDED** |
| [RUST-ANALYZER-PLAN.md](RUST-ANALYZER-PLAN.md) | Replace rustdoc pipeline with in-process rust-analyzer producer | Drop `cargo rustdoc --output-format json`; load workspaces via `ra_ap_*` 0.0.341; lower HIR directly to `ir::Index`; P0 complete, P1 (RA as default producer) landed | **ACTIVE** |
| [DAEMON-PLAN.md](DAEMON-PLAN.md) | Sealed compute — compiler as a horizontally scalable daemon | `SealedInput → Cage → CAS → Postgres queue`; 8 phases; ForgeRuntime assembled (§2.5 done); blob GC and some ops phases pending | **ACTIVE** |
| [OXC-PLAN.md](OXC-PLAN.md) | TypeScript producer: deno_doc → direct OXC | Replace deno_doc/deno_graph/swc with oxc_parser+oxc_semantic+oxc_resolver (0.139.0 / 11.23.0); facts-first two-pass extractor; symbol-accurate type_links; exceed tiers: oxc_isolated_declarations normalizer + tsgo (TS 7.0 GA) checker oracle; ~60 vendored crates removed | **PROPOSED** |
| [NIX-PLAN.md](NIX-PLAN.md) | Nix as a sixth producer language | FlakeHub acquisition → snix in-process evaluation → IR; RFC 145 doc comments; typed signatures; rnix static layer | **ACTIVE** |
| [RENDER-PLAN.md](RENDER-PLAN.md) | IR → source renderer for all five languages | Wadler-Lindig doc algebra + `Backend` trait + five emit backends; foundation (doc.rs, backend.rs, all five emit files) landed; compile-check test matrix is the open phase | **PARTIALLY LANDED** |
| [GRAPH-ARCHITECTURE.md](GRAPH-ARCHITECTURE.md) | IR → TerminusDB graph model redesign | Graph-native node/edge model; `compiler/graph/` (model.rs, from_ir.rs, link.rs) implemented; supersedes the old flat-index mapping | **LANDED** |
| [CARGO-BUCK-PLAN.md](CARGO-BUCK-PLAN.md) | Cargo-like tooling for Buck2 | `buck2 run //:add\|update\|check\|new` via `build/third-party/tools/crates.py`; core verbs (add/update/check) implemented; reconciler and `new` may be partial | **PARTIALLY LANDED** |
| [FORGE-PHASE4-PLAN.md](FORGE-PHASE4-PLAN.md) | ForgeRuntime: replace compile-plane process globals | Kill 11 `OnceLock` globals; inject owned `ForgeRuntime` into compile path; `ForgeRuntime<Ready>` assembled in `server/forge.rs` and wired in `lib.rs`. The last global — `util/sandbox`'s `global_backend()` `OnceLock` — was removed by STREAMLINE 3d (the `Backend` trait collapse to `Cage`) | **LANDED** |
| [IMPROVEMENTS_PLAN_RICH_METADATA_CACHING_SEARCH_KITCHENSINK.md](workspace/IMPROVEMENTS_PLAN_RICH_METADATA_CACHING_SEARCH_KITCHENSINK.md) | Rich metadata, stampede-resistant caching, search heuristics, facade | Port lib.rs/crates.rs strengths to workspace; caching crate + stampede/jitter primitives landed; symbol tokenizer + Phase 2 extractor + Phase 3 ranking fusion landed; Phases 4/5 (foyer L2, full facade) deferred | **PARTIALLY LANDED** |
| [OBSERVABILITY-PLAN.md](OBSERVABILITY-PLAN.md) | Wire the backend end-to-end into the NixOS telemetry stack (OTLP everywhere) | Spans Backend/ + nixos/. Today: zero OpenTelemetry in the Rust workspace; `/metrics` unscraped; Tempo/Pyroscope receive nothing; logs are plain-text journal-only. Target: a `telemetry` crate exporting traces+metrics+logs over OTLP to a local Alloy otelcol collector fanning out to Tempo/VictoriaMetrics/Loki, plus Pyroscope profiling, `OTEL_*` env in the backend unit, a backend scrape-job fallback, and backend dashboards/alerts. 8 phases; direct-OTLP logs with JSON-journal fallback | **PROPOSED** |
| [HANDOFF.md](HANDOFF.md) | Project state handoff (2026-07-09) | Point-in-time snapshot of branch topology, worktree state, uncommitted work, and next steps | **REFERENCE** |
| [towards-a-real-time-agent-system.md](towards-a-real-time-agent-system.md) | Hivemind — real-time multi-agent client vision | Product/UX vision for a tree-structured, always-live agent system built on this backend; no implementation plan or timeline | **VISION** |

---

No file named `TYPES-ARCHITECTURE.md` or a standalone rustdoc-in-process plan
exists at the repo root. Those were working names; the rustdoc-in-process work
was subsumed into `RUST-ANALYZER-PLAN.md` before a separate file was created.
