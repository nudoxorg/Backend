# Authority initializer private-file open repair

This evidence records the native macOS reproduction, the smallest repair, and the repaired-state verification. The raw supervisor receipts are preserved under `receipts/` with their original completion, launch, resource, stdout, and stderr files.

## Finding and repair

The initializer failure was at descriptor open, before `File::lock`, secret staging, or hard-link publication. A test-only syscall-stage trace reproduced the failure with 24 simultaneous opens under one pinned directory: 14 direct `openat` attempts returned `NotFound` (`ENOENT`) from `O_CREAT|O_NOFOLLOW`; no descriptor-admission trace followed. The earlier runtime trace independently recorded `initializer-descriptor-open` with `NotFound`.

`DirectoryCapability::open_private_file_read_write(name, true)` now attempts exclusive creation. If another caller has already created the child, it reopens the existing child without create and retains the existing `open_private(IfUnlinked::Reopen, ...)` admission loop. All opens still pass the established owner, regular-file, private-mode, single-link, and no-follow checks. The native suite verifies simultaneous callers retain the same admitted file identity and that symlink, hard-link, broad-mode, and directory occupants are rejected unchanged.

## Native results

The candidate source was tested remotely on `h16001mac` at commit `7f255a0e54a0f48e4a5914ea522165452e01f7e6`, tree `c7529ba25035f5652f55990cf2e594615e597669`. The equivalent local source commit was `f35c57d98768798e4805acc81eef553e361ddf48`, with the same tree. Each passing job has a fresh v5 fleet admission embedded in its completion receipt; source was clean before and after each job.

| Gate | Result | Receipt |
| --- | --- | --- |
| `backend-platform` native library suite, including pinned-directory contention and unsafe-child controls | 76 passed, 0 failed, 0 ignored | `receipts/startup-native-startup-private-open-backend-platform-20261007/` |
| Isolated 24-worker authority-secret initializer regression | 1 passed, 0 failed | `receipts/startup-native-startup-private-open-isolated-runtime-repeat-20261007-attempt02/` |
| Full `backend-runtime` library suite | 38 passed, 0 failed, 1 ignored | `receipts/startup-native-startup-private-open-full-runtime-20261007/` |

The baseline runtime receipt on commit `0bd76200c5c0eaa9d469b5310512e81e4b4d7de2` remains preserved: 37 passed, 1 failed, 1 ignored. The isolated `2d9fe2efccad3cbfe302db5e4d13d501f8df537d` failure and the `e52f133bcf57ffa13ebaf7ecb9eee6440f789fa8` direct-open trace failure are preserved separately as pre-fix RED evidence.

## Source and executable identities

The tested source file is `crates/platform/directory.rs`, Git blob `6b3cfb97f875757fbda1ef59f84d693f78b0055b`, SHA-256 `e9e14f40dc3545696a1c49d4366198593289988e448fe13374f08a0fc06543f1`. `Cargo.lock` is unchanged; its SHA-256 is `b508bc8978b37e09e54887a76cdb7055a4188f1243770057784a08ad61560268`. `source-binary-identities.json` records the complete source/lock Git IDs and SHA-256 values, plus the actual remote test-executable paths, byte sizes, and SHA-256 values.

## Attempts with no native pass credit

The malformed-commit supervisor request was rejected before Cargo started. The first isolated-repeat admission was rejected because its census was 30.51 seconds old at supervisor admission. A separate managed-runner invocation supplied `cargo test` where the runner expects `test`; it returned “no such command: cargo” and ran no tests. `non-native-attempts.json` records these as harness/admission errors, not native test results; the accepted retry used a fresh census and passed.

The original dirty `/Users/mileswirht/Downloads/backend/.config/nix/tools.nix` was not edited; this work and all new evidence are in the isolated repair worktree.
