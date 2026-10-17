# Report 27: current verification checkpoint

Updated 2026-10-17, 16:25 UTC. This brief distinguishes current observations from the [historical checkpoints](report22-current-readiness-20261013.md).

## Actual build and native results

Source `1fc3e0a3ab703c34d0af4288fab6dfd1e29df4e7`, tree `b0a7a53279d06511c2c4b81e0ba10929971dfe90`, built all four default-feature applications on ILO in 227.03 seconds: zero compiler errors and 1,706 warnings. Root independently verified 344 courier files, exact Cargo artifacts, independent executable copies, loader dependencies, source preservation, ECHILD closure and permit release.

| Binary | SHA-256 |
| --- | --- |
| CLI | `9317cb0ead8a7745d38bf3ea5b79417662d4012cc3c71b83b02cbff51ecfcfdb` |
| MCP | `927cabbfe9deefea767f4f0b0387296840ca362a2f0cae4bcac1920165579b88` |
| locald | `ddfc54ec64adf7a1162e1cffdadba858bf3e689619be1003b53cadd0e627b3c3` |
| Desktop | `53ea53896b6dedce41c981c14fb56f3dd77213fdfc2ded3f6cdc21d08524b05d` |

Root adoption SHA-256: `16a218108a26cdf1530f6b52339094e0cf2ad850d7f560e6d7483640099cb02b`. These are experiment candidates. The direct ELF GLIBC floor is 2.39; Ubuntu transitive compatibility and release acceptance remain unproved.

The separate native run executed **85 of 289 selected bodies: 84 passed, one failed, 204 unrun**. The remaining package bodies did not compile because of a stale test-only call. One TSZ library-origin assertion failed. Parser Clippy failed on three argument-count diagnostics; semantic-render Clippy passed. All-crate Clippy retained 24,246 warnings. The controller subsequently hit its retained-log limit, so no terminal `check-result.json` exists. Root verified raw checkpoint `cee76c67c044864dc6823987c47be951470b0cbecf7a60268264e7b4c1ca4020` and failed scope closure. That failure has not been relabeled.

Proposed repair source `0f23e973f49619131929aeb4731c7bf8e5bf9837`, tree `603efbc586c463d3cce9857b7b15c171edca21d6`, matches the PR's production/test postimages. Its 294 selected bodies and three Clippy commands remain unrun. No later source inherits an earlier pass.

## Actual GUI failure

GUI02 stopped when accounting encountered a driver-created tool symlink missing from its allowlist. The actor was terminated, reaped through ECHILD and its permit released. The outer receipt has no product or child exit field; the reaped primary exit is -15.

Root verified 160 copied files, 3,553,705 bytes and 37 loan journals. Replay: `actual-gui-native37-02/ROOT-REPLAY-GUI02-FAILURE.json`, SHA-256 `c5afed7a77129891cb45153307efc72ca0cb9c52f7166b1f11cd399fdac96c4e`. Eighteen private/runtime directories are explicitly omitted from this courier. Resource accounting separately scans the whole retained root before another grant.

The Rust process emitted drawn frames, but the observer found no visible PID-owned X11 window. Its black image is a desktop fallback, not an application capture. The configured private CargoHome was absent and owner startup failed; the missing window's cause remains unresolved. Reviewed driver corrections create CargoHome, avoid new tool aliases and retain display/environment diagnostics without weakening ownership checks. **Current GUI acceptance remains open.**

## Remaining application gates

| Gate | Current status |
| --- | --- |
| TypeScript identity and full search | Current37 full lexical chain and exact import/declaration checks remain unrun. Angular36 sampled early `published_names_only` pages. |
| Runtime admission | Fresh local, ILO and Prod census passed, with Mac freshly excluded as offline. A wrong native-accounting input lineage stopped preflight before any permit. The corrected immutable resume derives its binding from the actual current37 ordinary configuration and requires fresh guards. |
| Restart and removal | Current37 must prove graceful restart and post-removal cold state. The earlier second shutdown hit an exclusive-filename harness bug. |
| Python and Go | Fresh full Docs and PocketBase CLI/MCP runs remain unrun. The 456.50-second PocketBase diagnostic checked inferred JavaScript roots, not Go application acceptance. |
| Original reproductions | Exact ASP/GymBroApp sources remain unavailable here; representative projects do not close those reproductions. |
| Other report findings | Registry latency, row-count accounting, HTTP MCP auto-start, original N15, configured vector search and broad-corpus acceptance remain open. |
| Delivery | Historical installer/help checks are separate. No current binary release, Homebrew proof or current Mac bundle is established. |

Coworker refs were freshly fetched from Forgejo into a private bare store; canonical remains `83e9df85c7d3c8de8e4fba9a647f7ae6bc9576fc`. The temporarily unreadable Downloads checkout was not reset, replaced or edited. The preexisting Nix edit was not touched. Heavy compilation stays on ILO; Prod lacks destination disk for another workload. No production-readiness or full report closure is claimed.
