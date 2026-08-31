# Shared crates migration insights

## Observed

- fingerprint: legacy-workspace-exception-list
  role: terra
  capability and commit: shared-crates-migration (pending first structural commit)
  observed behavior and concrete artifact: `tools/check-crate-layout.sh` explicitly allowed three shipping crates outside `crates/` and delegated shipping topology to nine workspace manifests.
  why the current rubric/skill/tool allowed it: the layout gate was transitional and its exceptions could keep migration debt green indefinitely.
  local correction attempted and result: replaced the exceptions with a root-metadata and legacy-manifest falsifier; it is intentionally red until the git-aware moves land.
  suggested enforcement: repository gate
  occurrences: shared-crates-migration
  state: closed
  owner and closing artifact: Terra, `tools/check-crate-layout.sh`

- fingerprint: offline-git-source-cache
  role: terra
  capability and commit: shared-crates-migration, `255375bb`
  observed behavior and concrete artifact: a clean host Cargo cache makes bare `cargo metadata --manifest-path Cargo.toml --locked --offline --format-version=1` exit 101 for unavailable registry sources (first observed `bolero-afl v0.13.0`); a populated host cache can instead reveal a later GPUI git checkout miss.
  why the current rubric/skill/tool allowed it: root metadata and the layout gate prove the graph but bare Cargo deliberately relies on mutable host caches rather than the Nix vendor configuration used by the pinned quality shell.
  local correction attempted and result: the R2/R4 proof commands now run `stable_cargo` inside `nix develop .#quality`, where `flake.nix` vendors crates.io and the exact GPUI git source. Completion remains pending only because the host-wide `nix store gc` blocks shell materialization; no structural or product failure was observed.
  suggested enforcement: repository gate
  occurrences: shared-crates-migration
  state: watching
  owner and closing artifact: Terra, `flake.nix` and `tools/quality.sh`

- fingerprint: inherited-lint-policy-drift
  role: terra
  capability and commit: shared-crates-migration, post-`34fb9070`
  observed behavior and concrete artifact: root strict Clippy rejected `nudox-ir-format` for `missing_docs` and `clippy::must_use_candidate` after moved manifests newly opted into `[lints] workspace = true`.
  why the current rubric/skill/tool allowed it: path normalization was treated as equivalent to policy inheritance even where the source manifest previously owned no workspace lint configuration.
  local correction attempted and result: removed new workspace-lint inheritance from the ten formerly independent manifests and restored the two local lint tables that existed before the move; focused strict Clippy for IR and compiler packages is green.
  suggested enforcement: test
  occurrences: shared-crates-migration
  state: closed
  owner and closing artifact: Terra, moved `Cargo.toml` manifests

- fingerprint: former-plane-lint-policy-drift
  role: terra
  capability and commit: shared-crates-migration, post-`45c5bbd2`
  observed behavior and concrete artifact: a hostile strict-Clippy falsifier found that `nudox-adaptive` and the four `wave-application-*` crates had also lost the narrower lint tables inherited from their former plane workspaces; adaptive consequently produced 14 strict-Clippy errors under the broader root policy.
  why the current rubric/skill/tool allowed it: the first repair handled manifests that were formerly independent and two local tables, but did not compare every moved crate against the lint table inherited from its old nested workspace.
  local correction attempted and result: copied the exact former adaptive and application plane lint tables into the five moved leaf manifests; the root remains the sole Cargo workspace while the prior per-crate lint ownership is preserved.
  suggested enforcement: test
  occurrences: shared-crates-migration
  state: closed
  owner and closing artifact: Terra, `crates/nudox-adaptive/Cargo.toml` and `crates/wave-application-*/Cargo.toml`

## Explained

- Cargo's workspace rules make the root the only owner of the shared lock, inherited package/lint policy, and profiles; moved manifests can inherit that policy without inheriting adapter dependencies. The rubric now requires a root metadata check and one root lock.

## Corrected

- old-to-new shipping crate map: `domains/ir/crates/{nudox-ir-format,nudox-ir-vocab}` → `crates/{nudox-ir-format,nudox-ir-vocab}`; `planes/compiler/crates/{nudox-compile-registry,nudox-compile-vocab}` → `crates/{nudox-compile-registry,nudox-compile-vocab}`; `planes/index/crates/{nudox-index-core,nudox-index-graph-vector,nudox-index-vocab}` → `crates/{nudox-index-core,nudox-index-graph-vector,nudox-index-vocab}`; `planes/index/adapters/{nudox-index-qdrant,nudox-index-tantivy}` → `crates/{nudox-index-qdrant,nudox-index-tantivy}`; `planes/adaptive/crates/nudox-adaptive` → `crates/nudox-adaptive`; `adapters/{durable-journal,observability}` → `crates/{nudox-durable-journal,nudox-observability-adapter}`; `planes/application/crates/{wave-application-cli,wave-application-core,wave-application-mcp,wave-application-protocol}` and `planes/application/gpui_shell` → their matching `crates/wave-application-*` directories.
- `255375bb` makes the root lock, package policy, and normalized path graph authoritative: metadata reports 30 packages/30 workspace members with 70 existing path edges, and `bash tools/check-crate-layout.sh`, locked/offline root metadata (including all features), and root fmt are green. The staged source-tree move and legacy manifest/lock removals complete the same structural increment.
- The adaptive and application leaf manifests retain their exact former plane lint tables instead of inheriting the broader root lint policy; this avoids treating a structural workspace move as a lint-policy rewrite.
- Post-canonical transplant procedure (do not run against the saved canonical checkout): create an integration branch at `53099825`, then apply the known concurrent compiler semantic commit: `git switch -c codex/shared-crates-integrated 53099825 && git cherry-pick 31fb61dc`. Replay this structural sequence with `git rebase --rebase-merges --onto HEAD 92ab6cf0 codex/shared-crates-migration` (equivalently cherry-pick `255375bb d2e3bb81 34fb9070 45c5bbd2 04cf8a30` and the rubric closure commit). If a replay conflict touches a moved Rust source or test, retain the integration branch's semantic body (the `ours` stage during rebase), move it using the old-to-new map above, and take the migration side only for `Cargo.toml`, root lock, path/script/Nix, Dylint, and documentation topology edits. Resolve each crate before the next: compiler `planes/compiler/crates/*` → `crates/*`; IR `domains/ir/crates/*` → `crates/*`; index portable `planes/index/crates/*` and adapters `planes/index/adapters/*` → `crates/*`; adaptive `planes/adaptive/crates/*` → `crates/*`; application `planes/application/crates/*` and `planes/application/gpui_shell` → `crates/*`; durable/observability `adapters/*` → their mapped `crates/*`. The compiler cherry-pick creates `planes/compiler/crates/nudox-compile-driver`, which is not in this preserved candidate: register it explicitly with `git mv workspace2/planes/compiler/crates/nudox-compile-driver workspace2/crates/nudox-compile-driver`, confirm the root `crates/*` member glob yields 31 members, and regenerate only the root lock with `cargo generate-lockfile --offline`; do not retain the compiler-plane lock. Then run the R1/R2/R4 commands and inspect `git diff --find-renames 92ab6cf0..HEAD -- workspace2/crates` to ensure the semantic bodies, including `VectorSegmentId`/Qdrant identities and compiler frontends, survived the path move without duplicate sources.

## Promoted
