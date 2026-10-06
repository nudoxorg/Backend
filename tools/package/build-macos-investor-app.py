#!/usr/bin/env python3
"""Build the three macOS app executables through an operator-pinned Cargo runner."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import platform
import re
import shlex
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path


TARGETS = {
    "aarch64-apple-darwin": "arm64",
    "x86_64-apple-darwin": "x86_64",
}
EXECUTABLES = ("backend-desktop", "backend-mcp", "backend-locald", "backend-cli")
RUNNER_TOOL_VARIABLES = {"RUSTC", "RUSTDOC", "RUSTC_WRAPPER", "CARGO"}
DIRECT_ENVIRONMENT_VARIABLES = {
    "AR",
    "AS",
    "CC",
    "CARGO",
    "CARGO_BUILD_JOBS",
    "CARGO_HOME",
    "CARGO_INCREMENTAL",
    "CFLAGS",
    "CPPFLAGS",
    "CXX",
    "CXXFLAGS",
    "CMAKE_INCLUDE_PATH",
    "CMAKE_LIBRARY_PATH",
    "DEVELOPER_DIR",
    "DETERMINISTIC_BUILD",
    "LD",
    "LD_DYLD_PATH",
    "LIBCLANG_PATH",
    "LIBRARY_PATH",
    "LDFLAGS",
    "MACOSX_DEPLOYMENT_TARGET",
    "NM",
    "NIX_APPLE_SDK_VERSION",
    "NIX_BINTOOLS",
    "NIX_BINTOOLS_WRAPPER_TARGET_HOST_arm64_apple_darwin",
    "NIX_BUILD_CORES",
    "NIX_CFLAGS_COMPILE",
    "NIX_CFLAGS_COMPILE_BEFORE",
    "NIX_CFLAGS_LINK",
    "NIX_CXXFLAGS_COMPILE",
    "NIX_CXXFLAGS_LINK",
    "NIX_CXXSTDLIB_COMPILE",
    "NIX_CXXSTDLIB_LINK",
    "NIX_CC",
    "NIX_CC_WRAPPER_TARGET_HOST_arm64_apple_darwin",
    "NIX_DONT_SET_RPATH",
    "NIX_DONT_SET_RPATH_FOR_BUILD",
    "NIX_ENFORCE_NO_NATIVE",
    "NIX_HARDENING_ENABLE",
    "NIX_IGNORE_LD_THROUGH_GCC",
    "NIX_LDFLAGS",
    "NIX_LDFLAGS_BEFORE",
    "NIX_NO_SELF_RPATH",
    "NIX_PKG_CONFIG_WRAPPER_TARGET_HOST_arm64_apple_darwin",
    "NIX_STORE",
    "NIXPKGS_CMAKE_PREFIX_PATH",
    "NIX_SSL_CERT_FILE",
    "OBJCOPY",
    "OBJDUMP",
    "PATH",
    "PKG_CONFIG",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_SYSROOT_DIR",
    "PYTHONPATH",
    "RANLIB",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTDOC",
    "RUSTUP_HOME",
    "SDKROOT",
    "SIZE",
    "SSL_CERT_FILE",
    "SOURCE_DATE_EPOCH",
    "STRINGS",
    "STRIP",
    "ZERO_AR_DATE",
}


class BuildError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise BuildError(message)


def run(command: list[str], cwd: Path) -> str:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except OSError as error:
        fail(f"cannot run {command[0]}: {error}")
    if result.returncode:
        fail(f"command failed ({result.returncode}): {command!r}\n{result.stdout.strip()}")
    return result.stdout.strip()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def source_manifest(source: Path, revision: str, tree: str) -> dict[str, str]:
    actual_revision = run(["git", "rev-parse", "HEAD"], source)
    actual_tree = run(["git", "rev-parse", "HEAD^{tree}"], source)
    status = run(["git", "status", "--porcelain", "--untracked-files=all"], source)
    if actual_revision != revision or actual_tree != tree or status:
        fail(
            "source must remain at the exact operator-pinned clean commit; "
            f"expected {revision} ({tree}), got {actual_revision} ({actual_tree}); status={status!r}"
        )
    lock = source / "Cargo.lock"
    if not lock.is_file():
        fail("pinned source is missing Cargo.lock")
    return {
        "git_revision": actual_revision,
        "git_tree": actual_tree,
        "cargo_lock_sha256": sha256(lock),
        "working_tree": "clean",
    }


def _static_environment(path: Path) -> dict[str, str]:
    """Read a direct-runner snapshot containing literal exports only."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as error:
        fail(f"cannot read direct Cargo environment snapshot {path}: {error}")

    environment: dict[str, str] = {}
    for line_number, line in enumerate(lines, start=1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if not stripped.startswith("export "):
            fail(
                "direct Cargo environment snapshots may contain only literal export assignments; "
                f"unsupported content at {path}:{line_number}"
            )
        assignment = stripped[len("export ") :]
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)=(.*)", assignment)
        if match is None:
            fail(f"invalid direct Cargo environment assignment at {path}:{line_number}")
        name, raw_value = match.groups()
        if name not in DIRECT_ENVIRONMENT_VARIABLES:
            fail(f"direct Cargo environment snapshot contains unsupported variable {name}")
        if name in environment:
            fail(f"direct Cargo environment snapshot repeats variable {name}")
        try:
            words = shlex.split(raw_value, comments=False, posix=True)
        except ValueError as error:
            fail(f"invalid shell quoting in direct Cargo environment at {path}:{line_number}: {error}")
        if len(words) != 1 or raw_value != shlex.quote(words[0]):
            fail(
                "direct Cargo environment values must use one canonical, non-expanding shell literal; "
                f"unsupported assignment at {path}:{line_number}"
            )
        if re.search(r"(?:TOKEN|SECRET|PASSWORD|CREDENTIAL|PRIVATE_KEY|AUTH)", name, re.IGNORECASE):
            fail(f"direct Cargo environment snapshot must not contain credential variable {name}")
        environment[name] = words[0]
    if not environment:
        fail(f"direct Cargo environment snapshot is empty: {path}")
    return environment


def inspect_runner(runner_argument: Path, expected_sha256: str) -> dict[str, object]:
    if not runner_argument.is_absolute():
        fail("--cargo-runner must be an absolute path to the Root-supplied pinned runner")
    try:
        runner = runner_argument.resolve(strict=True)
    except OSError as error:
        fail(f"cannot resolve the pinned Cargo runner: {error}")
    if not runner.is_file():
        fail(f"pinned Cargo runner is not a regular file: {runner}")
    actual_runner_sha256 = sha256(runner)
    if actual_runner_sha256 != expected_sha256:
        fail("pinned Cargo runner content differs from --expected-runner-sha256")

    assets: dict[str, str] = {"runner": actual_runner_sha256}

    def capture(path_text: str, role: str, *, executable: bool) -> Path:
        candidate = Path(path_text)
        if not candidate.is_absolute():
            fail(f"pinned runner reference {role} must use an absolute path")
        try:
            path = candidate.resolve(strict=True)
        except OSError as error:
            fail(f"pinned runner reference {role} is unavailable: {error}")
        if not path.is_file() or (executable and not os.access(path, os.X_OK)):
            fail(f"pinned runner reference {role} is not a usable file: {path}")
        assets[role] = sha256(path)
        return path

    try:
        with runner.open("rb") as stream:
            header = stream.read(2)
    except OSError as error:
        fail(f"cannot inspect pinned Cargo runner: {error}")
    if header != b"#!":
        fail("pinned Cargo runner must be a shebang script with a directly named absolute interpreter")
    try:
        lines = runner.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as error:
        fail(f"cannot read pinned Cargo runner: {error}")
    try:
        interpreter_words = shlex.split(lines[0][2:].strip())
    except (IndexError, ValueError) as error:
        fail(f"invalid shebang in pinned Cargo runner: {error}")
    if not interpreter_words or not Path(interpreter_words[0]).is_absolute():
        fail("pinned Cargo runner must name an absolute shebang interpreter")
    interpreter = capture(interpreter_words[0], "runner.interpreter", executable=True)
    command_prefix = [str(interpreter), *interpreter_words[1:], str(runner)]
    normalized_prefix = ["$CARGO_RUNNER_INTERPRETER", *interpreter_words[1:], "$CARGO_RUNNER"]

    source_index = 0
    commands: list[list[str]] = []
    command_records: list[tuple[int, list[str]]] = []
    sourced_environments: list[dict[str, str]] = []
    literal_runner_assignments: list[tuple[int, str, str]] = []
    exec_words: list[str] | None = None
    for line_number, line in enumerate(lines[1:], start=2):
        try:
            words = shlex.split(line, comments=True)
        except ValueError as error:
            fail(f"cannot parse pinned Cargo runner line: {error}")
        if not words:
            continue
        commands.append(words)
        command_records.append((line_number, words))
        if words[0] in {"source", "."}:
            if len(words) != 2:
                fail("pinned Cargo runner uses a dynamic source expression")
            source_path = capture(words[1], f"runner.environment.{source_index}", executable=False)
            if Path(words[1]).is_absolute():
                try:
                    sourced_environments.append(_static_environment(source_path))
                except BuildError:
                    # Existing wrapper runners may source a controlled shell
                    # activation script. Direct mode is detected below and
                    # requires this static parser to succeed.
                    sourced_environments.append({})
            source_index += 1
            continue

        assignments = words[1:] if words[0] == "export" else words[:1]
        for assignment in assignments:
            if "=" not in assignment:
                continue
            name, value = assignment.split("=", 1)
            if name in RUNNER_TOOL_VARIABLES and value.startswith("/"):
                tool = capture(value, f"runner.tool.{name}", executable=True)
                assets[f"runner.tool.{name}"] = sha256(tool)

        if words[0] == "exec":
            if len(words) < 2 or not Path(words[1]).is_absolute():
                fail("pinned Cargo runner must exec an absolute Cargo path")
            capture(words[1], "runner.exec", executable=True)
            exec_words = words

    if "runner.interpreter" not in assets:
        fail("pinned Cargo runner must expose its absolute shebang interpreter")
    if not any(role.startswith("runner.environment.") for role in assets):
        fail("pinned Cargo runner must name its saved environment script directly")
    if exec_words is None:
        fail("pinned Cargo runner must end by execing its absolute Cargo path")

    execution_kind = "direct-cargo" if Path(exec_words[1]).name == "cargo" else "wrapper"
    if execution_kind == "direct-cargo":
        for line_number, words in command_records:
            if words[0] != "export":
                continue
            line = lines[line_number - 1]
            stripped = line.strip()
            assignment_text = stripped[len("export ") :]
            match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)=(.*)", assignment_text)
            if match is None:
                fail(f"direct Cargo runner exports must be literal assignments at {runner}:{line_number}")
            name, raw_value = match.groups()
            if name not in DIRECT_ENVIRONMENT_VARIABLES:
                fail(f"direct Cargo runner contains unsupported environment variable {name}")
            try:
                parsed = shlex.split(raw_value, comments=False, posix=True)
            except ValueError as error:
                fail(f"invalid literal export at {runner}:{line_number}: {error}")
            if len(parsed) != 1 or raw_value != shlex.quote(parsed[0]):
                fail(f"direct Cargo runner exports must use canonical literal values: {runner}:{line_number}")
            literal_runner_assignments.append((line_number, name, parsed[0]))
        for words in commands:
            if words[0] == "unset" and words[1:] != ["RUSTC_WRAPPER"]:
                fail("direct Cargo runner may unset only RUSTC_WRAPPER")
        assignment_events = {line_number: (name, value) for line_number, name, value in literal_runner_assignments}
        effective_environment: dict[str, str] = {}
        source_position = 0
        for line_number, words in command_records:
            if words[0] in {"source", "."}:
                effective_environment.update(sourced_environments[source_position])
                source_position += 1
            elif words[0] == "export":
                name, value = assignment_events[line_number]
                effective_environment[name] = value
            elif words[0] == "unset":
                for name in words[1:]:
                    effective_environment.pop(name, None)
    else:
        effective_environment = {}

    if execution_kind == "direct-cargo":
        if len(exec_words) != 3 or exec_words[2] != "$@":
            fail('direct Cargo runner must forward arguments exactly as: exec /absolute/path/cargo "$@"')
        if len(sourced_environments) != 1 or not sourced_environments[0]:
            fail("direct Cargo runner must source exactly one nonempty literal environment snapshot")
        unsupported_commands = [
            words
            for words in commands
            if words[0] not in {"set", "source", ".", "export", "unset", "exec"}
            or (words[0] == "set" and words[1:] not in [["-euo", "pipefail"], ["-eu"], ["-e"]])
            or (words[0] == "exec" and words is not commands[-1])
        ]
        if unsupported_commands:
            fail(f"direct Cargo runner contains unsupported shell behavior: {unsupported_commands[0]!r}")
        if not {"RUSTC", "RUSTDOC"}.issubset(effective_environment):
            fail("direct Cargo runner must pin explicit RUSTC and RUSTDOC paths")
        try:
            cargo_jobs = int(effective_environment["CARGO_BUILD_JOBS"])
        except (KeyError, ValueError):
            fail("direct Cargo runner must pin CARGO_BUILD_JOBS to an integer from 1 through 4")
        if not 1 <= cargo_jobs <= 4:
            fail("direct Cargo runner CARGO_BUILD_JOBS must be from 1 through 4")
        if "NIX_BUILD_CORES" in effective_environment:
            try:
                nix_build_cores = int(effective_environment["NIX_BUILD_CORES"])
            except ValueError:
                fail("direct Cargo runner NIX_BUILD_CORES must be an integer from 1 through 4")
            if not 1 <= nix_build_cores <= 4:
                fail("direct Cargo runner NIX_BUILD_CORES must be from 1 through 4")
        if "CARGO_INCREMENTAL" in effective_environment and effective_environment["CARGO_INCREMENTAL"] not in {"0", "1", "false", "true"}:
            fail("direct Cargo runner CARGO_INCREMENTAL must be a literal boolean")
        path_value = effective_environment.get("PATH", "")
        if not path_value or any(not Path(part).is_absolute() for part in path_value.split(":")):
            fail("direct Cargo runner PATH must be a nonempty colon-separated list of absolute directories")
        tool_paths = {
            "cargo": Path(exec_words[1]),
            "rustc": Path(effective_environment["RUSTC"]),
            "rustdoc": Path(effective_environment["RUSTDOC"]),
        }
        if "RUSTC_WRAPPER" in effective_environment:
            tool_paths["rustc_wrapper"] = Path(effective_environment["RUSTC_WRAPPER"])
        for role, path in tool_paths.items():
            if not path.is_absolute():
                fail(f"direct Cargo {role} path must be absolute")
            captured = capture(str(path), f"runner.tool.{role.upper()}", executable=True)
            if role == "cargo" and assets["runner.exec"] != assets[f"runner.tool.{role.upper()}"]:
                fail("direct Cargo executable identity differs between exec and tool manifests")
        environment_values = effective_environment
        environment_sha256 = hashlib.sha256(
            json.dumps(environment_values, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        assets["runner.environment.snapshot"] = hashlib.sha256(
            json.dumps(environment_values, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        if "RUSTC_WRAPPER" in effective_environment:
            assets["runner.tool.RUSTC_WRAPPER"] = sha256(tool_paths["rustc_wrapper"])
        normalized_prefix = ["$CARGO_RUNNER_INTERPRETER", *interpreter_words[1:], "$CARGO_RUNNER"]
        return {
            "sha256": actual_runner_sha256,
            "expected_sha256": expected_sha256,
            "execution_kind": execution_kind,
            "referenced_asset_sha256": dict(sorted(assets.items())),
            "environment_sha256": environment_sha256,
            "environment_variables": sorted(environment_values),
            "_environment": environment_values,
            "_tool_paths": {name: str(path) for name, path in tool_paths.items()},
            "_command_prefix": command_prefix,
            "invocation_prefix": normalized_prefix,
        }

    if not {"runner.tool.RUSTC", "runner.tool.RUSTC_WRAPPER", "runner.exec"}.issubset(assets):
        fail("pinned wrapper runner must expose its compiler, Rust wrapper, and Cargo wrapper paths")
    return {
        "sha256": actual_runner_sha256,
        "expected_sha256": expected_sha256,
        "execution_kind": execution_kind,
        "referenced_asset_sha256": dict(sorted(assets.items())),
        "_command_prefix": command_prefix,
        "invocation_prefix": normalized_prefix,
    }


def cargo_provenance(
    provenance_dir: Path,
    previous_records: set[str],
    source_root: Path,
    source: dict[str, str],
    target: str,
    target_dir: Path,
) -> dict[str, object]:
    records = {path.name: path for path in provenance_dir.glob("*.json")}
    new_names = set(records) - previous_records
    if len(new_names) != 1:
        fail(f"expected one new pinned Cargo provenance record, found {len(new_names)}")
    path = records[new_names.pop()]
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read the Cargo wrapper provenance record: {error}")
    if not isinstance(value, dict):
        fail("Cargo wrapper provenance record must be a JSON object")
    toolchain = value.get("toolchain")
    wrapper = value.get("wrapper")
    features = value.get("features")
    if (
        value.get("schema") != 2
        or value.get("workspace_root") != str(source_root)
        or value.get("cargo_target_dir") != str(target_dir)
        or value.get("git_head") != source["git_revision"]
        or value.get("cargo_lock_sha256") != source["cargo_lock_sha256"]
        or value.get("cargo_lock_sha256_after") != source["cargo_lock_sha256"]
        or value.get("source_changed_during_build") is not False
        or value.get("source_dirty_sha256") != value.get("source_dirty_sha256_after")
        or value.get("cargo_exit_status") != 0
        or not isinstance(toolchain, dict)
        or toolchain.get("capture_complete") is not True
        or toolchain.get("changed_during_build") is not False
        or not isinstance(wrapper, dict)
        or not isinstance(features, dict)
        or features.get("targets") != [target]
    ):
        fail("Cargo provenance does not prove an unchanged, successful build from the pinned source/toolchain")
    if not isinstance(value.get("source_dirty_sha256"), str) or not re.fullmatch(
        r"[0-9a-f]{64}", value["source_dirty_sha256"]
    ):
        fail("Cargo provenance has no valid clean-source digest")
    if not isinstance(value.get("run_id"), str) or not value["run_id"]:
        fail("Cargo provenance is missing the unique invocation identity")
    versions = {name: toolchain.get(name) for name in ("cargo", "rustc", "rustdoc")}
    if any(not isinstance(version, str) or not version.strip() for version in versions.values()):
        fail("Cargo provenance is missing a pinned Cargo/Rust tool version")
    if features != {
        "features": [],
        "all_features": False,
        "no_default_features": False,
        "targets": [target],
    }:
        fail("Cargo provenance does not match the requested target-specific release build")
    wrapper_hashes = {
        "runtime": wrapper.get("runtime_sha256"),
        "source": wrapper.get("source_sha256"),
        "rustc": wrapper.get("rustc_sha256"),
    }
    if any(not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest) for digest in wrapper_hashes.values()):
        fail("Cargo provenance is missing a pinned wrapper hash")
    outputs = value.get("outputs")
    if not isinstance(outputs, list) or any(
        not isinstance(item, dict)
        or not isinstance(item.get("path"), str)
        or not isinstance(item.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is None
        for item in outputs
    ):
        fail("Cargo provenance has an invalid executable-output manifest")
    if len({item["path"] for item in outputs}) != len(outputs):
        fail("Cargo provenance output manifest contains duplicate paths")
    return {
        "schema": 2,
        "record_sha256": sha256(path),
        "run_id": value.get("run_id"),
        "git_head": value["git_head"],
        "source_dirty_sha256": value["source_dirty_sha256"],
        "source_unchanged": True,
        "cargo_lock_sha256": value["cargo_lock_sha256"],
        "cargo_exit_status": 0,
        "features": features,
        "toolchain": versions,
        "toolchain_capture_complete": True,
        "toolchain_unchanged": True,
        "wrapper_sha256": wrapper_hashes,
        "outputs": outputs,
    }


def _direct_build_environment(runner: dict[str, object], source: Path, target_dir: Path) -> dict[str, str]:
    home = os.environ.get("HOME")
    if not home or not Path(home).is_absolute():
        fail("direct Cargo build requires an absolute operating-system HOME")
    temporary = os.environ.get("TMPDIR") or tempfile.gettempdir()
    if not Path(temporary).is_absolute():
        fail("direct Cargo build requires an absolute TMPDIR")
    environment = dict(runner["_environment"])
    environment["HOME"] = home
    environment["TMPDIR"] = temporary
    environment.setdefault("CARGO_HOME", str(Path(home) / ".cargo"))
    if not Path(environment["CARGO_HOME"]).is_absolute():
        fail("direct Cargo CARGO_HOME must be absolute")
    environment["PWD"] = str(source)
    environment["CARGO_TARGET_DIR"] = str(target_dir)
    environment["RUSTC"] = runner["_tool_paths"]["rustc"]
    environment["RUSTDOC"] = runner["_tool_paths"]["rustdoc"]
    environment["PATH"] = runner["_environment"]["PATH"]
    wrapper = runner["_tool_paths"].get("rustc_wrapper")
    if wrapper is None:
        environment.pop("RUSTC_WRAPPER", None)
    else:
        environment["RUSTC_WRAPPER"] = wrapper
    return environment


def _direct_tool_snapshot(runner: dict[str, object], source: Path, environment: dict[str, str]) -> dict[str, dict[str, str]]:
    result: dict[str, dict[str, str]] = {}
    for name, path_text in runner["_tool_paths"].items():
        path = Path(path_text)
        try:
            completed = subprocess.run(
                [str(path), "--version"],
                cwd=source,
                env=environment,
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )
        except OSError as error:
            fail(f"cannot inspect direct Cargo tool {name}: {error}")
        version = completed.stdout.strip()
        if completed.returncode or not version:
            fail(f"direct Cargo tool {name} did not return a version: {version!r}")
        result[name] = {"version": version, "sha256": sha256(path)}
    return result


def _parse_direct_cargo_line(line: bytes) -> tuple[str | None, tuple[str, str] | None]:
    """Return user-facing text and an executable artifact from one raw Cargo JSON line."""
    rendered_line = line.decode("utf-8", errors="replace")
    try:
        event = json.loads(rendered_line)
    except json.JSONDecodeError:
        return rendered_line, None
    if not isinstance(event, dict):
        return None, None
    reason = event.get("reason")
    if reason == "compiler-message":
        message = event.get("message")
        rendered = message.get("rendered") if isinstance(message, dict) else None
        return (rendered if isinstance(rendered, str) and rendered else None), None
    if reason == "compiler-artifact":
        target = event.get("target")
        executable = event.get("executable")
        if (
            isinstance(target, dict)
            and isinstance(target.get("name"), str)
            and isinstance(executable, str)
        ):
            return None, (target["name"], executable)
    return None, None


def _stop_owned_cargo_group(process: subprocess.Popen[bytes]) -> None:
    """Stop only the still-live process group created for this direct invocation."""
    try:
        if process.poll() is not None:
            return
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except OSError as error:
            sys.stderr.write(f"could not signal owned Cargo process group: {error}\n")
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            # Never signal the group after its Cargo leader has been reaped: the
            # numeric process-group ID could then belong to an unrelated process.
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except OSError as error:
                    sys.stderr.write(f"could not kill owned Cargo process group: {error}\n")
                    if process.poll() is None:
                        process.kill()
                process.wait()
    except BaseException as cleanup_error:
        # Cleanup diagnostics must not replace the parse/stream/cancellation error.
        sys.stderr.write(f"Cargo process cleanup was incomplete: {cleanup_error}\n")


def _stream_direct_cargo(
    command: list[str], cwd: Path, environment: dict[str, str], log_path: Path
) -> dict[str, object]:
    """Stream diagnostics while retaining Cargo's exact combined stdout/stderr bytes."""
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started_at = ""
    started_monotonic = 0
    finished_at = ""
    finished_monotonic = 0
    artifacts: dict[str, Path] = {}
    duplicate_artifacts: set[str] = set()
    process: subprocess.Popen[bytes] | None = None
    try:
        with log_path.open("xb") as log:
            started_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
            started_monotonic = time.monotonic_ns()
            try:
                process = subprocess.Popen(
                    command,
                    cwd=cwd,
                    env=environment,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    bufsize=0,
                    start_new_session=True,
                )
            except OSError as error:
                fail(f"cannot start direct Cargo; empty invocation log retained at {log_path}: {error}")
            if process.stdout is None:
                fail("direct Cargo output pipe was not created")
            print(
                f"direct Cargo started: pid={process.pid} started={started_at} log={log_path}",
                flush=True,
            )
            for raw_line in iter(process.stdout.readline, b""):
                log.write(raw_line)
                log.flush()
                rendered, artifact = _parse_direct_cargo_line(raw_line)
                if rendered:
                    sys.stdout.write(rendered)
                    if not rendered.endswith("\n"):
                        sys.stdout.write("\n")
                    sys.stdout.flush()
                if artifact is not None and artifact[0] in EXECUTABLES:
                    if artifact[0] in artifacts:
                        duplicate_artifacts.add(artifact[0])
                    else:
                        artifacts[artifact[0]] = Path(artifact[1])
            process.stdout.close()
            returncode = process.wait()
            finished_monotonic = time.monotonic_ns()
            finished_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
            log.flush()
            os.fsync(log.fileno())
            log_size = log.tell()
    except OSError as error:
        if process is not None and process.poll() is None:
            _stop_owned_cargo_group(process)
        fail(f"direct Cargo output logging failed; partial log retained at {log_path}: {error}")
    except BaseException:
        if process is not None:
            _stop_owned_cargo_group(process)
        raise
    if duplicate_artifacts:
        fail(f"Cargo emitted more than one executable artifact for {sorted(duplicate_artifacts)}")
    output_log = {
        "path": log_path.name,
        "sha256": sha256(log_path),
        "size_bytes": log_size,
    }
    print(
        f"direct Cargo finished: pid={process.pid} exit={returncode} finished={finished_at} "
        f"log_sha256={output_log['sha256']} log_bytes={log_size} log={log_path}",
        flush=True,
    )
    return {
        "returncode": returncode,
        "child_pid": process.pid,
        "artifacts": artifacts,
        "started_at_utc": started_at,
        "finished_at_utc": finished_at,
        "elapsed_ns": finished_monotonic - started_monotonic,
        "output_log": output_log,
    }


def _write_direct_cargo_provenance(
    provenance_dir: Path,
    source_root: Path,
    target_dir: Path,
    source: dict[str, str],
    runner: dict[str, object],
    target: str,
    command: list[str],
    runner_command: list[str],
    tools_before: dict[str, dict[str, str]],
    tools_after: dict[str, dict[str, str]],
    outputs: list[dict[str, object]],
    started_at: str,
    finished_at: str,
    elapsed_ns: int,
    effective_environment_sha256: str,
    effective_environment_variables: list[str],
    run_id: str,
    child_pid: int,
    output_log: dict[str, object],
) -> dict[str, object]:
    if tools_before != tools_after:
        fail("direct Cargo, rustc, rustdoc, or selected wrapper changed during the build")
    features = {
        "features": [],
        "all_features": False,
        "no_default_features": False,
        "targets": [target],
    }
    source_fingerprint = hashlib.sha256(
        json.dumps(source, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    runner_assets = runner["referenced_asset_sha256"]
    wrapper = tools_before.get("rustc_wrapper")
    record = {
        "schema": 3,
        "kind": "direct-cargo",
        "run_id": run_id,
        "workspace_root": str(source_root),
        "cargo_target_dir": str(target_dir),
        "git_head": source["git_revision"],
        "git_tree": source["git_tree"],
        "git_head_after": source["git_revision"],
        "git_tree_after": source["git_tree"],
        "cargo_lock_sha256": source["cargo_lock_sha256"],
        "cargo_lock_sha256_after": source["cargo_lock_sha256"],
        "source_fingerprint_sha256": source_fingerprint,
        "source_fingerprint_sha256_after": source_fingerprint,
        "source_unchanged": True,
        "cargo_exit_status": 0,
        "cargo_child_pid": child_pid,
        "cargo_output_log": output_log,
        "started_at_utc": started_at,
        "finished_at_utc": finished_at,
        "elapsed_ns": elapsed_ns,
        "target": target,
        "profile": "release",
        "locked": True,
        "features": features,
        "command": command,
        "command_sha256": hashlib.sha256(
            json.dumps(command, separators=(",", ":")).encode()
        ).hexdigest(),
        "runner_invocation": runner_command,
        "runner_sha256": runner["sha256"],
        "runner_asset_sha256": runner_assets,
        "environment_snapshot_sha256": runner["environment_sha256"],
        "environment_variables": runner["environment_variables"],
        "effective_environment_sha256": effective_environment_sha256,
        "effective_environment_variables": effective_environment_variables,
        "toolchain": tools_before,
        "toolchain_after": tools_after,
        "toolchain_unchanged": True,
        "rustc_wrapper_sha256": wrapper["sha256"] if wrapper else None,
        "runner_sha256_after": runner["sha256"],
        "runner_asset_sha256_after": runner_assets,
        "outputs": outputs,
    }
    provenance_dir.mkdir(parents=True, exist_ok=True)
    record_path = provenance_dir / f"direct-{run_id}.json"
    try:
        with record_path.open("x", encoding="utf-8") as stream:
            stream.write(json.dumps(record, indent=2, sort_keys=True) + "\n")
            stream.flush()
            os.fsync(stream.fileno())
    except OSError as error:
        fail(f"cannot write direct Cargo provenance record: {error}")
    tools = {
        name: {"version": value["version"], "sha256": value["sha256"]}
        for name, value in tools_before.items()
    }
    return {
        "schema": 3,
        "kind": "direct-cargo",
        "record_sha256": sha256(record_path),
        "run_id": run_id,
        "git_head": source["git_revision"],
        "git_tree": source["git_tree"],
        "git_head_after": source["git_revision"],
        "git_tree_after": source["git_tree"],
        "source_fingerprint_sha256": source_fingerprint,
        "source_fingerprint_sha256_after": source_fingerprint,
        "source_unchanged": True,
        "cargo_lock_sha256": source["cargo_lock_sha256"],
        "cargo_exit_status": 0,
        "cargo_child_pid": child_pid,
        "cargo_output_log": output_log,
        "features": features,
        "target": target,
        "profile": "release",
        "locked": True,
        "started_at_utc": started_at,
        "finished_at_utc": finished_at,
        "elapsed_ns": elapsed_ns,
        "command_sha256": record["command_sha256"],
        "command": [
            "$CARGO",
            "build",
            "--locked",
            "--manifest-path",
            "$SOURCE_ROOT/Cargo.toml",
            "--target-dir",
            "$SOURCE_ROOT/.local/target",
            "--target",
            target,
            "--profile",
            "release",
            "-p",
            "backend-desktop",
            "-p",
            "backend-mcp",
            "-p",
            "backend-locald",
            "-p",
            "backend-cli",
            "--message-format=json-render-diagnostics",
        ],
        "toolchain": tools,
        "toolchain_after": tools,
        "toolchain_unchanged": True,
        "runner_sha256": runner["sha256"],
        "runner_sha256_after": runner["sha256"],
        "runner_asset_sha256": runner_assets,
        "runner_asset_sha256_after": runner_assets,
        "environment_snapshot_sha256": runner["environment_sha256"],
        "environment_variables": runner["environment_variables"],
        "effective_environment_sha256": effective_environment_sha256,
        "effective_environment_variables": effective_environment_variables,
        "rustc_wrapper_sha256": wrapper["sha256"] if wrapper else None,
        "outputs": outputs,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path)
    parser.add_argument("--expected-revision", required=True, help="full operator-reviewed Git commit SHA")
    parser.add_argument("--expected-tree", required=True, help="full operator-reviewed Git tree SHA")
    parser.add_argument("--cargo-runner", required=True, type=Path, help="absolute path to the Root-supplied pinned Cargo runner")
    parser.add_argument("--expected-runner-sha256", required=True, help="full operator-reviewed Cargo runner SHA-256")
    parser.add_argument("--target", choices=TARGETS, default="aarch64-apple-darwin")
    parser.add_argument("--output-dir", required=True, type=Path, help="new directory for copied binaries and receipt")
    args = parser.parse_args()

    if re.fullmatch(r"[0-9a-f]{40}", args.expected_revision) is None:
        fail("--expected-revision must be a full lowercase Git commit SHA")
    if re.fullmatch(r"[0-9a-f]{40}", args.expected_tree) is None:
        fail("--expected-tree must be a full lowercase Git tree SHA")
    if re.fullmatch(r"[0-9a-f]{64}", args.expected_runner_sha256) is None:
        fail("--expected-runner-sha256 must be a full lowercase SHA-256 digest")
    if platform.system() != "Darwin":
        fail("application build requires a native macOS host")
    host_arch = platform.machine().lower()
    host_arch = "arm64" if host_arch == "aarch64" else host_arch
    if host_arch != TARGETS[args.target]:
        fail(f"native build host is {host_arch}; requested target is {TARGETS[args.target]}")

    source = args.source_root.resolve(strict=True)
    if args.output_dir.exists() or args.output_dir.is_symlink():
        fail(f"output directory already exists; refusing reuse: {args.output_dir}")
    output_dir = args.output_dir.resolve(strict=False)
    if output_dir.exists() or output_dir.is_symlink():
        fail(f"output directory already exists; refusing reuse: {output_dir}")
    try:
        output_dir.relative_to(source)
    except ValueError:
        pass
    else:
        fail("receipt output must be outside the pinned source checkout")

    source_before = source_manifest(source, args.expected_revision, args.expected_tree)
    runner = inspect_runner(args.cargo_runner, args.expected_runner_sha256)
    target_dir = source / ".local/target"
    provenance_dir = target_dir / ".nudox-provenance"
    previous_provenance = {
        path.name for path in provenance_dir.glob("*.json")
    } if provenance_dir.is_dir() else set()
    cargo_arguments = [
        "build",
        "--locked",
        "--manifest-path",
        str(source / "Cargo.toml"),
        "--target-dir",
        str(target_dir),
        "--target",
        args.target,
        "--profile",
        "release",
        "-p",
        "backend-desktop",
        "-p",
        "backend-mcp",
        "-p",
        "backend-locald",
        "-p",
        "backend-cli",
    ]
    direct_mode = runner["execution_kind"] == "direct-cargo"
    direct_run_id = uuid.uuid4().hex if direct_mode else None
    direct_log_path = (
        provenance_dir / f"direct-{direct_run_id}.cargo.log" if direct_mode else None
    )
    if direct_mode:
        cargo_arguments.extend(["--message-format=json-render-diagnostics"])
        build_env = _direct_build_environment(runner, source, target_dir)
        tools_before = _direct_tool_snapshot(runner, source, build_env)
    else:
        build_env = os.environ.copy()
        # The admitted Root runner forms its worktree-owned target path from $PWD.
        # subprocess(cwd=...) does not rewrite an inherited PWD value.
        build_env["PWD"] = str(source)
        tools_before = None
    command = [*runner["_command_prefix"], *cargo_arguments]
    direct_cargo_command = [runner["_tool_paths"]["cargo"], *cargo_arguments] if direct_mode else None
    if direct_mode:
        if direct_run_id is None or direct_log_path is None:
            fail("direct Cargo invocation identity was not initialized")
        direct_run = _stream_direct_cargo(command, source, build_env, direct_log_path)
        completed_returncode = direct_run["returncode"]
        direct_artifact_paths = direct_run["artifacts"]
        started_at = direct_run["started_at_utc"]
        finished_at = direct_run["finished_at_utc"]
        elapsed_ns = direct_run["elapsed_ns"]
        direct_output_log = direct_run["output_log"]
        direct_child_pid = direct_run["child_pid"]
        if (
            not isinstance(completed_returncode, int)
            or not isinstance(direct_artifact_paths, dict)
            or not isinstance(started_at, str)
            or not isinstance(finished_at, str)
            or not isinstance(elapsed_ns, int)
            or not isinstance(direct_output_log, dict)
            or not isinstance(direct_child_pid, int)
            or direct_child_pid <= 0
        ):
            fail("direct Cargo process receipt has invalid types")
        if completed_returncode:
            fail(
                f"direct Cargo build failed with status {completed_returncode}; "
                f"full raw output retained at {direct_log_path}"
            )
        if set(direct_artifact_paths) != set(EXECUTABLES):
            fail(
                "direct Cargo invocation did not emit exactly the required executable artifacts: "
                f"{sorted(direct_artifact_paths)}"
            )
        for name, artifact_path in direct_artifact_paths.items():
            expected_path = target_dir / args.target / "release" / name
            if artifact_path.resolve(strict=True) != expected_path.resolve(strict=True):
                fail(f"Cargo artifact event for {name} points outside the requested target/profile output")
    else:
        started_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
        started_monotonic = time.monotonic_ns()
        try:
            completed = subprocess.run(command, cwd=source, env=build_env, check=False)
        except OSError as error:
            fail(f"cannot start the operator-pinned Cargo runner: {error}")
        finished_monotonic = time.monotonic_ns()
        finished_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
        elapsed_ns = finished_monotonic - started_monotonic
        completed_returncode = completed.returncode
        direct_artifact_paths = {}
        direct_output_log = None
        direct_child_pid = None
        if completed_returncode:
            fail(f"operator-pinned Cargo build failed with status {completed_returncode}")

    source_after = source_manifest(source, args.expected_revision, args.expected_tree)
    if source_after != source_before:
        fail("source manifest changed during the build; refusing an application receipt")
    runner_after = inspect_runner(args.cargo_runner, args.expected_runner_sha256)
    if runner_after != runner:
        fail("pinned Cargo runner or one of its referenced environment/tool files changed during the build")
    effective_environment_variables = sorted(build_env) if direct_mode else []
    effective_environment_sha256 = (
        hashlib.sha256(json.dumps(build_env, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        if direct_mode
        else None
    )
    if direct_mode:
        for identity in (runner, runner_after):
            identity["effective_environment_sha256"] = effective_environment_sha256
            identity["effective_environment_variables"] = effective_environment_variables
    build_dir = target_dir / args.target / "release"
    binaries = {name: build_dir / name for name in EXECUTABLES}
    for name, path in binaries.items():
        if not path.is_file() or not os.access(path, os.X_OK):
            fail(f"build completed without the required executable {name}: {path}")
    if direct_mode:
        for name, path in binaries.items():
            if direct_artifact_paths[name].resolve(strict=True) != path.resolve(strict=True):
                fail(f"Cargo artifact event does not identify the current {name} executable")
        tools_after = _direct_tool_snapshot(runner_after, source, build_env)
        direct_outputs = [
            {
                "path": f"{args.target}/release/{name}",
                "sha256": sha256(path),
                "size_bytes": path.stat().st_size,
            }
            for name, path in binaries.items()
        ]
        build_provenance = _write_direct_cargo_provenance(
            provenance_dir,
            source,
            target_dir,
            source_before,
            runner,
            args.target,
            direct_cargo_command,
            command,
            tools_before,
            tools_after,
            direct_outputs,
            started_at,
            finished_at,
            elapsed_ns,
            effective_environment_sha256,
            effective_environment_variables,
            direct_run_id,
            direct_child_pid,
            direct_output_log,
        )
    else:
        build_provenance = cargo_provenance(
            provenance_dir,
            previous_provenance,
            source,
            source_before,
            args.target,
            target_dir,
        )
    cargo_outputs = {
        item["path"]: item["sha256"]
        for item in build_provenance["outputs"]
    }
    for name, path in binaries.items():
        output_name = f"{args.target}/release/{name}"
        if output_name not in cargo_outputs:
            fail(f"Cargo provenance is missing the required {name} output")
        if cargo_outputs[output_name] != sha256(path):
            fail(f"Cargo provenance output hash differs from the current {name} artifact")

    output_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="nudox-app-inputs-", dir=output_dir.parent) as temporary:
        staged = Path(temporary) / "inputs"
        artifacts = staged / "artifacts"
        artifacts.mkdir(parents=True)
        for name, binary in binaries.items():
            shutil.copy2(binary, artifacts / name)
        receipt = {
            "schema": 1,
            "source": source_before,
            "source_before": source_before,
            "source_after": source_after,
            "source_unchanged": True,
            "target": args.target,
            "profile": "release",
            "locked_build": True,
            "target_cache_policy": (
                "worktree-owned .local/target; direct Cargo reused its normal fingerprinted outputs"
                if direct_mode
                else "worktree-owned .local/target; the operator-pinned Cargo wrapper decides safe reuse"
            ),
            "cargo_runner_before": {
                key: value for key, value in runner.items() if not key.startswith("_")
            },
            "cargo_runner_after": {
                key: value for key, value in runner_after.items() if not key.startswith("_")
            },
            "cargo_runner_unchanged": True,
            "cargo_provenance": build_provenance,
            "command": [
                *runner["invocation_prefix"],
                "build",
                "--locked",
                "--manifest-path",
                "$SOURCE_ROOT/Cargo.toml",
                "--target-dir",
                "$SOURCE_ROOT/.local/target",
                "--target",
                args.target,
                "--profile",
                "release",
                "-p",
                "backend-desktop",
                "-p",
                "backend-mcp",
                "-p",
                "backend-locald",
                "-p",
                "backend-cli",
            ],
            "executables": {
                name: sha256(artifacts / name)
                for name in EXECUTABLES
            },
        }
        (staged / "application-build-receipt.json").write_text(
            json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        output_dir.mkdir()
        shutil.copytree(staged / "artifacts", output_dir / "artifacts")
        shutil.copy2(staged / "application-build-receipt.json", output_dir / "application-build-receipt.json")

    print(f"Created {output_dir / 'artifacts'}")
    print(f"Created {output_dir / 'application-build-receipt.json'}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BuildError as error:
        print(f"build refused: {error}", file=sys.stderr)
        sys.exit(2)
