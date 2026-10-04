# Closed Cargo graph clone protocol

This local-only prototype validates a closed Cargo receipt, then can create a private APFS CoW copy of only its role graph slot. It does not run Cargo, copy `CARGO_HOME`, copy `CARGO_TARGET_DIR`, copy provenance or owner records, or reuse source/destination leases. The default mode is read-only validation. An actual clone requires an explicit `--clone` argument and is limited to local macOS with source and destination on the same APFS volume.

No real receipt was passed to this helper, and no APFS clone was performed. The protocol tests use synthetic receipts and inject `shutil.copy2` in place of `clonefile(2)`. The test result is labeled `synthetic_clone_protocol_exercised`; it is not an APFS or Cargo result.

## Contract

Pass a JSON object with `schema: 1`, `source`, `destination`, and `reuse_identity`:

- `reuse_identity` binds Cargo/rustc versions, paths and binary hashes, rustc/runtime/source wrapper hashes, exact Cargo argument array, profile, target triple, feature selection, shared cache root, lock hash, an effective build-environment map and its digest, and an explicit map of unchanged input-file SHA-256 values. The environment map uses uppercase variable names and string or `null` values, where `null` means absent. Its digest is SHA-256 over `nudox-build-environment-v1\0` followed by sorted compact UTF-8 JSON. It must include `Cargo.toml` and `Cargo.lock` in `same_input_sha256`. The helper verifies every listed file under both immutable source roots. Include every additional input that policy requires to remain identical; the helper does not infer a complete source equivalence proof from the two required entries.
- `source` binds the closed receipt, attempt ID, provenance run ID, immutable source root, commit/tree, source manifest hash, lock hash, an explicitly accepted Cargo exit status, and an exact copy of `reuse_identity`.
- `destination` binds a freshly prepared empty receipt, its immutable source root, commit/tree, source manifest hash and lock hash, and an exact copy of `reuse_identity`.

Paths must be absolute and contain no symlink component. The source and destination roots, receipts, locks, toolchain, wrappers, commands, feature configuration, and selected build settings must agree with their receipt evidence. The helper refuses active owner PIDs, nonempty graph leases, nonempty destination roots, symlinks, special files, hard links, mismatched run-cargo scripts, and non-APFS or cross-volume clone attempts. `source.expected_cargo_exit` is checked against the closed result and provenance; the helper does not judge whether a nonzero test result is acceptable.

Dry-run invocation:

```sh
python3 clone_closed_cargo_graph.py --contract /absolute/path/to/reviewed-contract.json
```

Only after a separate review of the concrete receipts and contract should a caller consider the mutating form:

```sh
python3 clone_closed_cargo_graph.py --contract /absolute/path/to/reviewed-contract.json --clone
```

The clone form first requires matching historical graph-output and effective-environment witnesses. It then holds all four destination role-graph slot leases while it creates a staging tree, calls `clonefile(2)` for each regular file, writes the destination `.nudox-worktree-root` stamp, verifies file contents and distinct inodes, installs slot 0, and checks that the source tree inventory stayed unchanged. It removes a failed staging/install candidate and releases its leases on error. The inventory covers every path, file SHA-256 and size, type, POSIX mode/owner/group, mtime/ctime, device/inode and link count; it intentionally excludes atime, and it does not compare ACLs, extended attributes, or file flags. It hashes the source before and after, then hashes the staged and installed candidate; for a multi-gigabyte graph, expect several full reads. That cost is deliberate integrity checking, not a performance result.

On macOS the APFS guard resolves each path's mounted device with `/bin/df -P`, then calls `/usr/sbin/diskutil info -plist <device>` and checks the native `FilesystemType` and `DeviceIdentifier` keys. On this host, `stat -f '%T' /private/tmp` returned `/`, while `diskutil` returned `FilesystemType=apfs`; `%T` is not used as a filesystem-name signal.

The report explicitly marks that the original source provenance did not attest the graph outputs. In the reviewed f85 evidence the source run was closed with exit 101 (260 passed, one failed fixture, one ignored); the helper can confirm the recorded status but cannot establish that the failure is harmless. It snapshots and compares the graph at clone time, which detects concurrent or later mutation during the operation, but that does not prove the graph contents came from the recorded Cargo run.

## Provenance needed before treating reuse as evidence

The reviewed Cargo provenance record records `cargo_build_dir` but its `outputs` array is empty; it does not hash the role graph. The reviewed runtime wrapper identity was SHA-256 `3db7a569285fc426dead9aa77e958d9173bdb4ecfd66078fdc0aa9d199108100`; the shared Rust cache wrapper identity was `106d5183d4f26789d6eaf9466b5666720ec21cf92ecaa41046a28176eb39da84`. The latter routes compiler calls through sccache; it is not a build-graph integrity witness. Add `cargo_build_graph_inventory` to provenance with `schema: 1`, `capture_complete: true`, `capture_point: "after-cargo-exit-before-slot-release"`, the exact slot path, `inventory_sha256`, `entry_count`, `regular_files`, and `regular_bytes`. The digest is SHA-256 over `nudox-closed-cargo-graph-v1\0` followed by sorted compact UTF-8 JSON entries containing path, type, POSIX mode, UID/GID, and mtime; regular-file entries also contain size and SHA-256. The helper recomputes this from the current source slot and refuses `--clone` if the receipt is missing or mismatched.

The current provenance also does not record a complete effective compiler-affecting environment. Add `build_environment` with `schema: 1`, `capture_complete: true`, `capture_point: "cargo-invocation"`, the normalized values map, and its digest; the helper requires an exact map/digest match to the contract before `--clone`. The allowlist should cover policy-relevant compiler/build variables such as rustflags, profile overrides, target/linker settings and deployment target, while representing absent optional values explicitly as `null`. The exact source/destination runner script bytes, command/profile/target/features, and toolchain/wrapper identities are checked too, but they do not prove unrecorded ambient variables were equal.

The reviewed f85 provenance lacks both the historical graph inventory and the effective environment witness, so this prototype's dry run can report the gap but `--clone` refuses before APFS checks or destination mutation. No unproven-cache experiment mode is implemented. Neither this helper nor these tests establish that relocated Cargo fingerprints remain valid when the new immutable source root and build-graph path differ. The next Cargo run must use the ordinary reviewed wrapper and produce its own receipt; correctness and time savings remain unmeasured.

## Protocol tests

Run the local synthetic suite with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 test_clone_closed_cargo_graph.py
```

The tests exercise receipt/status binding, active owners and leases, input/toolchain/runner mismatches, symlinks and hard links, destination emptiness, same-volume refusal, staging cleanup, source mutation detection, stamp rebinding, and distinct destination inodes. They do not invoke macOS `clonefile(2)`, Cargo, SSH, or any real receipt.
