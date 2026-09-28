# Backend CI plan

The single source of truth for Backend's continuous integration: **where it stands, how it's
designed, and what comes next.** Backend owns *what* runs; MachineConfigurations
(`system/services/concourse/`) only schedules it.

Last updated 2026-09-28. Branch `ci/canonical-linux-baseline` (PR #8 → `canonical`),
head `d0fc947b9`.

---

## 1. Status

| | x86_64 Linux | aarch64 Linux | Windows x64 | macOS arm64 |
|---|---|---|---|---|
| Compiles in CI | ✅ | ❌ | ❌ | ❌ |
| Tests run in CI | ✅ (see §1.2) | ❌ | ❌ | ❌ |
| Real GUI window test in CI | ✅ `apps/desktop/tests/linux_window.rs` (Xvfb + Mesa) | ❌ | ❌ | ❌ |

### 1.1 PR #8: green on Linux

- **Build 2767** (`d0fc947b9`) passed the full PR lane: root-flake checks (~9.5 min) plus
  `backend test pr` (~21 min, ~3,700 tests: unit, integration, CLI/MCP/`locald` journeys,
  storage, crash, desktop runtime and the real Linux window).
- `canonical` was merged in at `fe0c4e979` (358 commits). **Every regression it introduced is
  fixed, with no regression quarantine.** They fell into three kinds:
  - **Product bugs:**
    - registry packages without a matching manifest no longer break view publication
    - `package-versions` lists every indexed version
    - index search skips external-target rows
    - the structural call graph skips pairs it can't place instead of failing
    - TypeScript callback parameters and index signatures lowered twice, and nested types
      overflowed the stack
    - C# explicit implementations of same-named interface members collided
    - the desktop showed an empty references list as "known" without a semantic publication
  - **Stale tests:** the MCP tool list, reads as references, Python receivers and first
    bindings, Java unresolved-dependency refusals, C# interface members and its golden image
  - **Test hygiene:** the Java corpus isolation, the tantivy cleanup race, timeouts under load,
    Linux keyboard shortcuts, float tolerance
- **Not yet mergeable:** `canonical` has moved **280+ commits** since, with conflicts in about
  10 files. PR #8 needs another sync and a green run before it merges.

### 1.2 What's still skipped

- **The PR quarantine** (`pr-quarantine` in `.config/nu/quality/test.nu`) holds 3 Linux-only GUI
  entries, about 18 tests:
  - 15 facet screenshot tests that need an off-screen renderer (GPUI only has one on macOS)
  - 3 facet/desktop tests that pin macOS-exact float, timing or sub-pixel values
  - §4.3 fixes these.
- **Deep-accuracy suites** (real-package corpora, reference and render snapshots, the 1,000-package
  audit) are excluded from the PR lane by design, and run in `backend test workspace`.
- **Known flake:** `backend-store` `dropping_a_real_pending_witness_is_drained_before_shutdown`
  (a pre-existing race).

### 1.3 ⚠️ The live gate changed on 2026-09-27

MachineConfigurations PR #15 (Harmonia store cache, applied on `ilo` at 19:20) replaced the live
`nudox-backend-pr` script with a crane Nix-derivation chain. It builds only `backend-mcp`'s crate
closure and runs its unit tests (`nextest -E 'kind(lib)'`). **The root-flake checks and
`backend test pr` no longer run in the live gate.** In this plan that derivation gate is the
`unit` lane, and it runs **alongside** the full `linux` lane (§3). This needs agreeing between
Robert and Codex before the next pipeline change.

---

## 2. Design: composable lanes

A **lane** is four interchangeable parts. Backend declares them in one table, and one command runs
any lane identically on a laptop or in CI:

```
backend ci <lane>
```

| Part | Meaning | Options |
|---|---|---|
| **Shell** | where it builds (a Nix dev shell) | `compiler`, `verification`, **`cross`** (exists in `.config/nix/shells.nix`, not yet used), `windows-wine` (new), none (a plain Windows host) |
| **Target** | what it builds for | `toolchains.crossTargets`: x86_64/aarch64 Linux, Windows GNU/MSVC, macOS arm64 |
| **Runner** | how test binaries execute | `native`; `wine` (via `CARGO_TARGET_<TRIPLE>_RUNNER`, so the tests don't know); `archive` (`cargo nextest archive` built on `ilo`, run on another machine with no Rust toolchain there) |
| **Selection** | which tests run | nextest filtersets: `unit`, `pr`, `platform` (the cross-platform core), `gui`, `deep` |

**MachineConfigurations stays generic:** one recipe `backendLane { lane, tags }` runs
`backend ci <lane>` on a worker with those tags and posts `concourse/backend-<lane>`. An
aggregate `concourse/backend` status is green only when every required lane is green, and branch
protection requires only that one. A serial group keeps heavy lanes on `ilo` from overlapping.
**A new platform is one row in the lane table plus one line in MachineConfigurations.**

**Tests are written once.** Platform differences live in the four parts, not in the tests.
`cfg(unix)` and `cfg(windows)` appear only where the OS genuinely differs, and a test that passes on
only one machine is a bug in the test.

---

## 3. Lane table

| Lane | Machine tag | Shell | Target | Runner | Selection | When |
|---|---|---|---|---|---|---|
| `fast` | `linux` | verification | host | — | fmt, clippy, structure, flake check | every PR, first |
| `unit` | `linux` | Harmonia derivations | host | native | `unit` | every PR |
| `linux` | `linux` | compiler | x86_64-linux | native | `pr` | every PR |
| `cross` | `linux` | cross | windows-gnu, aarch64-linux | `cargo check --all-targets` | — | every PR |
| `windows-wine` | `linux` | windows-wine | x86_64-windows-gnu | `wine` | `platform` + `gui` (`wine` driver) | every PR |
| `arm64` | `arm64` | compiler | aarch64-linux | native | `pr` | every PR, once the machine exists |
| `windows` | `windows` | none | x86_64-windows-msvc (archive built on `ilo`) | `archive` | `platform` + `gui` (`windows` driver) | every PR, or release candidates only |
| `macos` | `macos` | compiler | aarch64-darwin | native | `pr` + `gui` (`macos` driver) | later |
| `deep` | `linux` | compiler | host | native | `deep` | nightly |

Every lane has the same contract: exit 0 or 1, one summary, retained JUnit output and screenshots.

---

## 4. Work items

### 4.1 Window test drivers

`linux_window.rs` becomes `desktop_window.rs`, with the driver picked by `NUDOX_GUI_DRIVER`:

| Driver | Launch | Display | Capture | Close |
|---|---|---|---|---|
| `xvfb` | `backend-desktop` | Xvfb + openbox | `import` | `xdotool windowquit` |
| `wine` | `wine backend-desktop.exe`, `GPUI_DISABLE_DIRECT_COMPOSITION=1` | the same Xvfb | `import` | `xdotool windowquit` |
| `windows` | `backend-desktop.exe` | a logged-in session | a Win32 capture | `WM_CLOSE` |
| `macos` | `Nudox.app` | a logged-in session | `screencapture -l` | quit |

Every driver shares the same assertions: a visible window, rendered pixels, a clean close within 10 seconds.

### 4.2 Windows under Wine: why it's feasible

- **Rendering is Direct3D 11,** which DXVK translates to Vulkan (Mesa lavapipe in CI), the same
  path Proton uses for games.
- **Text is DirectWrite,** which Wine implements.
- **DirectComposition, which Wine handles poorly, can be turned off** through GPUI's
  `GPUI_DISABLE_DIRECT_COMPOSITION`.
- **Risk:** `gpui_ce_windows` requires DirectManipulation to create a window. If Wine rejects it,
  vendor `gpui_ce_windows` and make DirectManipulation optional.
- **Build note:** release builds need `fxc.exe`, so the Wine lane uses the dev profile, whose
  shaders compile at runtime.
- **What Wine doesn't cover:** real Windows semantics (named pipes, locks, ACLs, fsync,
  `MAX_PATH`). The `windows` lane covers those.

### 4.3 Portable screenshot tests

Build a **`wgpu` off-screen renderer**: implement `PlatformHeadlessRenderer` on `gpui_ce_wgpu`
(vendored beside `gpui-ce`) and return it from `current_headless_renderer()` on Linux and
Windows. It covers Linux, Windows and Wine in one piece of work. Pixel checks get a small
tolerance for per-OS text rasterization. Then remove the GUI quarantine.

### 4.4 Windows code readiness

- Forward-port Mikita's `fix/windows-build` (PR #7), which is 905 commits behind `canonical`.
- Work the open items in `WINDOWS_BUILD_ERRORS_FROM_EARLIER.md` (NuDox root):
  - ungated `std::os::unix` imports in `crates/engine/tests/typescript_authority.rs` and
    `apps/worker/src/service/tests.rs`
  - os error 123 (a filename built from a thread name)
  - `MAX_PATH` limits
  - no Windows candidates in native toolchain discovery
  - a hard dependency on `taskkill`

---

## 5. Machines

| Tag | Machine | Status |
|---|---|---|
| `linux` | `ilo` (i5-13500, 20 threads, KVM, 468 GB); disk I/O is the bottleneck, and one Backend build uses 100–130 GB transiently | ✅ |
| `arm64` | a small Hetzner ARM cloud server (CAX, 8+ cores, 16–32 GB, 150 GB), because QEMU on `ilo` would be 5–20× slower | to buy |
| `windows` | any Windows machine with a logged-in session (runs archives only); a VM on `ilo` isn't recommended | to decide, plus a license |
| `macos` | a Mac with a logged-in session | later |

**Before adding lanes on `ilo`:** daily Nix GC or `min-free`/`max-free`, plus the serial group.

---

## 6. Rollout

Each step adds one lane and stays green.

| # | Step | Repo | Needs |
|---|---|---|---|
| 0 | Sync PR #8 with `canonical`, green, then merge | Backend | — |
| 1 | `backend ci <lane>` plus the lane table (`fast`, `unit`, `linux`) | Backend | — |
| 2 | Generic `backendLane` recipe, aggregate status, serial group | MC | agreement on §1.3 with Codex |
| 3 | `cross` lane plus the Windows readiness fixes (§4.4) | Backend + one MC line | 1–2 |
| 4 | Window drivers plus the `windows-wine` lane (§4.1–4.2) | Backend + one MC line | 3 |
| 5 | ARM worker plus the `arm64` lane | MC | a CAX server |
| 6 | Windows worker, MSVC archive (`cargo-xwin`), plus the `windows` lane | MC + Backend | a machine and a license |
| 7 | `wgpu` off-screen renderer, then remove the GUI quarantine (§4.3) | Backend | — |
| 8 | `macos` lane | MC | a Mac worker |

Steps 0–4 need no new hardware.

## 7. Open decisions

1. The full `linux` lane alongside the unit-only derivation gate (§1.3).
2. The Windows `platform` selection: the proposal is process, transport, storage, `locald` ↔ CLI/MCP
   smoke, and the window test.
3. Where the Windows runner lives, plus the license.
4. Timing of the `canonical` sync (it keeps moving; agree a window with philocalyst).
