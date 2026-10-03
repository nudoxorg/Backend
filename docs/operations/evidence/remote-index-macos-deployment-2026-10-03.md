# Remote Mac index and compiler canary — 2026-10-03

## Scope and status

This is a one-host MacOS owner plus trusted MacOS worker staged on `h16001mac` (`100.106.68.17`, macOS 26.7 arm64). It is not a cross-host cluster, not a Linux-owner/Mac-worker result, and not a GUI-connection proof. No changes were made to ILO, its service, its full disk, or the Tailscale/NixOS configuration. No public unauthenticated listener was opened. The owner and worker use direct-only encrypted Iroh transport on the Tailscale address, with relays/public discovery disabled.

The runtime is currently started manually with `nohup`, not supervised by LaunchAgent or boot service. The current binaries are installed privately and versioned. Owner and worker processes are running at report time; process IDs and commands are recorded in the deployment state. Persistent activation is withheld until the remote worker assignment gate succeeds. OS reboot recovery is untested.

## Exact build and installation

Frozen source commit `a3a729c17617eddad58e4d901bbb2797dcbba610`, tree `68d71c723e897ac61914f9fcec1a101e9a1ab992`; source archive SHA-256 `7a9a342494454489daf3e7b21b800ed0e08d7c361edc0f5e1b26894b38ac7e6b`; source manifest SHA-256 `bfdac1ce73539af670ecb69047f7ead0d05b0200705efff8d74c77398ea2dcf6`; Cargo.lock SHA-256 `01167011cb3541f2b29d011c02d05d2577dede45732a571891c6207bcc5e0bef`; flake.lock SHA-256 `0f00535b1632c77b3376d00de10de932d609a4e71104c837b68dc52998c247f5`.

Build was a locked/offline Cargo release with one job, incremental disabled, and process-local `MACOSX_DEPLOYMENT_TARGET=26.0`. It exited 0. All four arm64 Mach-O artifacts report minimum OS 26.0 and SDK 14.4; they launch on host 26.7. The pinned Nix `libiconv` dependency is already rooted by installed Nix profiles. This only qualifies the deployment host and does not establish compatibility with older macOS releases. The pinned Nix C wrapper also emits a nonfatal target-triple mismatch diagnostic; 48 nonfatal `-?` compiler-family probe warnings were observed in the build log.

Versioned release candidate: `/Users/rmccrar6/.local/share/nudox/client-releases/a3a729c17617eddad58e4d901bbb2797dcbba610-macos-arm64-deploy26` (directories mode 0700, evidence files mode 0600). Installed binary SHA-256 values:

- `backend-cli`: `9c1bea1408e453fdc6ec4a48ede92240d04da6c5b22e6923ee8fab9da0302efd`
- `backend-locald`: `60987601892a8bc7e718e63bef38d082b839c7389ce301aff7c3f00f5aed60ad`
- `backend-mcp`: `785a65ee6f2b9ba8eeceb1a8e20b9f6c21a80d9a80731e187b68dff1ef490b21`
- `backend-worker`: `10a29d0bd0f4cb75626adb4ea0666707a5b19277b63510a68c32f20c60f616d4`

Full frozen build receipt directory: `/Users/rmccrar6/.local/share/nudox/build-receipts/a3a729c17617eddad58e4d901bbb2797dcbba610/sccache-fd8192-r2`.

## Private owner and worker setup

Durable deployment state is under `/Users/rmccrar6/.local/share/nudox/remote-indexes/a3a729c17617eddad58e4d901bbb2797dcbba610-macos26`, with owner-state, worker-state, fixture project, logs, and sanitized receipts. Top-level state and leaf service directories are owned by `rmccrar6`, mode 0700. Private owner and worker identity files are mode 0600. One-time invitation token material was not recorded in this report or printed; it was consumed during trust import.

Owner peer: `07b8adf2c8d1f3ccbbce7974fa097d2b845f5bb94de3b32bcb368f6462ac221d`, advertised on Tailscale UDP `100.106.68.17:60023`.

Worker peer: `02b34f22f860b638514f3d45db77f336650d32be153fc2ba1e9f15db03dad043`, Tailscale UDP `100.106.68.17:60024`. Worker identity show confirms direct-only transport and exactly one trusted coordinator and one execution grant after import.

The Rust 2024 scoped compiler tuple reported during preflight and persisted in the owner grant:

- namespace `909283423042e871d160cececa67590e`
- recipe `11b85689d713f73dba5f2ea99c0a0e5f891ab4ccb19ed16cf2f10468cee1270d`
- profile `0003`, stage `lower-ir`
- toolchain `0f0f3cdfaed7e97d6186e2655b9bf233e06276ced14a1ab08aead2126eb1b998`
- environment `81eea6ea64deee63b7f8b573465ee8aed7ca5bd7e7ff44f6c1274fc6eda5e555`
- target platform `f5b5702b1aeba9d46c34f2015a8351e9775e3d6ba8e72d0f871b073b80891336`

Pinned Rust and Cargo 1.97.1 capability inspection passed with explicit compiler/sysroot/Cargo-home/registry-root paths. `cluster scope show` succeeded. It reports `requires-project-inspection` for package eligibility and placement; that report field is not itself a dispatch predicate and is not treated as a failure.

## Actual indexing canary

A private, no-dependency Rust 2024 crate was created at `/Users/rmccrar6/.local/share/nudox/remote-indexes/a3a729c17617eddad58e4d901bbb2797dcbba610-macos26/project`.

The first `add` request was accepted and indexed locally. The selected semantic result for `cluster_deploy_smoke` was searchable; the source marker was `NUDOX_CLUSTER_SMOKE_V1`. A subsequent changed-input `index_start --execution-intent background` returned a typed ticket and reached terminal `published`; the selected V2 marker `NUDOX_CLUSTER_SMOKE_V2` was searchable. Both publications came from local fallback, not worker execution.

Exact runtime trace for the background V2 attempt:

```text
locald compiler input capture: mode=PathCopy changed_files=Some(1) path_copied_pages=3 source_bytes_read=536
locald compiler probe summary: matching_grants=0 candidates=0 no_exact_trust_grant=1
locald compiler route fallback: NoEligibleWorker
locald semantic profile selected from local compiler fallback
```

The current CLI scope report and the persisted owner trust JSON match exactly for namespace, recipe, profile, stage, toolchain, environment, and target platform. Both owner and worker inherited identical pinned compiler environment paths. This frozen build only reports an aggregate `no_exact_trust_grant`; no exact captured V2 manifest tuple or manifest-object ID/content hash was recorded in the receipts. No grant was broadened or reissued by guess. No remote worker offer/result/owner-ACK proof is claimed. Worker pending is empty, which is not an ACK proof by itself.

## Registry and readiness observations

The owner health query after local ingest reports readiness `ready`, revision `e2e057db…`, and 5 rows. It also explicitly reports semantic lane `unavailable` (`unconfigured`), embedding `unconfigured`, oracles 0/18 ready, and 17 `no-manifest`; this is not full semantic or compiler readiness.

`explore tokio --limit 3` returned zero rows. `index-search tokio --limit 3` returned an exact count of zero. `package pkg:cargo/serde@1.0.228` was refused with `package … does not match any indexed local manifest` (exit 2). Although the locald source has seven official default registry endpoints, no official registry metadata ingest/search catalog has yet been proven on this owner. A pinned official-package `add`/acquisition canary remains to be run.

## Classified setup failures and unverified gates

1. Starting locald with the socket under the long versioned state path failed: 122-byte Unix socket path exceeded macOS `sockaddr_un.sun_path` limit 103. The failure log and stale PID marker are preserved. Startup was corrected using the private durable socket directory `/Users/rmccrar6/.cache/ndx-a3a729c1/locald.sock` (47 bytes). This is an operational path constraint, not a source fix.
2. The trusted Mac worker did not match the real Add manifest scope; a3 diagnostics aggregate the mismatch but do not identify its field. Remote execution is therefore unproven and the runtime remains local-first.
3. No unauthorized remote query/revocation canary has been run against this new owner. The scoped import was authenticated and successful, but no cross-host client exercised the owner.
4. No LaunchAgent, boot-level service, login persistence, restart-after-OS-reboot, application-level backup/restore, official registry ingest, or GUI connection to this owner is proven.
5. The Linux ILO service and state are unchanged. ILO had previously been observed with no free disk, and cross-platform Linux-owner/Mac-worker grants are intentionally not attempted because the target platform identity must match.

## Receipt inventory

Sanitized deployment receipts are under `/Users/rmccrar6/.local/share/nudox/remote-indexes/a3a729c17617eddad58e4d901bbb2797dcbba610-macos26/receipts`; runtime logs are in sibling `logs/`. Key receipts include `scope-post-add.json`, `trust-post-add.json`, `add-baseline.json`, `index-start-v2.json`, `index-progress-v2.json`, `search-after-v2.json`, `health-after-v2.json`, `worker-pending-after-v2.txt`, and the registry canary outputs. No token, signing key, authority secret, or credential is included.
