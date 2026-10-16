from __future__ import annotations

import errno
import importlib.util
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile
import textwrap
import types
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).parents[1]
CENSUS_PATH = ROOT / ".config/scripts/build-census.py"
WRAPPER_PATH = ROOT / ".config/scripts/cargo-shared-cache.sh"
SPEC = importlib.util.spec_from_file_location("build_census_disk_reserve_test", CENSUS_PATH)
assert SPEC is not None and SPEC.loader is not None
census = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = census
SPEC.loader.exec_module(census)

GIB = 1024**3
RESERVE_PATH = pathlib.Path("/etc/nudox-ci-disk-reserve-gib")
CI_PATH = pathlib.Path("/srv/ci")


class DiskVolumeSnapshotTests(unittest.TestCase):
    def test_real_private_directory_snapshot_is_bound_to_open_directory(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-ci-disk-") as temporary:
            root = pathlib.Path(temporary).resolve()
            snapshot = census._disk_volume_snapshot(root)
            self.assertEqual(snapshot["device"], root.stat().st_dev)
            self.assertGreater(snapshot["total_bytes"], 0)
            self.assertGreaterEqual(snapshot["available_bytes"], 0)
            self.assertLessEqual(snapshot["available_bytes"], snapshot["total_bytes"])
            self.assertGreaterEqual(snapshot["used_bytes"], 0)
            self.assertLessEqual(snapshot["used_bytes"], snapshot["total_bytes"])

    def test_non_directory_symlink_and_missing_paths_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-ci-disk-invalid-") as temporary:
            root = pathlib.Path(temporary).resolve()
            regular = root / "regular"
            regular.write_text("not a directory")
            link = root / "link"
            link.symlink_to(root, target_is_directory=True)
            missing = root / "missing"
            for path in (regular, link):
                with self.subTest(path=path.name), self.assertRaises(ValueError):
                    census._disk_volume_snapshot(path)
            with self.assertRaises(FileNotFoundError):
                census._disk_volume_snapshot(missing)

    def test_open_permission_error_is_not_treated_as_absence(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-ci-disk-unreadable-") as temporary:
            root = pathlib.Path(temporary).resolve()
            real_open = census.os.open

            def deny_target(path: object, flags: int, *args: object, **kwargs: object) -> int:
                if pathlib.Path(path) == root:
                    raise PermissionError(errno.EACCES, "fixture denies directory open", str(root))
                return real_open(path, flags, *args, **kwargs)

            with mock.patch.object(census.os, "open", side_effect=deny_target):
                with self.assertRaises(PermissionError):
                    census._disk_volume_snapshot(root)

    def test_path_rebound_after_fstatvfs_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-ci-disk-rebound-") as temporary:
            root = pathlib.Path(temporary).resolve()
            target = root / "volume"
            moved = root / "volume-opened"
            target.mkdir()
            real_fstatvfs = census.os.fstatvfs
            changed = False

            def rebound_after_open(fd: int) -> object:
                nonlocal changed
                usage = real_fstatvfs(fd)
                if not changed:
                    target.rename(moved)
                    target.mkdir()
                    changed = True
                return usage

            with mock.patch.object(census.os, "fstatvfs", side_effect=rebound_after_open):
                with self.assertRaisesRegex(ValueError, "changed during observation"):
                    census._disk_volume_snapshot(target)


class HostResourceSelectionTests(unittest.TestCase):
    def host_snapshot(
        self,
        *,
        ci_state: str = "directory",
        root_device: int = 11,
        ci_device: int = 22,
        reserve_text: str | None = "2",
        reserve_error: OSError | None = None,
        root_available_bytes: int = 8 * GIB,
        ci_available_bytes: int = 3 * GIB,
    ) -> dict[str, object]:
        original_lstat = pathlib.Path.lstat
        original_resolve = pathlib.Path.resolve
        original_read_text = pathlib.Path.read_text

        ci_lstat_calls = 0

        def fake_lstat(path: pathlib.Path, *args: object, **kwargs: object) -> object:
            nonlocal ci_lstat_calls
            if path == CI_PATH:
                ci_lstat_calls += 1
                if ci_state == "missing" or (ci_state == "appears" and ci_lstat_calls == 1):
                    raise FileNotFoundError(errno.ENOENT, "fixture CI volume is absent", str(path))
                mode = stat.S_IFDIR | 0o700 if ci_state == "directory" else stat.S_IFREG | 0o600
                return types.SimpleNamespace(st_mode=mode)
            if path == RESERVE_PATH:
                if reserve_text is None and reserve_error is None:
                    raise FileNotFoundError(errno.ENOENT, "fixture reserve file is absent", str(path))
                return types.SimpleNamespace(st_mode=stat.S_IFREG | 0o600)
            return original_lstat(path, *args, **kwargs)

        def fake_resolve(path: pathlib.Path, *args: object, **kwargs: object) -> pathlib.Path:
            if path == CI_PATH and ci_state == "symlink":
                return pathlib.Path("/private/fixture/ci-volume")
            if path == CI_PATH:
                return CI_PATH
            return original_resolve(path, *args, **kwargs)

        def fake_read_text(path: pathlib.Path, *args: object, **kwargs: object) -> str:
            if path == pathlib.Path("/proc/meminfo"):
                return "MemAvailable: 8192 kB\n"
            if path == RESERVE_PATH:
                if reserve_error is not None:
                    raise reserve_error
                assert reserve_text is not None
                return reserve_text
            return original_read_text(path, *args, **kwargs)

        root_total_bytes = max(20 * GIB, root_available_bytes)
        ci_total_bytes = max(30 * GIB, ci_available_bytes)
        root_snapshot = dict(device=root_device, available_bytes=root_available_bytes,
                             total_bytes=root_total_bytes, used_bytes=root_total_bytes - root_available_bytes)
        ci_snapshot = dict(device=ci_device, available_bytes=ci_available_bytes,
                           total_bytes=ci_total_bytes, used_bytes=ci_total_bytes - ci_available_bytes)

        def disk_snapshot(path: pathlib.Path) -> dict[str, int]:
            if path == pathlib.Path("/"):
                return root_snapshot
            if path == CI_PATH:
                if ci_state == "file":
                    raise ValueError("CI path is not a directory")
                if ci_state == "unreadable":
                    raise PermissionError(errno.EACCES, "fixture CI volume cannot be opened")
                if ci_state == "rebound":
                    raise ValueError("CI path changed during observation")
                return ci_snapshot
            raise AssertionError(f"unexpected disk volume path: {path}")

        with (
            mock.patch.object(census.sys, "platform", "linux"),
            mock.patch.object(pathlib.Path, "lstat", new=fake_lstat),
            mock.patch.object(pathlib.Path, "resolve", new=fake_resolve),
            mock.patch.object(pathlib.Path, "read_text", new=fake_read_text),
            mock.patch.object(census, "_disk_volume_snapshot", side_effect=disk_snapshot) as observed,
        ):
            if ci_state in {"directory", "missing"}:
                result = census._host_resource_snapshot()
                self.assertEqual(observed.call_args_list[0].args[0], pathlib.Path("/"))
                if ci_state == "directory":
                    self.assertEqual(observed.call_args_list[1].args[0], CI_PATH)
                else:
                    self.assertEqual(observed.call_count, 1)
                return result
            with self.assertRaises(census.CensusError):
                census._host_resource_snapshot()
            self.assertEqual(observed.call_args_list[0].args[0], pathlib.Path("/"))
            if ci_state == "appears":
                self.assertEqual(observed.call_count, 1, "appearing CI path must be rejected before sampling it")
            return {}

    def test_root_reserve_is_present_only_when_ci_shares_root_device(self) -> None:
        same = self.host_snapshot(root_device=11, ci_device=11)
        self.assertEqual(same["root_disk_reserved_bytes"], 2 * GIB)
        self.assertEqual(same["ci_disk_reserved_bytes"], 2 * GIB)
        self.assertEqual(same["ci_disk_path"], str(CI_PATH))
        self.assertEqual(same["ci_disk_device"], 11)
        self.assertEqual(same["ci_disk_available_bytes"], 3 * GIB)

        separate = self.host_snapshot(root_device=11, ci_device=22)
        self.assertEqual(separate["root_disk_reserved_bytes"], 0)
        self.assertEqual(separate["ci_disk_reserved_bytes"], 2 * GIB)
        self.assertEqual(separate["root_disk_available_bytes"], 8 * GIB)
        self.assertEqual(separate["ci_disk_device"], 22)

    def test_missing_ci_volume_uses_root_volume_for_both_observations(self) -> None:
        result = self.host_snapshot(ci_state="missing", root_device=31)
        self.assertEqual(result["ci_disk_path"], "/")
        self.assertEqual(result["ci_disk_device"], 31)
        self.assertEqual(result["ci_disk_available_bytes"], 8 * GIB)
        self.assertEqual(result["root_disk_reserved_bytes"], 2 * GIB)
        self.assertEqual(result["ci_disk_reserved_bytes"], 2 * GIB)

    def test_ci_volume_appearing_during_absence_check_is_rejected(self) -> None:
        self.host_snapshot(ci_state="appears")

    def test_distinct_284_gib_ci_volume_keeps_80_gib_reserve_off_74_gib_root(self) -> None:
        result = self.host_snapshot(reserve_text="80", root_device=11, ci_device=22,
                                    root_available_bytes=74 * GIB, ci_available_bytes=284 * GIB)
        self.assertEqual(result["root_disk_available_bytes"], 74 * GIB)
        self.assertEqual(result["root_disk_reserved_bytes"], 0)
        self.assertEqual(result["ci_disk_device"], 22)
        self.assertEqual(result["ci_disk_available_bytes"], 284 * GIB)
        self.assertEqual(result["ci_disk_reserved_bytes"], 80 * GIB)

    def test_root_reserve_survives_74_gib_root_and_284_gib_ci_samples(self) -> None:
        result = self.host_snapshot(reserve_text="80", root_device=11, ci_device=11,
                                    root_available_bytes=74 * GIB, ci_available_bytes=284 * GIB)
        self.assertEqual(result["root_disk_device"], 11)
        self.assertEqual(result["root_disk_available_bytes"], 74 * GIB)
        self.assertEqual(result["root_disk_reserved_bytes"], 80 * GIB)
        self.assertEqual(result["ci_disk_device"], 11)
        self.assertEqual(result["ci_disk_available_bytes"], 284 * GIB)
        self.assertEqual(result["ci_disk_reserved_bytes"], 80 * GIB)

    def test_invalid_ci_path_and_reserve_inputs_fail_closed(self) -> None:
        for state in ("symlink", "file", "unreadable", "rebound"):
            with self.subTest(ci_state=state):
                self.host_snapshot(ci_state=state)
        for reserve in ("0", "-2", "not-a-number"):
            with self.subTest(reserve=reserve), self.assertRaises(census.CensusError):
                self.host_snapshot(reserve_text=reserve)
        with self.assertRaises(census.CensusError):
            self.host_snapshot(reserve_error=PermissionError(errno.EACCES, "fixture reserve unreadable"))


class HostProcessRowsTests(unittest.TestCase):
    def test_linux_ps_selection_and_fallback_use_the_real_pipe_and_parser(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-ci-fake-ps-") as temporary:
            fake_ps = pathlib.Path(temporary) / "ps"
            fake_ps.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' '987654 123 123 1000 Mon Jan 01 00:00:00 2024 fixture-ps-row'\n"
            )
            fake_ps.chmod(0o700)
            real_popen = census.subprocess.Popen
            expected_args = [
                "-ww", "-e", "-o", "pid=", "-o", "ppid=", "-o", "pgid=", "-o", "euid=",
                "-o", "lstart=", "-o", "comm=",
            ]

            # Exercise the production selector, Popen, file-descriptor selector,
            # pipe draining, and row parser. Only the /bin/ps execution in the
            # fallback case is redirected to the private fixture executable.
            for which_result, expected_executable in ((str(fake_ps), str(fake_ps)), (None, "/bin/ps")):
                with self.subTest(which_result=which_result):
                    calls: list[tuple[list[str], dict[str, object]]] = []

                    def delegate_popen(command: list[str], *args: object, **kwargs: object) -> object:
                        selected_command = list(command)
                        calls.append((selected_command, dict(kwargs)))
                        actual_command = list(selected_command)
                        if actual_command[0] == "/bin/ps":
                            actual_command[0] = str(fake_ps)
                        return real_popen(actual_command, *args, **kwargs)

                    with (
                        mock.patch.object(census.sys, "platform", "linux"),
                        mock.patch.object(census.shutil, "which", return_value=which_result),
                        mock.patch.object(census.subprocess, "Popen", side_effect=delegate_popen),
                    ):
                        rows = census._host_process_rows()

                    self.assertEqual(len(calls), 1)
                    self.assertEqual(calls[0][0], [expected_executable, *expected_args])
                    self.assertEqual(calls[0][1]["env"], {"LC_ALL": "C"})
                    self.assertEqual(rows, [census._PsRow(
                        pid=987654,
                        ppid=123,
                        pgid=123,
                        effective_uid=1000,
                        started="Mon Jan 01 00:00:00 2024",
                        ucomm="fixture-ps-row",
                    )])


class RenderedCargoWrapperTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="nudox-ci-reserve-wrapper-")
        self.root = pathlib.Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.fixture = self.root / "volume"
        self.fixture.mkdir()
        self.worktree = self.root / "worktree"
        self.worktree.mkdir()
        (self.worktree / "Cargo.lock").write_text("fake lockfile\n")
        self.reserve_file = self.root / "reserve-gib"
        self.reserve_file.write_text("1\n")
        self.cargo_log = self.root / "cargo.log"
        self.site = self.root / "python-site"
        self.site.mkdir()
        self._write_tools()
        self._write_sitecustomize()
        self.wrapper = self.root / "cargo-wrapper"
        values = {
            "@cargo@": str(self.bin / "cargo"),
            "@git@": str(self.bin / "git"),
            "@sccache@": str(self.bin / "sccache"),
            "@rustc_cache_wrapper@": str(self.bin / "rustc-cache-wrapper"),
            "@python3@": sys.executable,
            "@rustc@": str(self.bin / "rustc"),
            "@wrapper_source@": str(WRAPPER_PATH),
            "@provenance@": str(ROOT / ".config/scripts/cargo-provenance.py"),
        }
        rendered = "#!/bin/sh\n" + WRAPPER_PATH.read_text()
        for key, value in values.items():
            rendered = rendered.replace(key, value)
        self.wrapper.write_text(rendered)
        self.wrapper.chmod(0o700)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _executable(self, name: str, body: str) -> None:
        path = self.bin / name
        path.write_text("#!/bin/sh\nset -eu\n" + body)
        path.chmod(0o700)

    def _write_tools(self) -> None:
        self._executable(
            "git",
            """if [ "${1:-}" = -C ]; then shift 2; fi
case "${1:-} ${2:-} ${3:-}" in
  'rev-parse --show-toplevel ') printf '%s\\n' "$NUDOX_TEST_WORKTREE" ;;
  'rev-parse HEAD ') printf '%s\\n' '0123456789abcdef0123456789abcdef01234567' ;;
  'status --porcelain=v1 -z') exit 0 ;;
  'diff --binary HEAD --') exit 0 ;;
  'ls-files --others --exclude-standard') exit 0 ;;
  'worktree list --porcelain') printf 'worktree %s\\n' "$NUDOX_TEST_WORKTREE" ;;
  *) exit 0 ;;
esac
""",
        )
        self._executable(
            "cargo",
            """if [ "${1:-}" = --version ]; then printf '%s\\n' 'cargo 1.97.1-test'; exit 0; fi
printf '%s\\n' "$*" >> "$NUDOX_TEST_CARGO_LOG"
exit 0
""",
        )
        self._executable(
            "rustc",
            """if [ "${1:-}" = --version ]; then printf '%s\\n' 'rustc 1.97.1-test'; exit 0; fi
printf '%s\\n' 'rustc 1.97.1-test'
exit 0
""",
        )
        self._executable("rustdoc", "printf '%s\\n' 'rustdoc 1.97.1-test'\nexit 0\n")
        self._executable("sccache", "exit 0\n")
        self._executable("rustc-cache-wrapper", "exec \"$@\"\n")

    def _write_sitecustomize(self) -> None:
        sitecustomize = r'''
import os
import pathlib
import stat
import sys
import types

scenario = os.environ.get("NUDOX_TEST_DISK_SCENARIO")
if scenario:
    fixture = pathlib.Path(os.environ["NUDOX_TEST_CI_VOLUME"])
    workspace = pathlib.Path(os.environ["NUDOX_TEST_WORKTREE"]).resolve()
    reserve_file = pathlib.Path(os.environ["NUDOX_CI_DISK_RESERVE_FILE"])
    real_lstat = pathlib.Path.lstat
    real_resolve = pathlib.Path.resolve
    real_read_text = pathlib.Path.read_text
    real_open = os.open
    real_fstat = os.fstat
    real_fstatvfs = os.fstatvfs
    fd_paths = {}
    ci_lstat_calls = 0

    def mapped(path):
        path = pathlib.Path(path)
        if path == pathlib.Path("/srv/ci"):
            return fixture
        if path == workspace:
            return workspace
        return path

    def fake_lstat(path, *args, **kwargs):
        global ci_lstat_calls
        path = pathlib.Path(path)
        if path == pathlib.Path("/srv/ci"):
            ci_lstat_calls += 1
            if scenario == "missing" or (scenario == "appears" and ci_lstat_calls == 1):
                raise FileNotFoundError(2, "fixture CI volume is absent", str(path))
        if path == reserve_file and scenario == "reserve-unreadable":
            return real_lstat(path, *args, **kwargs)
        result = real_lstat(mapped(path), *args, **kwargs)
        if path in (pathlib.Path("/srv/ci"), pathlib.Path("/"), workspace):
            device_env = {
                pathlib.Path("/srv/ci"): "NUDOX_TEST_CI_DEV",
                pathlib.Path("/"): "NUDOX_TEST_ROOT_DEV",
                workspace: "NUDOX_TEST_WORKSPACE_DEV",
            }[path]
            device = int(os.environ[device_env])
            return types.SimpleNamespace(st_mode=result.st_mode, st_dev=device, st_ino=result.st_ino, st_uid=result.st_uid, st_size=result.st_size)
        return result

    def fake_resolve(path, *args, **kwargs):
        path = pathlib.Path(path)
        if path == pathlib.Path("/srv/ci"):
            if scenario == "symlink":
                return real_resolve(mapped(path), *args, **kwargs)
            return path
        return real_resolve(path, *args, **kwargs)

    def fake_read_text(path, *args, **kwargs):
        path = pathlib.Path(path)
        if path == reserve_file and scenario == "reserve-unreadable":
            raise PermissionError(13, "fixture reserve file unreadable", str(path))
        return real_read_text(path, *args, **kwargs)

    def fake_open(path, flags, *args, **kwargs):
        logical = pathlib.Path(path)
        if logical == pathlib.Path("/srv/ci") and scenario == "unreadable":
            raise PermissionError(13, "fixture CI directory unreadable", str(logical))
        fd = real_open(mapped(logical), flags, *args, **kwargs)
        fd_paths[fd] = logical
        return fd

    def fake_fstat(fd):
        result = real_fstat(fd)
        path = fd_paths.get(fd)
        if path in (pathlib.Path("/srv/ci"), pathlib.Path("/"), workspace):
            device_env = {
                pathlib.Path("/srv/ci"): "NUDOX_TEST_CI_DEV",
                pathlib.Path("/"): "NUDOX_TEST_ROOT_DEV",
                workspace: "NUDOX_TEST_WORKSPACE_DEV",
            }[path]
            device = int(os.environ[device_env])
            return types.SimpleNamespace(st_dev=device, st_ino=result.st_ino)
        return result

    def fake_fstatvfs(fd):
        path = fd_paths.get(fd)
        if path in (pathlib.Path("/srv/ci"), pathlib.Path("/"), workspace):
            if path == pathlib.Path("/srv/ci") and scenario == "rebound":
                moved = fixture.with_name(fixture.name + "-opened")
                fixture.rename(moved)
                fixture.mkdir()
            available_env = {
                pathlib.Path("/srv/ci"): "NUDOX_TEST_CI_AVAILABLE_BYTES",
                pathlib.Path("/"): "NUDOX_TEST_ROOT_AVAILABLE_BYTES",
                workspace: "NUDOX_TEST_WORKSPACE_AVAILABLE_BYTES",
            }[path]
            available = int(os.environ[available_env])
            return types.SimpleNamespace(f_bavail=available, f_frsize=1)
        return real_fstatvfs(fd)

    pathlib.Path.lstat = fake_lstat
    pathlib.Path.resolve = fake_resolve
    pathlib.Path.read_text = fake_read_text
    os.open = fake_open
    os.fstat = fake_fstat
    os.fstatvfs = fake_fstatvfs
'''
        (self.site / "sitecustomize.py").write_text(textwrap.dedent(sitecustomize))

    def run_build(
        self,
        scenario: str,
        *,
        available_bytes: int = 2 * GIB,
        reserve: str = "1",
        ci_device: int = 22,
        root_device: int = 11,
        workspace_device: int = 33,
        root_available_bytes: int | None = None,
        workspace_available_bytes: int | None = None,
    ) -> subprocess.CompletedProcess[str]:
        self.reserve_file.write_text(reserve + "\n")
        (self.cargo_log).unlink(missing_ok=True)
        env = {
            **os.environ,
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "PYTHONPATH": str(self.site),
            "NUDOX_TEST_DISK_SCENARIO": scenario,
            "NUDOX_TEST_CI_VOLUME": str(self.fixture),
            "NUDOX_TEST_ROOT_DEV": str(root_device),
            "NUDOX_TEST_CI_DEV": str(ci_device),
            "NUDOX_TEST_WORKSPACE_DEV": str(workspace_device),
            "NUDOX_TEST_CI_AVAILABLE_BYTES": str(available_bytes),
            "NUDOX_TEST_ROOT_AVAILABLE_BYTES": str(root_available_bytes if root_available_bytes is not None else available_bytes),
            "NUDOX_TEST_WORKSPACE_AVAILABLE_BYTES": str(workspace_available_bytes if workspace_available_bytes is not None else available_bytes),
            "NUDOX_TEST_WORKTREE": str(self.worktree.resolve()),
            "NUDOX_TEST_CARGO_LOG": str(self.cargo_log),
            "NUDOX_CI_DISK_RESERVE_FILE": str(self.reserve_file),
            "NUDOX_BUILD_CACHE_ROOT": str(self.root / "cache"),
            "NUDOX_CARGO_BUILD_SLOTS": "1",
            "NUDOX_CARGO_WORKTREE_WAIT_MS": "0",
            "NUDOX_CARGO_SLOT_WAIT_MS": "0",
            "NUDOX_CARGO_CACHE_DAEMON": "disabled",
            "CARGO_BUILD_BUILD_DIR": str(self.root / "build"),
            "CARGO_TARGET_DIR": str(self.root / "target"),
            "CARGO_BUILD_JOBS": "1",
            "RUSTC_WRAPPER": "",
            "RUSTC_WORKSPACE_WRAPPER": "",
            "CI": "",
            "CI_HEAD_SHA": "",
        }
        return subprocess.run([str(self.wrapper), "build", "--locked"], cwd=self.worktree, env=env, text=True, capture_output=True, timeout=40)

    def test_distinct_ci_volume_is_checked_and_low_space_skips_fake_cargo(self) -> None:
        result = self.run_build("directory", available_bytes=512 * 1024**2, ci_device=22, root_device=11)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn(f"path=/srv/ci device=22 available_bytes={512 * 1024**2} reserved_bytes={GIB}", result.stderr)
        self.assertFalse(self.cargo_log.exists(), "Cargo ran despite low /srv/ci headroom")

    def test_distinct_ci_volume_with_headroom_passes_even_if_root_is_below_reserve(self) -> None:
        result = self.run_build("directory", available_bytes=284 * GIB, reserve="80",
                                ci_device=22, root_device=11, root_available_bytes=74 * GIB)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"path=/srv/ci device=22 available_bytes={284 * GIB} reserved_bytes={80 * GIB}", result.stderr)
        self.assertEqual(self.cargo_log.read_text().splitlines(), ["build --locked"])

    def test_same_ci_volume_with_headroom_runs_only_fake_cargo(self) -> None:
        result = self.run_build("directory", available_bytes=2 * GIB, ci_device=11, root_device=11)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.cargo_log.read_text().splitlines(), ["build --locked"])

    def test_missing_ci_volume_preserves_the_workspace_volume_fence(self) -> None:
        result = self.run_build("missing", available_bytes=512 * 1024**2,
                                workspace_available_bytes=512 * 1024**2, root_available_bytes=8 * GIB)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn(f"path={self.worktree.resolve()} device=33 available_bytes={512 * 1024**2}", result.stderr)
        self.assertFalse(self.cargo_log.exists())

    def test_missing_ci_volume_uses_workspace_headroom_instead_of_low_root_headroom(self) -> None:
        result = self.run_build("missing", available_bytes=512 * 1024**2, reserve="80",
                                root_available_bytes=74 * GIB, workspace_available_bytes=100 * GIB,
                                workspace_device=33)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"path={self.worktree.resolve()} device=33 available_bytes={100 * GIB} reserved_bytes={80 * GIB}", result.stderr)
        self.assertEqual(self.cargo_log.read_text().splitlines(), ["build --locked"])

    def test_ci_volume_appearing_during_absence_check_fails_closed(self) -> None:
        result = self.run_build("appears", available_bytes=2 * GIB)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("build deferred to preserve CI disk headroom", result.stderr)
        self.assertFalse(self.cargo_log.exists())

    def test_invalid_unreadable_symlink_nondirectory_and_rebound_ci_fail_closed(self) -> None:
        cases = (
            ("directory", "0", "reserve"),
            ("reserve-unreadable", "1", "reserve"),
            ("appears", "1", "ci"),
            ("symlink", "1", "ci"),
            ("file", "1", "ci"),
            ("unreadable", "1", "ci"),
            ("rebound", "1", "ci"),
        )
        for scenario, reserve, label in cases:
            with self.subTest(scenario=scenario):
                if scenario == "symlink":
                    target = self.root / "volume-target"
                    target.mkdir(exist_ok=True)
                    link = self.root / "volume-link"
                    link.unlink(missing_ok=True)
                    link.symlink_to(target, target_is_directory=True)
                    self.fixture.rmdir()
                    self.fixture = link
                elif scenario == "file":
                    self.fixture.unlink(missing_ok=True)
                    self.fixture.write_text("not a volume")
                else:
                    if self.fixture.is_symlink() or self.fixture.is_file():
                        self.fixture.unlink()
                    if not self.fixture.exists():
                        self.fixture.mkdir()
                result = self.run_build(scenario, available_bytes=2 * GIB, reserve=reserve)
                self.assertEqual(result.returncode, 75, result.stderr)
                self.assertIn("build deferred to preserve CI disk headroom", result.stderr)
                self.assertFalse(self.cargo_log.exists(), f"Cargo ran for invalid {label} fixture")


if __name__ == "__main__":
    unittest.main()
