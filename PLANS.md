Plan index
==========

Design documents at the repo root. Do not delete or edit the plan files
themselves; this index only. Banners on superseded/stale files note their
status inline.

---

| File | Title | Scope | Status |
| ---- | ----- | ----- | ------ |
| [STREAMLINE-PLAN.md](STREAMLINE-PLAN.md) | Streamline — delete dead subsystems, unify duplicates, type-driven robustness | Audit-driven: ~3,000 LOC deletion, search/sandbox/CAS/version-resolution consolidation, capability-token authz, parse-don't-validate row decoding | **ACTIVE** |
| [RUST-ANALYZER-PLAN.md](RUST-ANALYZER-PLAN.md) | Replace rustdoc pipeline with in-process rust-analyzer producer | Drop `cargo rustdoc --output-format json`; load workspaces via `ra_ap_*` 0.0.341; lower HIR directly to `ir::Index`; P0 complete, P1 (RA as default producer) landed | **ACTIVE** |
| [DAEMON-PLAN.md](DAEMON-PLAN.md) | Sealed compute — compiler as a horizontally scalable daemon | `SealedInput → Cage → CAS → Postgres queue`; 8 phases; ForgeRuntime assembled (§2.5 done); blob GC and some ops phases pending | **ACTIVE** |
| [NIX-PLAN.md](NIX-PLAN.md) | Nix as a sixth producer language | FlakeHub acquisition → snix in-process evaluation → IR; RFC 145 doc comments; typed signatures; rnix static layer | **ACTIVE** |
| [RENDER-PLAN.md](RENDER-PLAN.md) | IR → source renderer for all five languages | Wadler-Lindig doc algebra + `Backend` trait + five emit backends; foundation (doc.rs, backend.rs, all five emit files) landed; compile-check test matrix is the open phase | **PARTIALLY LANDED** |
| [GRAPH-ARCHITECTURE.md](GRAPH-ARCHITECTURE.md) | IR → TerminusDB graph model redesign | Graph-native node/edge model; `compiler/graph/` (model.rs, from_ir.rs, link.rs) implemented; supersedes the old flat-index mapping | **LANDED** |
| [CARGO-BUCK-PLAN.md](CARGO-BUCK-PLAN.md) | Cargo-like tooling for Buck2 | `buck2 run //:add\|update\|check\|new` via `build/third-party/tools/crates.py`; core verbs (add/update/check) implemented; reconciler and `new` may be partial | **PARTIALLY LANDED** |
| [FORGE-PHASE4-PLAN.md](FORGE-PHASE4-PLAN.md) | ForgeRuntime: replace compile-plane process globals | Kill 11 `OnceLock` globals; inject owned `ForgeRuntime` into compile path; `ForgeRuntime<Ready>` assembled in `server/forge.rs` and wired in `lib.rs`; `BACKEND` `OnceLock` in `util/sandbox` remains | **IN PROGRESS** |
| [IMPROVEMENTS_PLAN_RICH_METADATA_CACHING_SEARCH_KITCHENSINK.md](workspace/IMPROVEMENTS_PLAN_RICH_METADATA_CACHING_SEARCH_KITCHENSINK.md) | Rich metadata, stampede-resistant caching, search heuristics, facade | Port lib.rs/crates.rs strengths to workspace; caching crate + stampede/jitter primitives landed; symbol tokenizer + Phase 2 extractor + Phase 3 ranking fusion landed; Phases 4/5 (foyer L2, full facade) deferred | **PARTIALLY LANDED** |
| [HANDOFF.md](HANDOFF.md) | Project state handoff (2026-07-09) | Point-in-time snapshot of branch topology, worktree state, uncommitted work, and next steps | **REFERENCE** |
| [towards-a-real-time-agent-system.md](towards-a-real-time-agent-system.md) | Hivemind — real-time multi-agent client vision | Product/UX vision for a tree-structured, always-live agent system built on this backend; no implementation plan or timeline | **VISION** |

---

No file named `TYPES-ARCHITECTURE.md` or a standalone rustdoc-in-process plan
exists at the repo root. Those were working names; the rustdoc-in-process work
was subsumed into `RUST-ANALYZER-PLAN.md` before a separate file was created.
