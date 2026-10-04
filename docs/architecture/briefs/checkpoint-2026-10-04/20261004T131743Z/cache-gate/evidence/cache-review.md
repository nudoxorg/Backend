# Shared Cargo cache gate review packet

Date: 2026-10-04

## Candidate and worktree

- Base commit: `9a896bf197aed9028e9dbf14f0532eb00daad077`
- Base tree: `19146a7b96b3dda49d856ba777dc3794a7e20695`
- Branch: `codex/cache-shared-leaf-admission-20261004`
- Worktree: `/private/tmp/nudox-cache-shared-leaf-admission-20261004`
- Source changes are limited to `.config/scripts/cargo-rustc-cache.sh` and `tests/cargo-shared-cache.sh`.

## Runtime source-path finding

Mac layout receipt `materialization-v6-20261004T1230Z/layout-result.json` records the private CargoHome as:

`/Users/rmccrar6/.local/share/nudox/build-receipts/validation-9a896bf197aed9028e9dbf14f0532eb00daad077/cargo-home`

A read-only SSH listing confirmed:

- `cargo-home/registry -> /Users/rmccrar6/.cache/nudox/cargo-1.97/registry`
- `cargo-home/git -> /Users/rmccrar6/.cache/nudox/cargo-1.97/git`
- `cargo-home/registry/src` physically resolves to `/Users/rmccrar6/.cache/nudox/cargo-1.97/registry/src`
- `cargo-home/git/checkouts` physically resolves to `/Users/rmccrar6/.cache/nudox/cargo-1.97/git/checkouts`

The completed Cargo provenance JSON names the actual wrapper as `/nix/store/7z8y4b9wvyp9v954z790rv03y8488ic5-nudox-dependency-rustc-cache`, hash `d3ef4fd199ee1584b4561329e7b9fe416a04dd7e852e9e7c0ee64e99ba8c373c`. A read-only hash and excerpt inspection matched candidate 9a's gate: it canonicalizes the source parent, then compares that physical source path against the unresolved private strings `$cargo_home/registry/src/*` and `$cargo_home/git/checkouts/*` (generated wrapper lines 88-91). For dependency sources reached through the recorded symlink leaves, those path spellings cannot match. The cache wrapper is configured in the Cargo run script and its path/hash are present in provenance.

The Cargo log has ordinary `Compiling ...` lines for many dependencies but does not include verbose rustc argv. A bounded `ps` check happened after the process exited and found no rustc process. Therefore no exact rustc source argv is recorded here and no claim is made about a particular dependency's argv or a measured performance impact. The layout plus exact wrapper source establish the static mismatch for sources whose parents resolve below the shared roots.

A read-only `sccache --show-stats` against the recorded socket and cache directory returned all-zero counters. This observation is retained verbatim in the task transcript, but is not used as proof of bypass because client-side mode can affect where statistics are observed.

The separate runtime preflight receipt was `OWNED_CACHE_PREFLIGHT_NO_COMPILATION` and says `cargo_launched:false`; it is not evidence of a compile.

## Change

The wrapper now canonicalizes both allowlisted roots (`CARGO_HOME/registry/src` and `CARGO_HOME/git/checkouts`) before comparing them with the already-canonical source parent. Each empty/unresolvable root is checked explicitly so an empty value cannot turn the shell glob into `/*`. It retains the final-entry `-L` refusal and all existing crate-type/name/source-count gating.

The existing stub-based harness now creates a private CargoHome with real `registry` and `git` symlinks into a separate shared cache and verifies both ordinary sources use sccache. It also checks an intermediate directory symlink escaping into a workspace and a CargoHome with missing roots stay on direct rustc. Existing tests continue covering workspace, final-file symlink, vendored, proc-macro, build-script, ambiguous invocation, and direct-rustc status behavior.

## Verification

Command run from the isolated worktree:

`sh -n .config/scripts/cargo-rustc-cache.sh && sh -n tests/cargo-shared-cache.sh && sh tests/cargo-shared-cache.sh`

Result: exit 0; final output `cargo-shared-cache: PASS (affinity, role-graph leases, isolation, hard ceiling, stamps, provenance, daemon protocol isolation, exit propagation, stale recovery)`.

Also ran `git diff --check`: pass. No Cargo, build, Nix evaluation, native test, deploy, staging, commit, push, or runtime/cache mutation was performed.

## Final local source hashes

- `.config/scripts/cargo-rustc-cache.sh`: `63f4bc4c79f37fccb3cd57b2f9391a433f229e9bc2d7c37b06de5d54b94ff6b0`
- `tests/cargo-shared-cache.sh`: `6be3581ba813e417b20529c01e16b2b558020386c17c33574dd93e1798766e9c`

Full incremental patch: `source.diff` in this packet.
