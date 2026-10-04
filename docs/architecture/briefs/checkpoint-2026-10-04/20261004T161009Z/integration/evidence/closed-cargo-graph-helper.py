#!/usr/bin/env python3
"""Fail-closed, local APFS clone for a closed Nudox Cargo build-graph slot.

This tool deliberately does not copy CargoHome, target directories, provenance,
owner records, or lease state. It is a candidate-reuse experiment, not a build
or correctness claim. Run without --clone to validate and print a plan.
"""

from __future__ import annotations

import argparse
import ctypes
import errno
import hashlib
import json
import os
import pathlib
import plistlib
import re
import shlex
import stat
import subprocess
import sys
from typing import Any, Callable


class SafetyError(RuntimeError):
    pass


STAMP = ".nudox-worktree-root"
SCHEMA = 1
_CHUNK = 1024 * 1024
_EXPECTED_TOP_LEVEL_LAYOUT_STATUS = "isolated_empty_build_roots_prepared"
_REQUIRED_BUILD_IDENTITY = {
    "cargo_version",
    "cargo_path",
    "cargo_executable_sha256",
    "rustc_version",
    "rustc_path",
    "rustc_executable_sha256",
    "rustc_wrapper_sha256",
    "runtime_wrapper_sha256",
    "source_wrapper_sha256",
    "command",
    "profile",
    "target_triple",
    "features",
    "environment",
    "shared_cache",
    "lock_sha256",
    "effective_environment_sha256",
}
_GRAPH_INVENTORY_SCHEMA = 1
_BUILD_ENVIRONMENT_SCHEMA = 1
_GRAPH_INVENTORY_DOMAIN = b"nudox-closed-cargo-graph-v1\x00"
_BUILD_ENVIRONMENT_DOMAIN = b"nudox-build-environment-v1\x00"


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise SafetyError(message)


def _canonical_absolute(value: Any, label: str) -> pathlib.Path:
    _require(isinstance(value, str) and value and "\n" not in value and "\x00" not in value,
             f"{label} must be a nonempty single-line path")
    path = pathlib.Path(value)
    _require(path.is_absolute(), f"{label} must be absolute")
    _require(".." not in path.parts, f"{label} must not contain '..'")
    current = pathlib.Path(path.anchor)
    for part in path.parts[1:]:
        current = current / part
        try:
            item = current.lstat()
        except FileNotFoundError:
            break
        except OSError as error:
            raise SafetyError(f"cannot inspect {label} component {current}: {error}") from error
        _require(not stat.S_ISLNK(item.st_mode), f"{label} contains symlink component: {current}")
    resolved = path.resolve(strict=False)
    _require(resolved == path, f"{label} is not already canonical: {path} -> {resolved}")
    return path


def _regular_nosymlink(path: pathlib.Path, label: str) -> os.stat_result:
    try:
        details = path.lstat()
    except OSError as error:
        raise SafetyError(f"cannot inspect {label}: {error}") from error
    _require(not stat.S_ISLNK(details.st_mode), f"{label} is a symlink: {path}")
    _require(stat.S_ISREG(details.st_mode), f"{label} is not a regular file: {path}")
    return details


def _directory_nosymlink(path: pathlib.Path, label: str) -> os.stat_result:
    try:
        details = path.lstat()
    except OSError as error:
        raise SafetyError(f"cannot inspect {label}: {error}") from error
    _require(not stat.S_ISLNK(details.st_mode), f"{label} is a symlink: {path}")
    _require(stat.S_ISDIR(details.st_mode), f"{label} is not a directory: {path}")
    return details


def _lexists(path: pathlib.Path) -> bool:
    """Like exists(), but recognize dangling symlinks so they fail closed."""
    try:
        path.lstat()
        return True
    except FileNotFoundError:
        return False
    except OSError as error:
        raise SafetyError(f"cannot inspect path {path}: {error}") from error


def _private_owned_dir(path: pathlib.Path, label: str) -> None:
    details = _directory_nosymlink(path, label)
    _require(details.st_uid == os.getuid(), f"{label} is not owned by this user: {path}")
    _require(stat.S_IMODE(details.st_mode) == 0o700, f"{label} must have mode 0700: {path}")


def _read_json(path: pathlib.Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(_read_regular_bytes(path, label).decode("utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise SafetyError(f"cannot read {label}: {error}") from error
    _require(isinstance(value, dict), f"{label} must be a JSON object")
    return value


def _read_kv(path: pathlib.Path, label: str) -> dict[str, str]:
    result: dict[str, str] = {}
    try:
        lines = _read_regular_bytes(path, label).decode("utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise SafetyError(f"cannot read {label}: {error}") from error
    for line in lines:
        if not line or "=" not in line:
            continue
        key, value = line.split("=", 1)
        _require(key not in result, f"{label} contains duplicate field {key}")
        result[key] = value
    return result


def _sha256_file(path: pathlib.Path, label: str) -> str:
    expected = _regular_nosymlink(path, label)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    digest = hashlib.sha256()
    try:
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as source:
            before = os.fstat(source.fileno())
            _require(_stable_file_identity(before) == _stable_file_identity(expected),
                     f"{label} changed before hashing")
            while True:
                block = source.read(_CHUNK)
                if not block:
                    break
                digest.update(block)
            after = os.fstat(source.fileno())
    except OSError as error:
        raise SafetyError(f"cannot hash {label}: {error}") from error
    _require(_stable_file_identity(before) == _stable_file_identity(after),
             f"{label} changed while hashing")
    return digest.hexdigest()


def _stable_file_identity(details: os.stat_result) -> tuple[int, int, int, int, int, int, int]:
    return (
        details.st_dev,
        details.st_ino,
        details.st_size,
        details.st_mtime_ns,
        details.st_ctime_ns,
        details.st_nlink,
        stat.S_IMODE(details.st_mode),
    )


def _read_regular_bytes(path: pathlib.Path, label: str) -> bytes:
    expected = _regular_nosymlink(path, label)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as source:
            before = os.fstat(source.fileno())
            _require(_stable_file_identity(before) == _stable_file_identity(expected),
                     f"{label} changed before reading")
            data = source.read()
            after = os.fstat(source.fileno())
    except OSError as error:
        raise SafetyError(f"cannot read {label}: {error}") from error
    _require(_stable_file_identity(before) == _stable_file_identity(after),
             f"{label} changed while reading")
    return data


def _normalized_text(value: str) -> str:
    return " ".join(value.split())


def _effective_environment_sha256(values: dict[str, Any]) -> str:
    encoded = json.dumps(values, ensure_ascii=True, separators=(",", ":"),
                         sort_keys=True).encode("utf-8")
    return hashlib.sha256(_BUILD_ENVIRONMENT_DOMAIN + encoded).hexdigest()


def _process_start_token(pid: int) -> str | None:
    try:
        completed = subprocess.run(
            ["/bin/ps", "-p", str(pid), "-o", "lstart="],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SafetyError(f"cannot check owner PID {pid}: {error}") from error
    if completed.returncode != 0:
        if completed.stderr.strip():
            raise SafetyError(f"ps could not inspect owner PID {pid}: {completed.stderr.strip()}")
        return None
    token = _normalized_text(completed.stdout)
    _require(bool(token), f"ps returned no start token for owner PID {pid}")
    return token


def _check_no_active_owners(receipt: pathlib.Path, label: str,
                            start_lookup: Callable[[int], str | None]) -> None:
    attempts = receipt / "attempts"
    if not _lexists(attempts):
        return
    _directory_nosymlink(attempts, f"{label} attempts directory")
    for attempt in sorted(attempts.iterdir()):
        _directory_nosymlink(attempt, f"{label} attempt directory")
        owner_path = attempt / "current-cargo-owner.txt"
        if not _lexists(owner_path):
            continue
        owner = _read_kv(owner_path, f"{label} Cargo owner record")
        pid_text = owner.get("pid")
        recorded_start = owner.get("start")
        _require(pid_text is not None and pid_text.isdigit() and int(pid_text) > 1,
                 f"{label} owner record has invalid PID: {owner_path}")
        _require(bool(recorded_start), f"{label} owner record has no start time: {owner_path}")
        current_start = start_lookup(int(pid_text))
        if current_start is not None and _normalized_text(current_start) == _normalized_text(recorded_start):
            raise SafetyError(f"{label} Cargo owner is still active (PID {pid_text})")


def _tree_inventory(root: pathlib.Path, label: str, *, hash_contents: bool) -> tuple[list[dict[str, Any]], str]:
    """Inventory a tree without following links; reject any non-regular payload."""
    root_stat = _directory_nosymlink(root, f"{label} root")
    device = root_stat.st_dev
    entries: list[dict[str, Any]] = [{
        "path": "",
        "kind": "dir",
        "mode": stat.S_IMODE(root_stat.st_mode),
        "uid": root_stat.st_uid,
        "gid": root_stat.st_gid,
        "mtime_ns": root_stat.st_mtime_ns,
        "ctime_ns": root_stat.st_ctime_ns,
        "dev": root_stat.st_dev,
        "ino": root_stat.st_ino,
        "nlink": root_stat.st_nlink,
    }]

    def walk(directory: pathlib.Path, relative: pathlib.PurePosixPath) -> None:
        try:
            children = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as error:
            raise SafetyError(f"cannot enumerate {label} directory {directory}: {error}") from error
        for child in children:
            child_path = pathlib.Path(child.path)
            child_rel = relative / child.name
            try:
                details = child.stat(follow_symlinks=False)
            except OSError as error:
                raise SafetyError(f"cannot inspect {label} entry {child_path}: {error}") from error
            _require(details.st_dev == device, f"{label} crosses filesystem device at {child_path}")
            _require(details.st_uid == os.getuid(), f"{label} entry is not owned by this user: {child_path}")
            _require(not stat.S_ISLNK(details.st_mode), f"{label} contains symlink: {child_path}")
            if stat.S_ISDIR(details.st_mode):
                entries.append({
                    "path": child_rel.as_posix(),
                    "kind": "dir",
                    "mode": stat.S_IMODE(details.st_mode),
                    "uid": details.st_uid,
                    "gid": details.st_gid,
                    "mtime_ns": details.st_mtime_ns,
                    "ctime_ns": details.st_ctime_ns,
                    "dev": details.st_dev,
                    "ino": details.st_ino,
                    "nlink": details.st_nlink,
                })
                walk(child_path, child_rel)
                continue
            _require(stat.S_ISREG(details.st_mode), f"{label} contains special file: {child_path}")
            _require(details.st_nlink == 1,
                     f"{label} contains hard-linked file (nlink={details.st_nlink}): {child_path}")
            digest = None
            if hash_contents:
                digest = _hash_open_file(child_path, details, label)
            entries.append({
                "path": child_rel.as_posix(),
                "kind": "file",
                "mode": stat.S_IMODE(details.st_mode),
                "uid": details.st_uid,
                "gid": details.st_gid,
                "size": details.st_size,
                "mtime_ns": details.st_mtime_ns,
                "ctime_ns": details.st_ctime_ns,
                "dev": details.st_dev,
                "ino": details.st_ino,
                "nlink": details.st_nlink,
                "sha256": digest,
            })

    walk(root, pathlib.PurePosixPath())
    entries.sort(key=lambda item: item["path"])
    serialized = json.dumps(entries, ensure_ascii=True, separators=(",", ":"), sort_keys=True).encode()
    return entries, hashlib.sha256(serialized).hexdigest()


def _graph_output_inventory_sha256(entries: list[dict[str, Any]]) -> str:
    """Stable output witness excluding clone-specific inode/ctime identity."""
    stable: list[dict[str, Any]] = []
    for entry in entries:
        output = {
            "path": entry["path"],
            "kind": entry["kind"],
            "mode": entry["mode"],
            "uid": entry["uid"],
            "gid": entry["gid"],
            "mtime_ns": entry["mtime_ns"],
        }
        if entry["kind"] == "file":
            output["size"] = entry["size"]
            output["sha256"] = entry["sha256"]
        stable.append(output)
    encoded = json.dumps(stable, ensure_ascii=True, separators=(",", ":"),
                         sort_keys=True).encode("utf-8")
    return hashlib.sha256(_GRAPH_INVENTORY_DOMAIN + encoded).hexdigest()


def _graph_inventory_metrics(entries: list[dict[str, Any]]) -> dict[str, int]:
    files = [entry for entry in entries if entry["kind"] == "file"]
    return {
        "entry_count": len(entries),
        "regular_files": len(files),
        "regular_bytes": sum(entry["size"] for entry in files),
    }


def _closed_run_attestation_issues(provenance: dict[str, Any], source_slot: pathlib.Path,
                                   entries: list[dict[str, Any]],
                                   identity: dict[str, Any]) -> list[str]:
    issues: list[str] = []
    expected_hash = _graph_output_inventory_sha256(entries)
    expected_metrics = _graph_inventory_metrics(entries)
    graph = provenance.get("cargo_build_graph_inventory")
    if not isinstance(graph, dict):
        issues.append("missing historical Cargo build-graph inventory")
    else:
        if graph.get("schema") != _GRAPH_INVENTORY_SCHEMA:
            issues.append("unsupported historical build-graph inventory schema")
        if graph.get("capture_complete") is not True:
            issues.append("historical build-graph inventory is not marked complete")
        if graph.get("capture_point") != "after-cargo-exit-before-slot-release":
            issues.append("historical graph inventory was not captured at the closed-run boundary")
        if graph.get("slot") != str(source_slot):
            issues.append("historical build-graph inventory names a different slot")
        if graph.get("inventory_sha256") != expected_hash:
            issues.append("historical build-graph inventory does not match current slot contents")
        for name, expected in expected_metrics.items():
            if graph.get(name) != expected:
                issues.append(f"historical build-graph inventory {name} does not match current slot")

    environment = provenance.get("build_environment")
    if not isinstance(environment, dict):
        issues.append("missing historical effective build-environment witness")
    else:
        if environment.get("schema") != _BUILD_ENVIRONMENT_SCHEMA:
            issues.append("unsupported historical build-environment schema")
        if environment.get("capture_complete") is not True:
            issues.append("historical effective build environment is not marked complete")
        if environment.get("capture_point") != "cargo-invocation":
            issues.append("historical environment witness was not captured at Cargo invocation")
        if environment.get("values") != identity["environment"]:
            issues.append("historical effective build environment differs from reuse contract")
        if environment.get("sha256") != identity["effective_environment_sha256"]:
            issues.append("historical effective build-environment digest differs from reuse contract")
    return issues


def _hash_open_file(path: pathlib.Path, expected: os.stat_result, label: str) -> str:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        fd = os.open(path, flags)
        with os.fdopen(fd, "rb") as source:
            before = os.fstat(source.fileno())
            _require(_stable_file_identity(before) == _stable_file_identity(expected),
                     f"{label} file changed before hashing: {path}")
            digest = hashlib.sha256()
            while True:
                block = source.read(_CHUNK)
                if not block:
                    break
                digest.update(block)
            after = os.fstat(source.fileno())
    except OSError as error:
        raise SafetyError(f"cannot hash {label} file {path}: {error}") from error
    _require(_stable_file_identity(before) == _stable_file_identity(after),
             f"{label} file changed while hashing: {path}")
    return digest.hexdigest()


def _check_apfs_same_volume(source: pathlib.Path, destination_parent: pathlib.Path) -> None:
    _require(sys.platform == "darwin", "actual cloning is supported only on local macOS")
    try:
        source_device = _mounted_device_for_path(source)
        destination_device = _mounted_device_for_path(destination_parent)
    except (OSError, subprocess.SubprocessError) as error:
        raise SafetyError(f"cannot query source/destination filesystem: {error}") from error
    _require(source_device["filesystem_type"] == "apfs"
             and destination_device["filesystem_type"] == "apfs",
             "source and destination must both be APFS (got "
             f"{source_device['filesystem_type']!r}, {destination_device['filesystem_type']!r})")
    _require(source_device["device_identifier"] == destination_device["device_identifier"],
             "source and destination resolve to different mounted devices")
    _require(os.stat(source).st_dev == os.stat(destination_parent).st_dev,
             "source slot and destination build root are on different devices")


def _parse_df_device(stdout: str) -> str:
    lines = stdout.splitlines()
    _require(len(lines) >= 2, "df returned no mounted device row")
    fields = lines[1].split()
    _require(bool(fields) and re.fullmatch(r"/dev/disk[0-9]+(?:s[0-9]+)*", fields[0]) is not None,
             "df returned an unfamiliar mounted-device path")
    return fields[0]


def _parse_diskutil_info_plist(data: bytes, expected_device: str) -> dict[str, str]:
    try:
        info = plistlib.loads(data)
    except (plistlib.InvalidFileException, ValueError) as error:
        raise SafetyError(f"diskutil returned malformed info plist: {error}") from error
    _require(isinstance(info, dict), "diskutil info plist must be a dictionary")
    filesystem_type = info.get("FilesystemType")
    device_identifier = info.get("DeviceIdentifier")
    _require(isinstance(filesystem_type, str) and filesystem_type,
             "diskutil info plist lacks FilesystemType")
    _require(isinstance(device_identifier, str) and device_identifier,
             "diskutil info plist lacks DeviceIdentifier")
    _require(expected_device == "/dev/" + device_identifier,
             "diskutil returned a different DeviceIdentifier than df")
    return {
        "filesystem_type": filesystem_type.casefold(),
        "device_identifier": device_identifier,
    }


def _mounted_device_for_path(path: pathlib.Path) -> dict[str, str]:
    df = subprocess.run(
        ["/bin/df", "-P", str(path)],
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=5,
    )
    device = _parse_df_device(df.stdout)
    result = subprocess.run(
        ["/usr/sbin/diskutil", "info", "-plist", device],
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10,
    )
    return _parse_diskutil_info_plist(result.stdout, device)


def _clonefile(source: pathlib.Path, destination: pathlib.Path) -> None:
    """Invoke macOS clonefile(2); destination must not exist."""
    _require(sys.platform == "darwin", "clonefile(2) is available only on macOS")
    library = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    function = library.clonefile
    function.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint32]
    function.restype = ctypes.c_int
    result = function(os.fsencode(source), os.fsencode(destination), 0)
    if result != 0:
        error_number = ctypes.get_errno() or errno.EIO
        raise SafetyError(
            f"clonefile failed for {source} -> {destination}: "
            f"{os.strerror(error_number)}"
        )


def _parse_cargo_args(value: str, label: str) -> list[str]:
    try:
        arguments = shlex.split(value, posix=True)
    except ValueError as error:
        raise SafetyError(f"cannot parse {label} argv: {error}") from error
    return arguments


def _profile_from_command(command: list[str]) -> str:
    cargo_args = command[:command.index("--")] if "--" in command else command
    _require(bool(cargo_args) and cargo_args[0] in {"test", "build", "check", "run"},
             "unsupported Cargo command for graph reuse")
    explicit_profiles = [arg.split("=", 1)[1] for arg in cargo_args if arg.startswith("--profile=")]
    _require(len(explicit_profiles) <= 1, "multiple Cargo profiles in command")
    release_flags = [arg for arg in cargo_args if arg in {"--release", "-r"}]
    _require(len(release_flags) <= 1, "multiple Cargo release flags in command")
    if explicit_profiles:
        _require(not release_flags, "Cargo command combines --profile with --release")
        return explicit_profiles[0]
    for index, arg in enumerate(cargo_args[:-1]):
        if arg == "--profile":
            raise SafetyError("separate --profile value is not supported in the reuse contract")
    if release_flags:
        return "release"
    return "test" if cargo_args[0] == "test" else "dev"


def _target_from_command(command: list[str], rustc_version: str) -> str:
    cargo_args = command[:command.index("--")] if "--" in command else command
    target_values: list[str] = []
    for index, arg in enumerate(cargo_args):
        if arg.startswith("--target="):
            _require(bool(arg.split("=", 1)[1]), "empty Cargo --target value")
            target_values.append(arg.split("=", 1)[1])
        elif arg == "--target":
            _require(index + 1 < len(cargo_args) and bool(cargo_args[index + 1]),
                     "Cargo --target is missing its value")
            target_values.append(cargo_args[index + 1])
    _require(len(target_values) <= 1, "multiple Cargo target arguments")
    if target_values:
        return target_values[0]
    for line in rustc_version.splitlines():
        if line.startswith("host: "):
            return line[len("host: "):]
    raise SafetyError("rustc verbose version does not contain a host target")


def _identity_contract(contract: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    _require(contract.get("schema") == SCHEMA, f"contract schema must be {SCHEMA}")
    source = contract.get("source")
    destination = contract.get("destination")
    identity = contract.get("reuse_identity")
    _require(isinstance(source, dict), "contract source must be an object")
    _require(isinstance(destination, dict), "contract destination must be an object")
    _require(isinstance(identity, dict), "contract reuse_identity must be an object")
    _require(_REQUIRED_BUILD_IDENTITY.issubset(identity),
             "reuse_identity is missing required build identity fields")
    _require(identity.get("profile") in {"test", "dev", "release"} and isinstance(identity.get("profile"), str),
             "reuse_identity.profile must be test, dev, or release")
    _require(isinstance(identity.get("command"), list) and all(isinstance(x, str) for x in identity["command"]),
             "reuse_identity.command must be a string array")
    for name in ("cargo_version", "cargo_path", "rustc_version", "rustc_path", "profile", "target_triple"):
        _require(isinstance(identity.get(name), str) and bool(identity[name]),
                 f"reuse_identity.{name} must be a nonempty string")
    _require(isinstance(identity.get("features"), dict), "reuse_identity.features must be an object")
    _require(isinstance(identity.get("environment"), dict), "reuse_identity.environment must be an object")
    environment = identity["environment"]
    for name, value in environment.items():
        _require(isinstance(name, str) and re.fullmatch(r"[A-Z][A-Z0-9_]*", name) is not None,
                 "reuse_identity.environment keys must be uppercase variable names")
        _require(value is None or isinstance(value, str),
                 f"reuse_identity.environment.{name} must be a string or null")
    _require(environment.get("CARGO_INCREMENTAL") == "0",
             "graph reuse requires CARGO_INCREMENTAL=0")
    _require(environment.get("CARGO_BUILD_JOBS") == "1",
             "this helper only accepts the reviewed single-job build layout")
    _require(environment.get("NUDOX_CARGO_BUILD_SLOTS") == "1",
             "graph reuse requires the reviewed single-slot build layout")
    _require(isinstance(environment.get("MACOSX_DEPLOYMENT_TARGET"), str)
             and bool(environment["MACOSX_DEPLOYMENT_TARGET"]),
             "reuse_identity.environment must bind MACOSX_DEPLOYMENT_TARGET")
    _require(isinstance(identity["effective_environment_sha256"], str)
             and re.fullmatch(r"[0-9a-f]{64}", identity["effective_environment_sha256"]) is not None,
             "reuse_identity.effective_environment_sha256 must be lowercase SHA-256")
    _require(identity["effective_environment_sha256"] == _effective_environment_sha256(environment),
             "reuse_identity effective environment digest does not match its values")
    _require(isinstance(identity.get("same_input_sha256"), dict),
             "reuse_identity.same_input_sha256 must bind unchanged build inputs")
    _require({"Cargo.toml", "Cargo.lock"}.issubset(identity["same_input_sha256"]),
             "same_input_sha256 must bind Cargo.toml and Cargo.lock")
    _require(identity["same_input_sha256"]["Cargo.lock"] == identity["lock_sha256"],
             "same_input_sha256 Cargo.lock hash differs from lock_sha256")
    _canonical_absolute(identity["shared_cache"], "reuse_identity.shared_cache")
    for side_name, side in (("source", source), ("destination", destination)):
        for field in ("receipt", "source_root", "commit", "tree", "source_manifest_sha256", "lock_sha256"):
            _require(isinstance(side.get(field), str) and side[field],
                     f"contract {side_name}.{field} is required")
        _canonical_absolute(side["receipt"], f"{side_name} receipt")
        _canonical_absolute(side["source_root"], f"{side_name} source_root")
        _require(re.fullmatch(r"[0-9a-f]{64}", side["source_manifest_sha256"]) is not None,
                 f"{side_name}.source_manifest_sha256 must be lowercase SHA-256")
        _require(re.fullmatch(r"[0-9a-f]{64}", side["lock_sha256"]) is not None,
                 f"{side_name}.lock_sha256 must be lowercase SHA-256")
        for revision_field in ("commit", "tree"):
            _require(re.fullmatch(r"[0-9a-f]{40}", side[revision_field]) is not None,
                     f"{side_name}.{revision_field} must be lowercase 40-digit Git identity")
    _require(source["receipt"] != destination["receipt"], "source and destination receipts must differ")
    _require(source["source_root"] != destination["source_root"], "source and destination source roots must differ")
    _require(source["lock_sha256"] == destination["lock_sha256"] == identity.get("lock_sha256"),
             "source, destination, and build contract lock hashes must match")
    _require(_profile_from_command(identity["command"]) == identity["profile"],
             "build profile does not match the Cargo command")
    _require(_target_from_command(identity["command"], identity["rustc_version"]) == identity["target_triple"],
             "target triple does not match the Cargo command or rustc host")
    source_identity = source.get("reuse_identity")
    destination_identity = destination.get("reuse_identity")
    _require(source_identity == identity and destination_identity == identity,
             "source and destination reuse identities must exactly equal reuse_identity")
    for name in ("cargo_executable_sha256", "rustc_executable_sha256", "rustc_wrapper_sha256",
                 "runtime_wrapper_sha256", "source_wrapper_sha256"):
        _require(isinstance(identity[name], str)
                 and re.fullmatch(r"[0-9a-f]{64}", identity[name]) is not None,
                 f"reuse_identity.{name} must be lowercase SHA-256")
    return source, destination, identity


def _resolve_receipt_layout(side: dict[str, Any], label: str) -> tuple[pathlib.Path, dict[str, Any]]:
    receipt = _canonical_absolute(side["receipt"], f"{label} receipt")
    _private_owned_dir(receipt, f"{label} receipt")
    layout = _read_json(receipt / "layout-result.json", f"{label} layout result")
    _require(layout.get("receipt") == str(receipt), f"{label} layout receipt path mismatch")
    _require(layout.get("source_commit") == side["commit"], f"{label} source commit mismatch")
    _require(layout.get("source_tree") == side["tree"], f"{label} source tree mismatch")
    _require(layout.get("source_manifest_sha256") == side["source_manifest_sha256"],
             f"{label} source manifest hash mismatch")
    identity = side.get("reuse_identity")
    if isinstance(identity, dict):
        for rel, expected_hash in identity.get("same_input_sha256", {}).items():
            _require(isinstance(rel, str) and rel and not pathlib.PurePosixPath(rel).is_absolute()
                     and ".." not in pathlib.PurePosixPath(rel).parts,
                     "same_input_sha256 keys must be safe relative paths")
            input_path = pathlib.Path(side["source_root"]) / rel
            _require(_sha256_file(input_path, f"{label} build input {rel}") == expected_hash,
                     f"{label} build input hash mismatch: {rel}")
    _require(layout.get("cargo_lock_sha256") == side["lock_sha256"],
             f"{label} Cargo.lock hash mismatch")
    _require(layout.get("status") == _EXPECTED_TOP_LEVEL_LAYOUT_STATUS,
             f"{label} receipt is not an isolated prepared layout")
    source_root = _canonical_absolute(side["source_root"], f"{label} source root")
    release = _canonical_absolute(str(layout.get("source_release", "")), f"{label} source release")
    _require(release / "source" == source_root, f"{label} source root is not the layout's immutable release")
    _directory_nosymlink(source_root, f"{label} source root")
    lock_path = source_root / "Cargo.lock"
    _require(_sha256_file(lock_path, f"{label} Cargo.lock") == side["lock_sha256"],
             f"{label} Cargo.lock content hash mismatch")
    manifest_path = release / "receipt" / "source-verification.json"
    _require(_sha256_file(manifest_path, f"{label} source manifest") == side["source_manifest_sha256"],
             f"{label} source manifest content hash mismatch")
    for key in ("cargo_home", "target_dir", "build_graph_dir"):
        path = _canonical_absolute(str(layout.get(key, "")), f"{label} {key}")
        _require(path.is_relative_to(receipt), f"{label} {key} escapes its receipt")
        _directory_nosymlink(path, f"{label} {key}")
        _private_owned_dir(path, f"{label} {key}")
    reviewed = layout.get("reviewed_wrapper_sha256")
    _require(isinstance(reviewed, dict) and reviewed,
             f"{label} layout does not bind its preparation wrappers")
    for name, expected_hash in reviewed.items():
        _require(isinstance(name, str) and pathlib.PurePosixPath(name).name == name,
                 f"{label} wrapper name is not a receipt-local basename")
        _require(isinstance(expected_hash, str)
                 and re.fullmatch(r"[0-9a-f]{64}", expected_hash) is not None,
                 f"{label} wrapper hash is invalid: {name}")
        _require(_sha256_file(receipt / name, f"{label} preparation wrapper {name}") == expected_hash,
                 f"{label} preparation wrapper hash mismatch: {name}")
    return receipt, layout



def _check_runner_settings(receipt: pathlib.Path, layout: dict[str, Any],
                           identity: dict[str, Any], label: str) -> str:
    candidates = sorted(receipt.glob("run-cargo*.sh"))
    _require(len(candidates) == 1, f"{label} receipt must contain exactly one run-cargo script")
    runner = candidates[0]
    _regular_nosymlink(runner, f"{label} run-cargo script")
    try:
        runner_bytes = _read_regular_bytes(runner, f"{label} run-cargo script")
        source = runner_bytes.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise SafetyError(f"cannot read {label} run-cargo script: {error}") from error
    runner_sha256 = hashlib.sha256(runner_bytes).hexdigest()
    reviewed = layout.get("reviewed_wrapper_sha256", {})
    _require(reviewed.get(runner.name) == runner_sha256,
             f"{label} run-cargo script no longer matches its reviewed hash")
    for name in ("CARGO_INCREMENTAL", "CARGO_BUILD_JOBS", "NUDOX_CARGO_BUILD_SLOTS",
                 "MACOSX_DEPLOYMENT_TARGET"):
        assignments = re.findall(rf"(?m)(?:^|\s|;)\b{re.escape(name)}=([^\s;]+)", source)
        _require(len(assignments) == 1,
                 f"{label} runner must assign {name} exactly once")
        _require(assignments[0] == identity["environment"][name],
                 f"{label} runner {name} differs from contract")
    target_assignments = re.findall(r"(?m)(?:^|\s|;)\bCARGO_BUILD_TARGET=([^\s;]+)", source)
    _require(len(target_assignments) <= 1, f"{label} runner assigns CARGO_BUILD_TARGET more than once")
    if target_assignments:
        _require(target_assignments[0] == identity["target_triple"],
                 f"{label} runner CARGO_BUILD_TARGET differs from contract")
    return runner_sha256

def _check_source_run(source: dict[str, Any], identity: dict[str, Any],
                      receipt: pathlib.Path, layout: dict[str, Any],
                      start_lookup: Callable[[int], str | None]
                      ) -> tuple[pathlib.Path, pathlib.Path, str, dict[str, Any]]:
    attempt_id = source.get("attempt_id")
    run_id = source.get("provenance_run_id")
    expected_exit = source.get("expected_cargo_exit")
    _require(isinstance(attempt_id, str) and attempt_id and "/" not in attempt_id and ".." not in attempt_id,
             "source.attempt_id is required and must be one path component")
    _require(isinstance(run_id, str) and re.fullmatch(r"[0-9]+-[0-9]+", run_id) is not None,
             "source.provenance_run_id is required")
    _require(isinstance(expected_exit, int) and not isinstance(expected_exit, bool)
             and 0 <= expected_exit <= 255,
             "source.expected_cargo_exit must be an integer exit status")
    _check_no_active_owners(receipt, "source", start_lookup)
    attempts_root = receipt / "attempts"
    _directory_nosymlink(attempts_root, "source attempts root")
    attempt_dir = attempts_root / attempt_id
    _directory_nosymlink(attempt_dir, "source attempt")
    result = _read_kv(attempt_dir / "result.txt", "source attempt result")
    owner = _read_kv(attempt_dir / "current-cargo-owner.txt", "source owner record")
    log_path = attempt_dir / "cargo.log"
    _require(_sha256_file(log_path, "source Cargo log") == result.get("log_sha256"),
             "source Cargo log hash does not match result receipt")
    _require(_sha256_file(attempt_dir / "current-cargo-owner.txt", "source owner record")
             == result.get("owner_record_sha256"),
             "source owner record hash does not match result receipt")
    _require(result.get("source_commit") == source["commit"], "source result commit mismatch")
    _require(result.get("source_tree") == source["tree"], "source result tree mismatch")
    _require(result.get("lock_sha256") == source["lock_sha256"], "source result lock hash mismatch")
    _require(result.get("source_manifest_sha256") == source["source_manifest_sha256"],
             "source result manifest hash mismatch")
    _require(result.get("cargo_exit") == str(expected_exit)
             and result.get("stage_exit") == str(expected_exit),
             "source Cargo result does not match the explicit expected exit status")
    _require(result.get("source_verification_after_exit") == "0",
             "source run did not pass its post-run source verification")
    _require(result.get("rustc_wrapper_sha256_expected") == identity["rustc_wrapper_sha256"]
             and result.get("rustc_wrapper_sha256_after") == identity["rustc_wrapper_sha256"]
             and result.get("rustc_wrapper_verification_after_exit") == "0",
             "source run rustc wrapper identity was not verified")
    _require(bool(result.get("cargo_completed_utc")) and bool(result.get("completed_utc")),
             "source run is not closed with completion timestamps")
    _require(owner.get("source") == source["source_root"], "source owner root mismatch")
    _require(owner.get("source_commit") == source["commit"] and owner.get("source_tree") == source["tree"],
             "source owner revision mismatch")
    actual_command = _parse_cargo_args(owner.get("argv", ""), "source owner")
    _require(actual_command == identity["command"], "source owner Cargo argv differs from reuse contract")
    _require(owner.get("cargo_version") == identity["cargo_version"],
             "source owner Cargo version differs from reuse contract")
    _require(owner.get("rustc_wrapper_sha256") == identity["rustc_wrapper_sha256"],
             "source owner rustc wrapper hash differs from reuse contract")
    _require(owner.get("macos_deployment_target") == identity["environment"].get("MACOSX_DEPLOYMENT_TARGET"),
             "source owner deployment target differs from reuse contract")

    target_dir = pathlib.Path(str(layout["target_dir"]))
    runner_sha256 = _check_runner_settings(receipt, layout, identity, "source")
    provenance_path = target_dir / ".nudox-provenance" / f"{run_id}.json"
    provenance = _read_json(provenance_path, "source Cargo provenance")
    _require(provenance.get("run_id") == run_id,
             "source provenance run ID differs from the requested closed run")
    _require(provenance.get("workspace_root") == source["source_root"],
             "source provenance root mismatch")
    _require(provenance.get("git_head") is None,
             "source release unexpectedly contains Git metadata")
    _require(provenance.get("source_changed_during_build") is False,
             "source changed during the closed Cargo run")
    _require(provenance.get("source_dirty_sha256_after") == provenance.get("source_dirty_sha256"),
             "source dirty witness changed during the run")
    _require(provenance.get("cargo_lock_sha256") == source["lock_sha256"]
             and provenance.get("cargo_lock_sha256_after") == source["lock_sha256"],
             "provenance Cargo.lock witness mismatch")
    _require(provenance.get("cargo_exit_status") == expected_exit,
             "provenance Cargo status differs from attempt result")
    features = provenance.get("features")
    _require(features == identity["features"], "source features differ from reuse contract")
    toolchain = provenance.get("toolchain", {})
    _require(isinstance(toolchain, dict), "source toolchain provenance is malformed")
    _require(toolchain.get("capture_complete") is True and toolchain.get("changed_during_build") is False,
             "source toolchain identity is incomplete or changed during the run")
    before = toolchain.get("executables_before", {})
    _require(isinstance(before, dict), "source executable provenance is malformed")
    for tool, version_key, path_key, hash_key in (
        ("cargo", "cargo_version", "cargo_path", "cargo_executable_sha256"),
        ("rustc", "rustc_version", "rustc_path", "rustc_executable_sha256"),
    ):
        record = before.get(tool, {})
        _require(record.get("version") == identity[version_key],
                 f"source {tool} version differs from reuse contract")
        _require(record.get("path") == identity[path_key],
                 f"source {tool} path differs from reuse contract")
        _require(record.get("sha256") == identity[hash_key],
                 f"source {tool} binary hash differs from reuse contract")
    wrapper = provenance.get("wrapper", {})
    _require(isinstance(wrapper, dict), "source wrapper provenance is malformed")
    _require(wrapper.get("runtime_sha256") == identity["runtime_wrapper_sha256"],
             "source runtime wrapper hash differs from reuse contract")
    _require(wrapper.get("source_sha256") == identity["source_wrapper_sha256"],
             "source wrapper source hash differs from reuse contract")
    _require(wrapper.get("rustc_sha256") == identity["rustc_wrapper_sha256"],
             "source rustc wrapper hash differs from reuse contract")
    _require(provenance.get("cargo_build_dir") == str(
        pathlib.Path(str(layout["build_graph_dir"])) / ".nudox-cargo" / "slot-0"),
        "source provenance graph slot is not the expected role slot")
    source_slot = pathlib.Path(str(provenance["cargo_build_dir"]))
    _directory_nosymlink(source_slot, "source build graph slot")
    _private_owned_dir(source_slot, "source build graph slot")
    _regular_nosymlink(source_slot / STAMP, "source graph slot stamp")
    _require(_read_regular_bytes(source_slot / STAMP, "source graph slot stamp")
             .decode("utf-8").strip() == source["source_root"],
             "source graph slot stamp does not match its immutable source root")
    _require(provenance.get("cargo_target_dir") == str(target_dir),
             "source provenance target path mismatch")
    _require(_profile_from_command(actual_command) == identity["profile"],
             "source run profile differs from reuse contract")
    _require(_target_from_command(actual_command, identity["rustc_version"]) == identity["target_triple"],
             "source run target differs from reuse contract")
    _require(identity["lock_sha256"] == source["lock_sha256"],
             "source lock identity differs from reuse contract")
    return source_slot, provenance_path, runner_sha256, provenance


def _check_destination(destination: dict[str, Any], identity: dict[str, Any],
                       receipt: pathlib.Path, layout: dict[str, Any],
                       start_lookup: Callable[[int], str | None]) -> tuple[pathlib.Path, str]:
    _check_no_active_owners(receipt, "destination", start_lookup)
    _require(not destination.get("attempt_id"), "destination must be a fresh, unbuilt receipt")
    _require(not destination.get("provenance_run_id"), "destination must be a fresh, unbuilt receipt")
    attempts = receipt / "attempts"
    if _lexists(attempts):
        _directory_nosymlink(attempts, "destination attempts root")
        _require(next(attempts.iterdir(), None) is None,
                 "destination receipt already contains an attempt")
    target = pathlib.Path(str(layout["target_dir"]))
    runner_sha256 = _check_runner_settings(receipt, layout, identity, "destination")
    target_entries = sorted(path.name for path in target.iterdir())
    allowed_target = [] if not _lexists(target / STAMP) else [STAMP]
    _require(target_entries == allowed_target, "destination target directory is not empty")
    if _lexists(target / STAMP):
        _regular_nosymlink(target / STAMP, "destination target stamp")
        _require(_read_regular_bytes(target / STAMP, "destination target stamp")
                 .decode("utf-8").strip() == destination["source_root"],
                 "destination target stamp has the wrong source root")
    build_root = pathlib.Path(str(layout["build_graph_dir"]))
    _require(not _lexists(build_root / ".nudox-provenance"),
             "destination graph root contains provenance")
    lane_root = build_root / ".nudox-cargo"
    destination_slot = lane_root / "slot-0"
    leases = lane_root / "leases"
    if _lexists(lane_root):
        _directory_nosymlink(lane_root, "destination role graph namespace")
        for child in lane_root.iterdir():
            if child.name == "slot-0":
                raise SafetyError("destination role graph slot already exists")
            _require(child.name == "leases",
                     f"destination role graph contains unexpected entry: {child.name}")
        if _lexists(leases):
            _directory_nosymlink(leases, "destination leases")
            _require(next(leases.iterdir(), None) is None, "destination has a role-graph lease")
    if _lexists(destination_slot):
        raise SafetyError("destination role graph slot already exists")
    if layout.get("old_source_target_or_build_graph_reused") is not False:
        raise SafetyError("destination layout does not attest empty, non-reused build roots")
    _require(layout.get("shared_cache") == identity["shared_cache"],
             "destination shared Cargo cache root differs from reuse contract")
    return build_root, runner_sha256


def _assert_dest_empty_tree(path: pathlib.Path) -> None:
    if _lexists(path):
        _directory_nosymlink(path, "destination graph namespace")
        allowed = {"leases"}
        names = {item.name for item in path.iterdir()}
        _require(names.issubset(allowed), "destination graph namespace has unexpected entries")
        leases = path / "leases"
        if _lexists(leases):
            _directory_nosymlink(leases, "destination role-graph leases")
            _require(next(leases.iterdir(), None) is None, "destination role-graph lease is not empty")


def _remove_tree_no_follow(path: pathlib.Path) -> None:
    try:
        details = path.lstat()
    except FileNotFoundError:
        return
    if stat.S_ISDIR(details.st_mode) and not stat.S_ISLNK(details.st_mode):
        for child in os.scandir(path):
            _remove_tree_no_follow(pathlib.Path(child.path))
        path.rmdir()
    else:
        path.unlink()




def _acquire_role_graph_leases(lease_dir: pathlib.Path, workspace_root: str) -> list[pathlib.Path]:
    """Block all four runtime graph slots for the duration of the clone."""
    _directory_nosymlink(lease_dir, "destination leases")
    start = _process_start_token(os.getpid())
    _require(start is not None, "cannot establish helper process start token")
    acquired: list[pathlib.Path] = []
    try:
        for index in range(4):
            lock = lease_dir / f"slot-{index}.lock"
            try:
                lock.mkdir(mode=0o700)
            except FileExistsError as error:
                raise SafetyError(f"destination graph lease is already held: {lock}") from error
            acquired.append(lock)
            for name, value in (
                ("pid", str(os.getpid())),
                ("start", start),
                ("workspace", workspace_root),
            ):
                path = lock / name
                descriptor = os.open(
                    path,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
                    0o600,
                )
                with os.fdopen(descriptor, "w", encoding="utf-8") as output:
                    output.write(value + "\n")
                    output.flush()
                    os.fsync(output.fileno())
        return acquired
    except Exception:
        _release_role_graph_leases(acquired)
        raise


def _release_role_graph_leases(locks: list[pathlib.Path]) -> None:
    errors: list[str] = []
    for lock in reversed(locks):
        try:
            _directory_nosymlink(lock, "helper graph lease")
            for name in ("pid", "start", "workspace"):
                path = lock / name
                if _lexists(path):
                    _regular_nosymlink(path, "helper graph lease metadata")
                    path.unlink()
            lock.rmdir()
        except OSError as error:
            errors.append(f"{lock}: {error}")
        except SafetyError as error:
            errors.append(f"{lock}: {error}")
    if errors:
        raise SafetyError("could not release helper graph lease(s): " + "; ".join(errors))

def _clone_tree(source: pathlib.Path, staging: pathlib.Path,
                clone_one: Callable[[pathlib.Path, pathlib.Path], None]) -> int:
    copied = 0
    source_stat = _directory_nosymlink(source, "source graph slot")
    _require(not _lexists(staging), f"clone staging path already exists: {staging}")
    staging.mkdir(mode=0o700)

    def descend(src_dir: pathlib.Path, dst_dir: pathlib.Path, *, root: bool = False) -> None:
        nonlocal copied
        for child in sorted(os.scandir(src_dir), key=lambda entry: entry.name):
            if root and child.name == STAMP:
                continue
            src = pathlib.Path(child.path)
            dst = dst_dir / child.name
            details = child.stat(follow_symlinks=False)
            _require(details.st_dev == source_stat.st_dev, f"source graph crosses device at {src}")
            _require(not stat.S_ISLNK(details.st_mode), f"source graph contains symlink: {src}")
            if stat.S_ISDIR(details.st_mode):
                dst.mkdir(mode=0o700)
                descend(src, dst)
                os.chmod(dst, stat.S_IMODE(details.st_mode))
                os.utime(dst, ns=(dst.stat().st_atime_ns, details.st_mtime_ns))
                continue
            _require(stat.S_ISREG(details.st_mode), f"source graph contains special file: {src}")
            _require(details.st_nlink == 1, f"source graph contains hard link: {src}")
            clone_one(src, dst)
            copied += 1

    try:
        descend(source, staging, root=True)
        os.chmod(staging, stat.S_IMODE(source_stat.st_mode))
        os.utime(staging, ns=(staging.stat().st_atime_ns, source_stat.st_mtime_ns))
    except Exception:
        _remove_tree_no_follow(staging)
        raise
    return copied


def _destination_inventory_equivalent(source_entries: list[dict[str, Any]],
                                      destination_entries: list[dict[str, Any]]) -> None:
    source_map = {entry["path"]: entry for entry in source_entries if entry["path"] != STAMP}
    destination_map = {entry["path"]: entry for entry in destination_entries if entry["path"] != STAMP}
    _require(source_map.keys() == destination_map.keys(), "cloned graph path inventory differs")
    for path, source in source_map.items():
        destination = destination_map[path]
        _require(source["kind"] == destination["kind"], f"cloned graph entry type differs: {path}")
        _require(source["mode"] == destination["mode"], f"cloned graph mode differs: {path}")
        _require(source["uid"] == destination["uid"] and source["gid"] == destination["gid"],
                 f"cloned graph ownership differs: {path}")
        if source["kind"] == "dir":
            _require(source["mtime_ns"] == destination["mtime_ns"],
                     f"cloned graph directory mtime differs: {path or '.'}")
        if source["kind"] == "file":
            _require(source["size"] == destination["size"] and source["sha256"] == destination["sha256"],
                     f"cloned graph file bytes differ: {path}")
            _require(source["mtime_ns"] == destination["mtime_ns"],
                     f"cloned graph file mtime differs: {path}")
            _require(destination["nlink"] == 1, f"cloned graph file is hard-linked: {path}")
            _require((source["dev"], source["ino"]) != (destination["dev"], destination["ino"]),
                     f"clone unexpectedly shares source inode: {path}")


def clone_closed_graph(contract: dict[str, Any], *, execute: bool,
                       start_lookup: Callable[[int], str | None] = _process_start_token,
                       clone_one: Callable[[pathlib.Path, pathlib.Path], None] = _clonefile,
                       apfs_check: Callable[[pathlib.Path, pathlib.Path], None] = _check_apfs_same_volume) -> dict[str, Any]:
    source_spec, destination_spec, identity = _identity_contract(contract)
    source_receipt, source_layout = _resolve_receipt_layout(source_spec, "source")
    destination_receipt, destination_layout = _resolve_receipt_layout(destination_spec, "destination")
    source_slot, provenance_path, source_runner_sha256, provenance = _check_source_run(
        source_spec, identity, source_receipt, source_layout, start_lookup
    )
    destination_build_root, destination_runner_sha256 = _check_destination(
        destination_spec, identity, destination_receipt, destination_layout, start_lookup
    )
    _require(source_runner_sha256 == destination_runner_sha256,
             "source and destination run-cargo scripts differ")
    _require(source_layout.get("shared_cache") == identity["shared_cache"],
             "source shared Cargo cache root differs from reuse contract")
    _require(source_slot != destination_build_root, "source and destination build roots alias")
    _require(not source_slot.is_relative_to(destination_build_root)
             and not destination_build_root.is_relative_to(source_slot),
             "source and destination graph roots overlap")
    destination_namespace = destination_build_root / ".nudox-cargo"
    _assert_dest_empty_tree(destination_namespace)
    destination_slot = destination_namespace / "slot-0"
    _require(not _lexists(destination_slot),
             "destination graph slot already exists")

    source_entries_before, source_inventory_before = _tree_inventory(
        source_slot, "source graph", hash_contents=True
    )
    attestation_issues = _closed_run_attestation_issues(
        provenance, source_slot, source_entries_before, identity
    )
    source_stamp = source_slot / STAMP
    _require(_read_regular_bytes(source_stamp, "source graph slot stamp")
             .decode("utf-8").strip() == source_spec["source_root"],
             "source graph stamp changed after receipt validation")
    source_leases = pathlib.Path(str(source_layout["build_graph_dir"])) / ".nudox-cargo" / "leases"
    _directory_nosymlink(source_leases, "source role-graph leases")
    _require(next(source_leases.iterdir(), None) is None, "source role-graph lease is not empty")

    report: dict[str, Any] = {
        "schema": SCHEMA,
        "status": "validated_plan" if not attestation_issues else "validated_plan_unattested",
        "source_commit": source_spec["commit"],
        "destination_commit": destination_spec["commit"],
        "source_root": source_spec["source_root"],
        "destination_source_root": destination_spec["source_root"],
        "source_provenance": str(provenance_path),
        "source_graph_slot": str(source_slot),
        "destination_graph_slot": str(destination_slot),
        "source_inventory_sha256_before": source_inventory_before,
        "source_regular_files": sum(entry["kind"] == "file" for entry in source_entries_before),
        "source_regular_bytes": sum(entry.get("size", 0) for entry in source_entries_before),
        "source_slot_hardlinks": 0,
        "target_dir_copied": False,
        "cargo_home_copied": False,
        "lease_state_copied": False,
        "provenance_copied": False,
        "clone_method": "clonefile(2)" if clone_one is _clonefile else "injected test callback",
        "filesystem_check": "APFS same-volume check" if apfs_check is _check_apfs_same_volume
        else "injected test callback",
        "source_run_cargo_script_sha256": source_runner_sha256,
        "destination_run_cargo_script_sha256": destination_runner_sha256,
        "source_cargo_exit": source_spec["expected_cargo_exit"],
        "destination_cargo_run": "not_run",
        "source_graph_outputs_hash_attested_by_original_receipt": not any(
            issue.startswith(("missing historical Cargo build-graph", "unsupported historical build-graph",
                              "historical build-graph", "historical graph inventory"))
            for issue in attestation_issues
        ),
        "source_effective_build_environment_attested_by_original_receipt": not any(
            issue.startswith(("missing historical effective build-environment", "unsupported historical build-environment",
                              "historical effective build environment", "historical environment witness"))
            for issue in attestation_issues
        ),
        "reuse_attestation_issues": attestation_issues,
        "same_cargo_home_path": str(source_layout["cargo_home"]) == str(destination_layout["cargo_home"]),
        "same_shared_cache_root": str(source_layout.get("shared_cache")) == str(destination_layout.get("shared_cache")),
        "note": "local reuse experiment only; no speed or correctness claim",
    }
    if not execute:
        return report
    _require(not attestation_issues,
             "refusing --clone without matching closed-run graph and environment provenance: "
             + "; ".join(attestation_issues))

    apfs_check(source_slot, destination_build_root)
    _private_owned_dir(destination_build_root, "destination build graph root")
    _private_owned_dir(destination_namespace, "destination graph namespace") if _lexists(destination_namespace) else None
    _require(os.stat(source_slot).st_dev == os.stat(destination_build_root).st_dev,
             "source slot and destination build root are on different devices")
    leases = destination_namespace / "leases"
    if not _lexists(destination_namespace):
        destination_namespace.mkdir(mode=0o700)
    if not _lexists(leases):
        leases.mkdir(mode=0o700)
    _private_owned_dir(destination_namespace, "destination graph namespace")
    _private_owned_dir(leases, "destination leases")
    _require(next(leases.iterdir(), None) is None, "destination leases became nonempty before clone")
    staging = destination_namespace / f".slot-0-clone.tmp-{os.getpid()}"
    _require(not _lexists(staging), "clone staging path already exists")
    copied_count = 0
    installed = False
    role_leases: list[pathlib.Path] = []
    try:
        role_leases = _acquire_role_graph_leases(leases, destination_spec["source_root"])
        copied_count = _clone_tree(source_slot, staging, clone_one)
        stamp_path = staging / STAMP
        descriptor = os.open(
            stamp_path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(destination_spec["source_root"] + "\n")
            output.flush()
            os.fsync(output.fileno())
        source_root_mtime = next(entry["mtime_ns"] for entry in source_entries_before
                                 if entry["path"] == "")
        os.utime(staging, ns=(staging.stat().st_atime_ns, source_root_mtime))
        _private_owned_dir(staging, "cloned destination slot")
        destination_entries, destination_inventory = _tree_inventory(
            staging, "destination graph", hash_contents=True
        )
        _destination_inventory_equivalent(source_entries_before, destination_entries)
        _require(_read_regular_bytes(staging / STAMP, "destination graph stamp")
                 .decode("utf-8").strip()
                 == destination_spec["source_root"],
                 "destination graph stamp was not rebound to its source root")
        _require(not _lexists(destination_slot), "destination graph slot appeared during clone")
        os.rename(staging, destination_slot)
        installed = True
        destination_entries_after, destination_inventory_after = _tree_inventory(
            destination_slot, "installed destination graph", hash_contents=True
        )
        _destination_inventory_equivalent(source_entries_before, destination_entries_after)
        source_entries_after, source_inventory_after = _tree_inventory(
            source_slot, "source graph after clone", hash_contents=True
        )
        _require(source_inventory_before == source_inventory_after,
                 "source graph inventory changed while cloning")
        _require(source_entries_before == source_entries_after,
                 "source graph files or metadata changed while cloning")
        source_leases_after = pathlib.Path(str(source_layout["build_graph_dir"])) / ".nudox-cargo" / "leases"
        _require(next(source_leases_after.iterdir(), None) is None,
                 "source role-graph lease appeared while cloning")
        report.update({
            "status": "cloned_private_apfs_candidate"
            if clone_one is _clonefile and apfs_check is _check_apfs_same_volume
            else "synthetic_clone_protocol_exercised",
            "destination_inventory_sha256": destination_inventory_after,
            "cloned_regular_files": copied_count,
            "destination_stamp": str(destination_slot / STAMP),
            "destination_stamp_value": destination_spec["source_root"],
        })
    except Exception:
        _remove_tree_no_follow(staging)
        if installed:
            _remove_tree_no_follow(destination_slot)
        raise
    finally:
        try:
            _release_role_graph_leases(role_leases)
        except Exception:
            _remove_tree_no_follow(staging)
            if installed:
                _remove_tree_no_follow(destination_slot)
            raise

    report["source_inventory_sha256_after"] = source_inventory_after
    report["source_inventory_unchanged"] = True
    return report


def _load_contract(path: pathlib.Path) -> dict[str, Any]:
    return _read_json(path, "clone contract")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--contract", required=True, help="strict JSON contract binding both receipts")
    parser.add_argument("--clone", action="store_true",
                        help="perform an APFS CoW clone; default only validates and prints a plan")
    args = parser.parse_args(argv)
    try:
        contract_path = _canonical_absolute(args.contract, "contract file")
        contract = _load_contract(contract_path)
        report = clone_closed_graph(contract, execute=args.clone)
        print(json.dumps(report, sort_keys=True, indent=2))
        return 0
    except SafetyError as error:
        print(f"refusing graph reuse: {error}", file=sys.stderr)
        return 2
    except OSError as error:
        print(f"refusing graph reuse after filesystem error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
