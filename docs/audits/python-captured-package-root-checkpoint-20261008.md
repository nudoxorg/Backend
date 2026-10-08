# Captured Python package-root checkpoint — 2026-10-08

Indexing a selected Python package directory containing `__init__.py` must preserve its actual module leaf. Previously the private captured tree placed it at a synthetic repository root, and Pyrefly could request an uncaptured parent when resolving package-relative imports. The faithful native control reproduced `UncapturedDependency` before this repair.

The captured layout now carries a typed package or repository root. Package captures use the native Pyrefly-admitted module leaf beneath a private import anchor, with a finite synthetic-parent witness. This preserves package-relative coordinates without granting access to the original parent directory, sibling packages, or site packages. Repository captures retain their existing root law. Layout identity participates in the capture fingerprint, and imported candidate witnesses are revalidated.

## Source checkpoint

Reviewed canonical base: `3bb94c2d81e71eeae26f06c809095fe245d36de3`.

Production/test replay: `b0c2c01ed965d1005bca085fea2cdd8a2b0a2a60`, tree `9035bd9a746d6668b560d4a60a1def73a6fd7e15`.

Root independently compared every recursive Git entry: all four changed files exactly match the native-tested predecessor `9ea1db5ecdeb4213037f809ca200a6bd6fdb9e47`; the other 17,941 entries match canonical. Canonical `Cargo.lock` remains `de731929bbf72c5220e59c0543aaddd06fcc2bc16899741a9d6cb5546e60780e`.

## Executed native evidence

These Linux tests ran in the existing ILO Nix/Cargo environment with two Cargo jobs, `--locked --offline`, fresh fleet admission, and owned-child kernel waits. Root independently read all four raw stdout/stderr streams, checked their receipt hashes, compared before/after source inventories, and verified the exact bytes and hashes of all 464 Python fixture sources.

| Gate | Actual source | Result |
| --- | --- | --- |
| Faithful selected-package regression | Baseline production plus the unchanged new control, `59111bf25cb84438ef8158c58bafab2d617efa53` | 0 passed, 1 failed: `UncapturedDependency` |
| Captured-layout controls | `9ea1db5ecdeb4213037f809ca200a6bd6fdb9e47` | 2 passed |
| Same selected-package regression | `9ea1db5ecdeb4213037f809ca200a6bd6fdb9e47` | 1 passed |
| Authentic Mealie selected-package control | `9ea1db5ecdeb4213037f809ca200a6bd6fdb9e47` | 1 passed; two fresh Pyrefly states |

Mealie is pinned to `879b133b5f98dd91419948f363d2d6d58793c3a0`, tree `c10eddbce31ed3529a78429a5c6ac1bde6f58c44`. Its selected package contains 464 Python sources totaling 1,564,950 bytes. The native control retains every source and checks a known call's original file/span and callee coordinates.

The raw archive SHA-256 is `6791b6cfcc59c1e128976e35915bb38efd968b77bff2f924c8df925213f34350`. The Root review is recorded in the adjacent JSON. All four owned process groups retired; both graph locks and original stamp identities were returned unchanged.

## Limits

The native predecessor used lock `afb3a7518cd1f19b29bee48ec1b3be3f3d888c1ad61dde6e7f8cd4e3976fd731`. The canonical replay with lock `de731…` has not been executed. These are source-specific native compiler controls, not current installed CLI/MCP/GUI acceptance. The existing installed `c0016d4…` release does not contain this repair.

Cargo reported fresh compiler artifacts for the first three gates and reuse at the same candidate source/path for the fourth. This archive does not retain executable bytes or their hashes, so it is not installed-binary provenance evidence. The older whole-application Mealie refusal's inner diagnostic remains unobserved; this checkpoint does not claim to have recovered that original cause.
