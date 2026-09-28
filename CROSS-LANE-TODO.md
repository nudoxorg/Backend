# Cross-compile CI lane — status & what's left

Branch `ci/cross-lane` (off canonical `92f974b37`). Adds Windows + aarch64-Linux
**compile** coverage to Backend CI. See plan §7.4 (B2) in `BACKEND-CI-PLAN.md`.

## Done on this branch (validated locally)

- **`fix(nix)`** (`11ed91c53`): the workspace source filter (`controlSourceRoots`
  in `.config/nix/tools.nix`) now keeps every `[patch]`-target vendor dir
  (`gpui-ce`, `gpui_ce_components_base`, `gpui_ce_macos`). Without it,
  `backend-control` — and entering most dev shells — fails on canonical with
  `failed to load source for dependency gpui-ce`. Confirmed by building
  `.#backend-control`.
- **`feat(ci)`** (`15f2684c6`): the cross lane.
  - `toolchains.crossCheckTargets = [ x86_64-pc-windows-gnu, aarch64-unknown-linux-gnu ]`,
    exported to the shell as `NUDOX_CROSS_CHECK_TARGETS`.
  - `.#cross` made **lean**: no longer spreads `common`, so it no longer drags
    the corpus, the seven language compilers, the GUI closure, or **Qdrant**
    (the ~37-min AVX-512 build) into the lane. Verified: the shell realizes in
    19 light derivations with none of those.
  - Standalone runner `.config/ci/cross-check.nu`, run as
    `nix develop .#cross -c nu .config/ci/cross-check.nu`. Streams cargo,
    reclaims each foreign target tree, exits non-zero on any failure.
  - Smoke-validated: `backend-platform` (win32 socket/security code)
    cross-checks clean for **both** targets.

## What's left

### 1. Full-workspace CI sweep (Backend — needs CI, can't run on the laptop)
The local checks compiled one crate per target. CI runs the real thing:
`cargo check --locked --workspace --all-targets --target <t>` for both targets.
Canonical product code is already well-guarded (`crates/platform/win32/`,
`#[cfg(windows)]` branches in the transports, guarded tests), so this is
expected to be close — but `--all-targets` compiles every test/bench too, so a
straggler test/tooling crate with an unguarded `std::os::unix` may surface.
**Action:** run the lane in CI; gate any straggler with `#[cfg(unix)]` /
`#[cfg(all(test, unix))]`.

### 2. MachineConfigurations wiring (MC repo — Robert applies; coordinate with Codex)
Pushing this Backend branch alone does **not** run the lane — the existing
Concourse pipelines don't know about it. MC needs one step, on the `linux`
worker, that runs:
```
nix develop .#cross -c nu .config/ci/cross-check.nu
```
and posts it as a Forgejo status (e.g. `concourse/backend-cross`). This is
plan §7.5 M1/M6 (the generic `backendLane` recipe + one word in `backendLanes`).
Codex is actively editing those files (Harmonia PR #15), so coordinate before
touching `system/services/concourse/dsl/`.

### 3. Follow-on lanes (later, out of scope for this branch)
From `BACKEND-CI-PLAN.md` §7:
- **`arm64` (native aarch64-linux run lane)** — needs a Hetzner CAX worker (M5).
  Cross-*check* here proves it compiles; running the tests natively is separate.
- **`windows` (native run lane)** — MSVC archive via `cargo-xwin` (B5) + a real
  Windows worker; and/or the `windows-wine` lane (B4) to run the `.exe` under
  Wine on the Linux host.
- **`macos` lane**, **portable screenshot tests** (B6), and the **release
  pipeline** (Layer 6) remain per the plan.

## Notes
- `x86_64-pc-windows-msvc` (needs cargo-xwin) and both Darwin targets (need the
  Apple SDK) are deliberately excluded from `crossCheckTargets`; they belong to
  their own native/emulated lanes.
- Disk hygiene: the runner deletes each `target/<triple>` tree after checking;
  keep an eye on `ilo` free space when the lane first runs at full workspace
  scale (plan M3/M4 cover serial groups and GC headroom).
