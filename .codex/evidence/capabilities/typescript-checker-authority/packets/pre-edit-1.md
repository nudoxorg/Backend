# Pre-edit review 1 (packet): phase0 contract review

Reviewer: registered `nudox_terra_reviewer` subagent (opencode task tool),
task ses_f9a956588ffeWMiACTSWAa8cqc, read-only by configuration
(.opencode/agents/reviewer.md: edit deny). Snapshot: live worktree at
d9411952f + fa7f6966d (L0 landed mid-review; reviewer verified both).
Review occurred BEFORE any L1 production edit; no builder rationale was
supplied to the reviewer.

## Verdict

NOT APPROVED — 2 blockers, 7 majors, 5 minors, 2 questions. Packet repair
required before L1.

## Findings (verbatim structure; full text retained in the review task log)

- BLOCKER 1 — frozen-baseline custody broken: L0 committed with no custody
  event; index.toml froze a hash that fa7f6966d superseded; baseline_tree
  narrated an unreachable tree. Correction: re-freeze at a named commit,
  backfill L0's worker record + card digest, then issue L1.
- BLOCKER 2 — law 1's required terminal undefined; R2 cannot kill the live
  degrade branch (lower/typescript.rs:1353-1357, doc 1342-1343). Pin exact
  variant + retained fields for three causes (env-var binary missing / node
  absent / exit-3 module missing); delete the swallow arm; negative
  zero-facts assertion; mutation test (reinstating `=> None` must red);
  NativeTool decision (default: reuse AuthorityFailure::TypeScript plumbing,
  zero new vocabulary).
- MAJOR 1 — R4 grep over/under-fires; exception list must be inline; primary
  form semantic (B2 terminals + no authority-optional branch).
- MAJOR 2 — pin `report: &'source Report` (borrowed, preserves Copy of
  CompileRequest/SemanticAuthorityInput); state non-exhaustive-match blast
  radius (compile.rs:233-245, 299-306 rustc-forced arms).
- MAJOR 3 — R5 fragment half lives in mutable inline assertions; freeze a
  decoded-facts table + chained decode→lower golden replay outside
  lower/typescript.rs; mutation-sensitivity required.
- MAJOR 4 — environment unpinned (node, typescript module, root
  package.json/node_modules unadmitted); R7/R8 receipts must be committed
  raw outputs + exact commands; authority-dependent tests must assert
  not-skipped (checker_protocol.rs:437-446 skips when node absent).
- MAJOR 5 — R1's primary gate blocked on out-of-constraint siblings;
  restate as lane-owned top-level target; pin assertion set so deletion
  fails.
- MAJOR 6 — baseline narrative stale in three places (python lane also red;
  rust.rs E0106; "8 errors" post-repair); re-anchor to named commit with
  command receipts.
- MAJOR 7 — law 5 reads production while registry legs are test-support;
  R10's package-context run has no production home (Checker::run hardcodes a
  bare temp dir). Decide: production Checker package-root run vs test
  support; R10 must demonstrate a module-qualified origin a bare temp-dir
  run cannot produce.
- m1 — R3 needs a fact-flows-to-IR discriminator (mutate one report fact,
  recompute digest, IR cell must flip).
- m2 — R9 needs baseline numbers + regression cap.
- m3 — R8 cites a rubric that does not exist; add mutation sensitivity +
  `other`-fallback accounting (main.cjs:95-101).
- m4 — checker.rs:800 `unwrap_or(u64::MAX)` sentinel in an error payload;
  carry as debt row.
- m5 — "matches sibling lanes" is only true of the go lane; fix wording.
- q1 — EmissionExtension::TypeScript lives in shared lower.rs; parent
  mandate grants the lane "lane surfaces in compiler/driver/lower.rs";
  ownership amended to name exactly that (TypeScript surface only).
- q2 — interface/protocol mirror of authority inputs: out of scope today;
  revisit if protocol goldens gain checker-authority fields.

## Terra disposition

All blockers/majors/minors accepted; repairs applied to brief.md,
proof-matrix.md, research.md, index.toml in the packet-repair commit, plus
cards/L1 (with the B2/M2/M5/M7 pins inlined). q2 recorded as open
environment note.
