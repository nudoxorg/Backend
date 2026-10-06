"""Shared validation for Root's real Cargo-build receipt and its adapter."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import shutil
import stat
import subprocess
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


RECEIPT_SCHEMA = "nudox.runtime-artifact-build-receipt.v1"
MANIFEST_SCHEMA = "nudox.runtime-build-manifest.v1"
MAX_RECEIPT_BYTES = 1024 * 1024
MAX_COMMAND_ITEMS = 256
MAX_COMMAND_ITEM_BYTES = 4096
EXECUTABLE_NAMES = ("backend-locald", "backend-cli", "backend-mcp")
SUPPORTED_INTERPRETER_NAMES = {"bash", "sh", "zsh"}
NIX_STORE_PATH_COMPONENT = re.compile(r"[0123456789abcdfghijklmnpqrsvwxyz]{32}-.+")


class ReceiptError(ValueError):
    """A sanitized refusal to admit a Cargo build receipt."""


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def read_regular(path: Path, maximum: int, label: str) -> bytes:
    if path.is_symlink():
        raise ReceiptError(f"{label} must not be a final symlink")
    try:
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise ReceiptError(f"{label} must be a single-link regular file")
        if before.st_size > maximum:
            raise ReceiptError(f"{label} exceeds its byte bound")
        with path.open("rb") as stream:
            opened = os.fstat(stream.fileno())
            payload = stream.read(maximum + 1)
            after_open = os.fstat(stream.fileno())
        after = path.lstat()
    except OSError as error:
        raise ReceiptError(f"{label} is unavailable") from error
    identity = lambda info: (
        info.st_dev,
        info.st_ino,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )
    if (
        len(payload) != before.st_size
        or len(payload) > maximum
        or identity(before) != identity(opened)
        or identity(opened) != identity(after_open)
        or identity(after_open) != identity(after)
        or not stat.S_ISREG(after.st_mode)
        or after.st_nlink != 1
    ):
        raise ReceiptError(f"{label} changed while it was read")
    return payload


def json_no_duplicate_keys(payload: bytes, label: str) -> Any:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate JSON key")
            result[key] = value
        return result

    try:
        return json.loads(payload, object_pairs_hook=pairs)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise ReceiptError(f"{label} is not duplicate-free JSON") from error


def canonical_json(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def normalize_arch(value: str) -> str:
    normalized = value.lower().replace("-", "_")
    return {
        "amd64": "x86_64",
        "x64": "x86_64",
        "i386": "x86",
        "i686": "x86",
        "arm64": "aarch64",
        "armv8l": "arm",
    }.get(normalized, normalized)


def architecture_from_binary_header(header: bytes, file_size: int, host_arch: str) -> str:
    if header.startswith(b"\x7fELF"):
        if len(header) < 20:
            raise ReceiptError("an ELF image header is truncated")
        encoding = header[5]
        byte_order = "little" if encoding == 1 else "big" if encoding == 2 else None
        if byte_order is None:
            raise ReceiptError("an ELF image has an unknown byte order")
        machine = int.from_bytes(header[18:20], byte_order)
        classes = {3: ("x86", 1), 40: ("arm", 1), 62: ("x86_64", 2), 183: ("aarch64", 2), 243: ("riscv64", 2)}
        item = classes.get(machine)
        if item is None or header[4] != item[1]:
            raise ReceiptError("an ELF image uses an unsupported or inconsistent architecture")
        return item[0]

    if len(header) < 8:
        raise ReceiptError("a binary image header is truncated")
    magic = header[:4]
    thin_magics = {
        b"\xce\xfa\xed\xfe": (4, "little"),
        b"\xcf\xfa\xed\xfe": (8, "little"),
        b"\xfe\xed\xfa\xce": (4, "big"),
        b"\xfe\xed\xfa\xcf": (8, "big"),
    }
    cpu_architectures = {
        7: "x86",
        0x01000007: "x86_64",
        12: "arm",
        0x0100000C: "aarch64",
    }
    if magic in thin_magics:
        header_width, byte_order = thin_magics[magic]
        if len(header) < header_width or file_size < header_width:
            raise ReceiptError("a thin Mach-O header is truncated")
        cpu = int.from_bytes(header[4:8], byte_order)
        architecture = cpu_architectures.get(cpu)
        if architecture is None:
            raise ReceiptError("a thin Mach-O uses an unsupported CPU type")
        return architecture

    fat_magics = {
        b"\xca\xfe\xba\xbe": ("big", False),
        b"\xbe\xba\xfe\xca": ("little", False),
        b"\xca\xfe\xba\xbf": ("big", True),
        b"\xbf\xba\xfe\xca": ("little", True),
    }
    if magic not in fat_magics:
        raise ReceiptError("an executable is not a supported ELF or Mach-O image")
    byte_order, is_fat64 = fat_magics[magic]
    count = int.from_bytes(header[4:8], byte_order)
    if not 0 < count <= 32:
        raise ReceiptError("a universal Mach-O has an invalid architecture table")
    entry_size = 32 if is_fat64 else 20
    table_end = 8 + count * entry_size
    if len(header) < table_end:
        raise ReceiptError("a universal Mach-O architecture table is truncated")
    found: set[str] = set()
    for index in range(count):
        offset = 8 + index * entry_size
        cpu = int.from_bytes(header[offset : offset + 4], byte_order)
        architecture = cpu_architectures.get(cpu)
        if architecture is None:
            raise ReceiptError("a universal Mach-O includes an unsupported CPU type")
        if is_fat64:
            slice_offset = int.from_bytes(header[offset + 8 : offset + 16], byte_order)
            slice_size = int.from_bytes(header[offset + 16 : offset + 24], byte_order)
        else:
            slice_offset = int.from_bytes(header[offset + 8 : offset + 12], byte_order)
            slice_size = int.from_bytes(header[offset + 12 : offset + 16], byte_order)
        if slice_size == 0 or slice_offset > file_size or slice_size > file_size - slice_offset:
            raise ReceiptError("a universal Mach-O has an invalid slice extent")
        found.add(architecture)
    normalized_host = normalize_arch(host_arch)
    if normalized_host not in found:
        raise ReceiptError("a universal Mach-O does not contain the host architecture")
    return normalized_host


def verify_architecture_parser_fixtures() -> None:
    thin = (
        (b"\xfe\xed\xfa\xce" + (7).to_bytes(4, "big") + bytes(20), 28, "x86"),
        (b"\xce\xfa\xed\xfe" + (12).to_bytes(4, "little") + bytes(20), 28, "arm"),
        (
            b"\xfe\xed\xfa\xcf" + (0x01000007).to_bytes(4, "big") + bytes(24),
            32,
            "x86_64",
        ),
        (
            b"\xcf\xfa\xed\xfe" + (0x0100000C).to_bytes(4, "little") + bytes(24),
            32,
            "aarch64",
        ),
    )
    for image, size, expected in thin:
        if architecture_from_binary_header(image, size, expected) != expected:
            raise ReceiptError("thin Mach-O parser self-check failed")

    def fat_image(magic: bytes, byte_order: str, cpus: tuple[int, ...]) -> bytes:
        is_fat64 = magic in {b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca"}
        entry_size = 32 if is_fat64 else 20
        slice_size = 4
        next_offset = 8 + len(cpus) * entry_size
        image = bytearray(magic + len(cpus).to_bytes(4, byte_order))
        offsets: list[int] = []
        for cpu in cpus:
            image.extend(cpu.to_bytes(4, byte_order))
            image.extend((0).to_bytes(4, byte_order))
            offsets.append(next_offset)
            if is_fat64:
                image.extend(next_offset.to_bytes(8, byte_order))
                image.extend(slice_size.to_bytes(8, byte_order))
                image.extend((0).to_bytes(4, byte_order))
                image.extend((0).to_bytes(4, byte_order))
            else:
                image.extend(next_offset.to_bytes(4, byte_order))
                image.extend(slice_size.to_bytes(4, byte_order))
                image.extend((0).to_bytes(4, byte_order))
            next_offset += slice_size
        for _ in offsets:
            image.extend(b"SLCE")
        return bytes(image)

    universal_cases = (
        (b"\xca\xfe\xba\xbe", "big", (7, 0x0100000C), "aarch64"),
        (b"\xbe\xba\xfe\xca", "little", (0x01000007, 12), "x86_64"),
        (b"\xca\xfe\xba\xbf", "big", (12, 0x01000007), "x86_64"),
        (b"\xbf\xba\xfe\xca", "little", (0x0100000C, 7), "aarch64"),
    )
    for magic, byte_order, cpus, host in universal_cases:
        image = fat_image(magic, byte_order, cpus)
        if architecture_from_binary_header(image, len(image), host) != host:
            raise ReceiptError("universal Mach-O host selection self-check failed")

    bad_fat_table = b"\xca\xfe\xba\xbe" + (2).to_bytes(4, "big") + bytes(20)
    bad_fat_extent = bytearray(fat_image(b"\xca\xfe\xba\xbe", "big", (7,)))
    bad_fat_extent[16:20] = (0xFFFFFFFF).to_bytes(4, "big")
    unknown_fat_cpu = bytearray(fat_image(b"\xca\xfe\xba\xbe", "big", (7,)))
    unknown_fat_cpu[8:12] = (0x70000001).to_bytes(4, "big")
    malformed = (
        (bad_fat_table, len(bad_fat_table), "x86_64"),
        (bytes(bad_fat_extent), len(bad_fat_extent), "x86"),
        (bytes(unknown_fat_cpu), len(unknown_fat_cpu), "x86"),
        (fat_image(b"\xca\xfe\xba\xbe", "big", (7,)), 32, "x86_64"),
    )
    for image, size, host in malformed:
        try:
            architecture_from_binary_header(image, size, host)
        except ReceiptError:
            continue
        raise ReceiptError("malformed Mach-O rejection self-check failed")


def stable_file(path: Path, label: str, *, executable: bool = False) -> dict[str, Any]:
    """Admit a stable single-link file, including receipts, source, runners, and artifacts."""
    return _stable_file(path, label, executable=executable, nix_executable_name=None)


def stable_tool_file(path: Path, label: str, *, executable: bool = True) -> dict[str, Any]:
    """Admit a tool, allowing only canonical immutable Nix hardlinks for Cargo/rustc."""
    if label.lower() not in {"cargo", "rustc"} or not executable:
        raise ReceiptError("the Nix hardlink exception is limited to executable Cargo and rustc")
    return _stable_file(path, label, executable=True, nix_executable_name=label.lower())


def stable_interpreter_file(path: Path) -> dict[str, Any]:
    """Hash an admitted shell; immutable Nix hardlinks retain the full ancestry proof."""
    if path.name not in SUPPORTED_INTERPRETER_NAMES:
        raise ReceiptError("runner interpreter must be an admitted shell")
    return _stable_file(
        path,
        "pinned runner interpreter",
        executable=True,
        nix_executable_name=path.name,
    )


def _stable_file(
    path: Path,
    label: str,
    *,
    executable: bool,
    nix_executable_name: str | None,
) -> dict[str, Any]:
    if not path.is_absolute() or path.is_symlink():
        raise ReceiptError(f"{label} must use an absolute non-symlink final path")
    try:
        resolved = path.resolve(strict=True)
        before = resolved.lstat()
    except OSError as error:
        raise ReceiptError(f"{label} is unavailable") from error
    if not stat.S_ISREG(before.st_mode):
        raise ReceiptError(f"{label} must be a regular file")
    nix_parent_identity = None
    if before.st_nlink < 1:
        raise ReceiptError(f"{label} must have a valid link count")
    if before.st_nlink != 1:
        if nix_executable_name is None:
            raise ReceiptError(f"{label} must be a single-link regular file")
        nix_parent_identity = _immutable_nix_tool_parent_identity(
            path, resolved, before, nix_executable_name
        )
        if nix_parent_identity is None:
            raise ReceiptError(
                f"{label} hardlinks are allowed only in a root-owned immutable Nix store path"
            )
    if executable and not os.access(resolved, os.X_OK):
        raise ReceiptError(f"{label} is not executable")
    digest = hashlib.sha256()
    try:
        with resolved.open("rb") as stream:
            opened = os.fstat(stream.fileno())
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
            after_open = os.fstat(stream.fileno())
        after = resolved.lstat()
        resolved_after = path.resolve(strict=True)
    except OSError as error:
        raise ReceiptError(f"{label} changed while being hashed") from error
    identity = lambda info: (
        info.st_dev,
        info.st_ino,
        info.st_mode,
        info.st_uid,
        info.st_gid,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
        info.st_nlink,
    )
    nix_parent_identity_after = None
    if nix_executable_name is not None and after.st_nlink > 1:
        nix_parent_identity_after = _immutable_nix_tool_parent_identity(
            path, resolved_after, after, nix_executable_name
        )
    if (
        identity(before) != identity(opened)
        or identity(opened) != identity(after_open)
        or identity(after_open) != identity(after)
        or resolved_after != resolved
        or not stat.S_ISREG(after.st_mode)
        or (after.st_nlink != 1 and nix_parent_identity_after is None)
        or (
            nix_parent_identity is not None
            and nix_parent_identity_after != nix_parent_identity
        )
    ):
        raise ReceiptError(f"{label} changed while being hashed")
    with resolved.open("rb") as stream:
        header = stream.read(4096)
    architecture = architecture_from_binary_header(
        header, before.st_size, normalize_arch(platform.machine())
    ) if executable else None
    return {
        "path": str(resolved),
        "sha256": digest.hexdigest(),
        "bytes": before.st_size,
        "architecture": architecture,
        "device": before.st_dev,
        "inode": before.st_ino,
    }


def _immutable_nix_tool_parent_identity(
    selected: Path,
    resolved: Path,
    file_info: os.stat_result,
    tool_name: str,
) -> tuple[tuple[int, ...], ...] | None:
    """Prove the immutable ancestry required before admitting a Nix tool hardlink."""
    if (
        tool_name not in {"cargo", "rustc"} | SUPPORTED_INTERPRETER_NAMES
        or selected != resolved
        or resolved.name != tool_name
        or len(resolved.parts) != 6
        or resolved.parts[:3] != ("/", "nix", "store")
        or resolved.parts[4] != "bin"
        or NIX_STORE_PATH_COMPONENT.fullmatch(resolved.parts[3]) is None
        or not stat.S_ISREG(file_info.st_mode)
        or file_info.st_uid != 0
        or stat.S_IMODE(file_info.st_mode) & 0o222
    ):
        return None

    directories = (
        Path("/"),
        Path("/nix"),
        Path("/nix/store"),
        resolved.parents[1],
        resolved.parent,
    )
    snapshots: list[tuple[int, ...]] = []
    for index, directory in enumerate(directories):
        try:
            info = directory.lstat()
        except OSError:
            return None
        mode = stat.S_IMODE(info.st_mode)
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0:
            return None
        if index == 2:
            # Nix uses a root-owned sticky store directory so users cannot replace
            # another owner's immutable store path, even when the group can add entries.
            if not (info.st_mode & stat.S_ISVTX) or mode & 0o002:
                return None
        elif index < 2:
            # The trusted root-owned system ancestors can be writable by root,
            # but never by group or other users.
            if mode & 0o022:
                return None
        elif mode & 0o222:
            return None
        snapshots.append((info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_gid))
    return tuple(snapshots)


def git_output(source: Path, arguments: list[str], label: str) -> bytes:
    git_name = shutil.which("git")
    if git_name is None:
        raise ReceiptError("Git is required to verify the build source")
    try:
        result = subprocess.run(
            [str(Path(git_name).resolve(strict=True)), "-C", str(source), *arguments],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ReceiptError(f"could not verify source {label}") from error
    if result.returncode != 0 or len(result.stdout) > 4 * 1024 * 1024:
        raise ReceiptError(f"could not verify source {label}")
    return result.stdout


def source_snapshot(source_input: Path) -> dict[str, Any]:
    if source_input.is_symlink():
        raise ReceiptError("source checkout must not be selected through a final symlink")
    try:
        source = source_input.resolve(strict=True)
    except OSError as error:
        raise ReceiptError("source checkout is unavailable") from error
    if not source.is_dir():
        raise ReceiptError("source checkout is not a directory")
    commit = git_output(source, ["rev-parse", "HEAD"], "commit").decode("ascii", "strict").strip()
    tree = git_output(source, ["rev-parse", "HEAD^{tree}"], "tree").decode("ascii", "strict").strip()
    status = git_output(
        source,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        "working-tree cleanliness",
    )
    if re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit) is None:
        raise ReceiptError("source checkout has an invalid commit identity")
    if re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", tree) is None:
        raise ReceiptError("source checkout has an invalid tree identity")
    lock = stable_file(source / "Cargo.lock", "Cargo.lock")
    return {
        "path": source,
        "commit": commit,
        "tree": tree,
        "clean": status == b"",
        "cargo_lock_sha256": lock["sha256"],
    }


def tool_version(path: Path, label: str) -> str:
    identity = stable_tool_file(path, label, executable=True)
    try:
        completed = subprocess.run(
            [identity["path"], "--version"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ReceiptError(f"could not verify {label} version") from error
    if completed.returncode != 0 or len(completed.stdout) > 16 * 1024:
        raise ReceiptError(f"could not verify {label} version")
    try:
        version = completed.stdout.decode("utf-8").strip()
    except UnicodeDecodeError as error:
        raise ReceiptError(f"{label} returned a non-text version") from error
    if not version:
        raise ReceiptError(f"{label} returned an empty version")
    identity_after = stable_tool_file(path, label, executable=True)
    if identity_after["sha256"] != identity["sha256"] or identity_after["path"] != identity["path"]:
        raise ReceiptError(f"{label} changed while its version was read")
    return version


def _parse_timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str):
        raise ReceiptError(f"build receipt {label} timestamp is missing")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ReceiptError(f"build receipt {label} timestamp is invalid") from error
    if parsed.tzinfo is None or parsed.utcoffset() != timezone.utc.utcoffset(parsed):
        raise ReceiptError(f"build receipt {label} timestamp is not UTC")
    return parsed


def execution_layout(command: Any, runner_path: str) -> tuple[list[str], str | None] | None:
    """Extract Cargo argv only from a direct runner or explicit shell-script prefix."""
    if (
        not isinstance(command, list)
        or not 2 <= len(command) <= MAX_COMMAND_ITEMS
        or any(
            not isinstance(item, str)
            or not item
            or "\x00" in item
            or len(item.encode("utf-8")) > MAX_COMMAND_ITEM_BYTES
            for item in command
        )
    ):
        return None
    expected_runner = Path(runner_path)
    try:
        if not expected_runner.is_absolute():
            return None
        expected_runner = expected_runner.resolve(strict=True)
        first = Path(command[0])
        if first.is_absolute() and first.resolve(strict=True) == expected_runner:
            return command[1:], None
        if len(command) < 3:
            return None
        interpreter = first
        runner_argument = Path(command[1])
        if (
            not interpreter.is_absolute()
            or interpreter.name not in SUPPORTED_INTERPRETER_NAMES
            or not runner_argument.is_absolute()
            or runner_argument.resolve(strict=True) != expected_runner
            or command[0] in {"-c", "-e"}
        ):
            return None
        return command[2:], str(interpreter.resolve(strict=True))
    except OSError:
        return None


def _is_build_command(command: Any, cargo_path: str, runner_path: str) -> bool:
    layout = execution_layout(command, runner_path)
    if layout is None:
        return False
    cargo_arguments, _ = layout
    if not Path(cargo_path).is_absolute() or not cargo_arguments:
        return False
    if cargo_arguments[0] != "build" or "--locked" not in cargo_arguments:
        return False
    package_names = set()
    for index, item in enumerate(cargo_arguments[:-1]):
        if item in {"-p", "--package"}:
            package_names.add(cargo_arguments[index + 1])
        elif item.startswith("--package="):
            package_names.add(item.split("=", 1)[1])
    if "--workspace" not in cargo_arguments and "--all" not in cargo_arguments:
        if not set(EXECUTABLE_NAMES).issubset(package_names):
            return False
    # `--workspace` does not by itself prove that the workspace's executable
    # targets were selected: Cargo also supports exclusions and target-only
    # selectors. A successful invocation followed by hashing pre-existing
    # artifacts would otherwise look like a fresh three-binary build.
    excluded_targets = {
        "--exclude",
        "--bin",
        "--lib",
        "--example",
        "--examples",
        "--test",
        "--tests",
        "--bench",
        "--benches",
        "--doc",
        "--docs",
    }
    if any(
        item in excluded_targets
        or any(item.startswith(f"{option}=") for option in excluded_targets)
        for item in cargo_arguments
    ):
        return False
    # The receipt's source identity is always the supplied checkout. Allowing
    # Cargo's manifest override would let a clean checkout receipt certify a
    # build launched from an unrelated workspace.
    if any(
        item == "--manifest-path"
        or item.startswith("--manifest-path=")
        for item in cargo_arguments[1:]
    ):
        return False
    return True


def verify_runtime_build_receipt(receipt_path: Path, source_input: Path) -> dict[str, Any]:
    receipt_bytes = read_regular(receipt_path, MAX_RECEIPT_BYTES, "Root Cargo build receipt")
    receipt = json_no_duplicate_keys(receipt_bytes, "Root Cargo build receipt")
    expected_keys = {
        "schema",
        "source",
        "command",
        "runner",
        "toolchain",
        "started_utc",
        "finished_utc",
        "exit",
        "check_only",
        "artifacts",
    }
    if not isinstance(receipt, dict) or set(receipt) != expected_keys:
        raise ReceiptError("Root Cargo build receipt has missing or unknown fields")
    if receipt["schema"] != RECEIPT_SCHEMA:
        raise ReceiptError("Root Cargo build receipt schema is unsupported")
    source_record = receipt["source"]
    if not isinstance(source_record, dict) or set(source_record) != {
        "commit",
        "tree",
        "clean_before",
        "clean_after",
        "cargo_lock_sha256",
    }:
        raise ReceiptError("Root Cargo build receipt source is malformed")
    if (
        source_record["clean_before"] is not True
        or source_record["clean_after"] is not True
        or re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", str(source_record["commit"])) is None
        or re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", str(source_record["tree"])) is None
        or re.fullmatch(r"[0-9a-f]{64}", str(source_record["cargo_lock_sha256"])) is None
    ):
        raise ReceiptError("Root Cargo build receipt does not certify a clean source and lockfile")
    source = source_snapshot(source_input)
    if not source["clean"]:
        raise ReceiptError("current source checkout is not clean")
    if any(
        source_record[name] != source[expected]
        for name, expected in (("commit", "commit"), ("tree", "tree"), ("cargo_lock_sha256", "cargo_lock_sha256"))
    ):
        raise ReceiptError("Root Cargo build receipt does not match the current exact source and Cargo.lock")
    if receipt["check_only"] is not False or type(receipt["exit"]) is not int or receipt["exit"] != 0:
        raise ReceiptError("Root Cargo build receipt is check-only, failed, or interrupted")
    started = _parse_timestamp(receipt["started_utc"], "start")
    finished = _parse_timestamp(receipt["finished_utc"], "finish")
    if finished < started:
        raise ReceiptError("Root Cargo build receipt finish precedes start")

    runner = receipt["runner"]
    if not isinstance(runner, dict) or set(runner) not in (
        {"path", "sha256"},
        {"path", "sha256", "interpreter"},
    ):
        raise ReceiptError("Root Cargo build receipt runner identity is malformed")
    runner_path = Path(runner["path"]) if isinstance(runner["path"], str) else Path()
    runner_identity = stable_file(runner_path, "pinned build runner")
    if not os.access(runner_identity["path"], os.X_OK):
        raise ReceiptError("pinned build runner is not executable")
    if runner_identity["path"] != runner["path"] or runner_identity["sha256"] != runner["sha256"]:
        raise ReceiptError("pinned build runner differs from the Root receipt")
    layout = execution_layout(receipt["command"], runner["path"])
    if layout is None:
        raise ReceiptError("Root Cargo command does not use the recorded runner with an admitted prefix")
    _, interpreter_path = layout
    recorded_interpreter = runner.get("interpreter")
    if interpreter_path is None:
        if recorded_interpreter is not None:
            raise ReceiptError("Root receipt records an unused runner interpreter")
    else:
        if not isinstance(recorded_interpreter, dict) or set(recorded_interpreter) != {
            "path",
            "sha256",
        }:
            raise ReceiptError("Root receipt omits its explicit runner interpreter identity")
        interpreter_identity = stable_interpreter_file(Path(interpreter_path))
        if (
            interpreter_identity["path"] != recorded_interpreter["path"]
            or interpreter_identity["sha256"] != recorded_interpreter["sha256"]
        ):
            raise ReceiptError("pinned runner interpreter differs from the Root receipt")

    toolchain = receipt["toolchain"]
    if not isinstance(toolchain, dict) or set(toolchain) != {"cargo", "rustc", "unchanged"} or toolchain["unchanged"] is not True:
        raise ReceiptError("Root Cargo build receipt toolchain capture is incomplete or changed")
    tool_identities: dict[str, dict[str, str]] = {}
    for name in ("cargo", "rustc"):
        item = toolchain[name]
        if not isinstance(item, dict) or set(item) != {"path", "version"}:
            raise ReceiptError(f"Root Cargo build receipt {name} identity is malformed")
        path = Path(item["path"]) if isinstance(item["path"], str) else Path()
        identity = stable_tool_file(path, name, executable=True)
        version = tool_version(path, name)
        if identity["path"] != item["path"] or version != item["version"]:
            raise ReceiptError(f"current {name} path or version differs from the Root receipt")
        tool_identities[name] = {"path": identity["path"], "version": version}
    cargo_path = tool_identities["cargo"]["path"]
    if not _is_build_command(receipt["command"], cargo_path, runner["path"]):
        raise ReceiptError("Root receipt does not record a locked Cargo build of all three executables")

    artifacts = receipt["artifacts"]
    if not isinstance(artifacts, dict) or set(artifacts) != set(EXECUTABLE_NAMES):
        raise ReceiptError("Root Cargo build receipt lacks an exact locald/CLI/MCP artifact set")
    host_arch = normalize_arch(platform.machine())
    verified_artifacts: dict[str, dict[str, Any]] = {}
    for name in EXECUTABLE_NAMES:
        item = artifacts[name]
        if not isinstance(item, dict) or set(item) != {"path", "sha256", "bytes", "architecture"}:
            raise ReceiptError(f"Root Cargo build receipt artifact {name} is malformed")
        artifact_path = Path(item["path"]) if isinstance(item["path"], str) else Path()
        identity = stable_file(artifact_path, name, executable=True)
        if (
            identity["path"] != item["path"]
            or identity["sha256"] != item["sha256"]
            or type(item["bytes"]) is not int
            or identity["bytes"] != item["bytes"]
            or identity["architecture"] != item["architecture"]
            or identity["architecture"] != host_arch
        ):
            raise ReceiptError(f"current artifact {name} differs from its exact Root receipt")
        verified_artifacts[name] = {
            "path": identity["path"],
            "sha256": identity["sha256"],
            "bytes": identity["bytes"],
            "architecture": identity["architecture"],
        }
    source_after = source_snapshot(source_input)
    if (
        not source_after["clean"]
        or source_after["commit"] != source["commit"]
        or source_after["tree"] != source["tree"]
        or source_after["cargo_lock_sha256"] != source["cargo_lock_sha256"]
    ):
        raise ReceiptError("source checkout changed while the Root receipt was being verified")
    receipt_after = read_regular(receipt_path, MAX_RECEIPT_BYTES, "Root Cargo build receipt")
    if sha256_bytes(receipt_after) != sha256_bytes(receipt_bytes):
        raise ReceiptError("Root Cargo build receipt changed while it was being verified")
    return {
        "receipt": receipt,
        "receipt_path": str(receipt_path.resolve(strict=True)),
        "receipt_sha256": sha256_bytes(receipt_bytes),
        "source": source,
        "artifacts": verified_artifacts,
    }
