from __future__ import annotations

import errno
import importlib.util
import pathlib
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = pathlib.Path(__file__).parents[1] / ".config/scripts/build-census.py"
SPEC = importlib.util.spec_from_file_location("build_census_proc_race", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
census = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = census
SPEC.loader.exec_module(census)


def stat_text(
    state: str,
    start: int,
    *,
    ppid: int = 11,
    pgid: int = 12,
    sid: int = 13,
    flags: int = 0,
) -> str:
    # Linux fields after comm: state is field 3 and starttime is field 22.
    fields = [state, str(ppid), str(pgid), str(sid), "0", "0", str(flags)] + ["0"] * 12 + [str(start)]
    return "41 (cargo (test)) " + " ".join(fields)


class FakeProc:
    def __init__(self, pid: int, *, starts: list[tuple[str, int] | tuple[str, int, int]] | None = None,
                 ready: list[bool | int | None] | None = None, failure: BaseException | None = None,
                 fail_path: str | None = None, flags: int = 0, status_uid: int = 1000) -> None:
        self.pid = pid
        self.pidfd = 73
        self.starts = list(starts or [("R", 101)] * 3)
        self.ready_values = list(ready or [False, False])
        self.failure = failure
        self.fail_path = fail_path
        self.flags = flags
        self.status_uid = status_uid
        self.events: list[str] = []

    def __enter__(self) -> "FakeProc":
        self.events.append("pidfd-open")
        self.events.append("open-pid-directory")
        return self

    def __exit__(self, _kind: object, _value: object, _traceback: object) -> None:
        self.events.append("close-pid-directory-and-pidfd")

    def read_stat(self) -> census._LinuxProcStat:
        self.events.append("stat")
        sample = self.starts.pop(0)
        state, start = sample[:2]
        flags = sample[2] if len(sample) == 3 else self.flags
        return census._parse_linux_proc_stat(stat_text(state, start, flags=flags))

    def pin_pidfd(self) -> None:
        pass

    def pidfd_ready(self) -> bool | None:
        self.events.append("pidfd-poll")
        flags = self.ready_values.pop(0)
        if flags is None:
            return None
        if isinstance(flags, bool):
            flags = census.select.POLLIN if flags else 0
        with patch.object(census.select, "poll", return_value=FakePoll(self.pidfd, flags)):
            return census._LinuxProcDirectory.pidfd_ready(self)

    def read_text(self, relative_path: str, _limit: int = 64 * 1024) -> str:
        self.events.append("read:" + relative_path)
        if self.failure is not None and self.fail_path == relative_path:
            raise self.failure
        if relative_path == "status":
            uid = self.status_uid
            return f"Name:\tcargo\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"
        raise AssertionError(relative_path)

    def readlink(self, relative_path: str) -> str:
        self.events.append("readlink:" + relative_path)
        if self.failure is not None and self.fail_path == relative_path:
            raise self.failure
        if relative_path == "exe":
            return "/usr/bin/cargo"
        raise AssertionError(relative_path)

    def read_bytes(self, relative_path: str, _limit: int) -> bytes:
        self.events.append("read-bytes:" + relative_path)
        if self.failure is not None and self.fail_path == relative_path:
            raise self.failure
        if relative_path == "cmdline":
            return b"/usr/bin/cargo\0build\0"
        raise AssertionError(relative_path)

    def stat_path(self, relative_path: str) -> object:
        self.events.append("stat:" + relative_path)
        if self.failure is not None and self.fail_path == relative_path:
            raise self.failure
        return type("Stat", (), {"st_dev": 8, "st_ino": 90 if relative_path == "ns/mnt" else 91})()


class FakePoll:
    def __init__(self, descriptor: int, flags: int) -> None:
        self.descriptor = descriptor
        self.flags = flags
        self.registration: tuple[int, int] | None = None

    def register(self, descriptor: int, events: int) -> None:
        self.registration = (descriptor, events)

    def poll(self, timeout: int) -> list[tuple[int, int]]:
        assert timeout == 0
        return [] if self.flags == 0 else [(self.descriptor, self.flags)]


class GoneAtPidfdOpen:
    def __init__(self, pid: int) -> None:
        self.pid = pid

    def __enter__(self) -> "GoneAtPidfdOpen":
        raise ProcessLookupError(errno.ESRCH, "no such process", f"/proc/{self.pid}")

    def __exit__(self, _kind: object, _value: object, _traceback: object) -> None:
        raise AssertionError("context exit is not called when __enter__ fails")

class LinuxProcRaceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.proc_root = pathlib.Path(tempfile.mkdtemp(prefix="linux-proc-race-test-"))
        boot_id = self.proc_root / "sys/kernel/random/boot_id"
        boot_id.parent.mkdir(parents=True)
        boot_id.write_text("test-boot-id\n", encoding="ascii")
        self.addCleanup(shutil.rmtree, self.proc_root)

    def factory(self, **kwargs: object):
        rows: list[FakeProc] = []

        def make(pid: int) -> FakeProc:
            row = FakeProc(pid, **kwargs)
            rows.append(row)
            return row

        return make, rows

    @staticmethod
    def cargo_row() -> census._PsRow:
        return census._PsRow(41, 1, 41, 1000, "Mon Oct 12 00:00:00 2026", "cargo")

    def test_gui_supervisor_capture_preserves_all_ownership_fields(self) -> None:
        factory, rows = self.factory()

        row, proof = census._capture_gui_supervisor_process(41, proc_factory=factory)

        self.assertIsNone(proof)
        self.assertEqual(row, {
            "pid": 41, "state": "R", "ppid": 11, "pgid": 12, "sid": 13,
            "start": 101, "uids": [1000, 1000, 1000, 1000],
            "ns": [8, 90], "root": [8, 91],
        })
        self.assertEqual(rows[0].events, [
            "pidfd-open", "open-pid-directory", "stat", "stat", "pidfd-poll",
            "read:status", "stat:ns/mnt", "stat:root", "stat", "pidfd-poll",
            "close-pid-directory-and-pidfd",
        ])

    def test_gui_namespace_enoent_is_accepted_only_with_exact_ready_pidfd(self) -> None:
        missing = FileNotFoundError(errno.ENOENT, "gone", "/proc/41/ns/mnt")
        factory, _rows = self.factory(
            starts=[("R", 101), ("R", 101), ("Z", 101)],
            ready=[False, True], failure=missing, fail_path="ns/mnt",
        )

        row, proof = census._capture_gui_supervisor_process(41, proc_factory=factory)

        self.assertIsNone(row)
        self.assertIsNotNone(proof)
        assert proof is not None
        self.assertEqual(proof.pid, 41)
        self.assertEqual(proof.first_start_ticks, "101")
        self.assertEqual(proof.last_start_ticks, "101")
        self.assertEqual(proof.first_state, "R")
        self.assertEqual(proof.last_state, "Z")
        self.assertTrue(proof.pidfd_ready)
        self.assertEqual(proof.missing_path, "/proc/41/ns/mnt")
        self.assertEqual(proof.missing_filename, "/proc/41/ns/mnt")

    def test_pidfd_poll_flags_distinguish_terminal_readiness_from_errors(self) -> None:
        cases = [
            (0, False),
            (census.select.POLLIN, True),
            (census.select.POLLHUP, True),
            (census.select.POLLIN | census.select.POLLHUP, True),
        ]
        for flags, expected in cases:
            with self.subTest(flags=flags):
                poller = FakePoll(73, flags)
                proc = census._LinuxProcDirectory(41)
                proc.pidfd = 73
                with patch.object(census.select, "poll", return_value=poller):
                    self.assertIs(proc.pidfd_ready(), expected)
                self.assertEqual(
                    poller.registration,
                    (73, census.select.POLLIN | census.select.POLLHUP | census.select.POLLERR),
                )
        for flags in (census.select.POLLERR, census.select.POLLNVAL):
            with self.subTest(flags=flags):
                poller = FakePoll(73, flags)
                proc = census._LinuxProcDirectory(41)
                proc.pidfd = 73
                with patch.object(census.select, "poll", return_value=poller):
                    with self.assertRaisesRegex(OSError, "pidfd-poll-error"):
                        proc.pidfd_ready()

    def test_pidfd_open_esrch_before_first_identity_is_incomplete_not_proof(self) -> None:
        with self.assertRaises(ProcessLookupError):
            census._capture_gui_supervisor_process(41, proc_factory=GoneAtPidfdOpen)

    def test_live_namespace_enoent_fails_closed_without_retries(self) -> None:
        factory, rows = self.factory(
            failure=FileNotFoundError(errno.ENOENT, "missing", "/proc/41/ns/mnt"),
            fail_path="ns/mnt",
        )

        with self.assertRaisesRegex(census._LinuxProcRace, "without-pidfd-exit-proof"):
            census._capture_gui_supervisor_process(41, proc_factory=factory)
        self.assertEqual(rows[0].events.count("stat:ns/mnt"), 1)

    def test_permission_failure_is_never_reclassified_as_exit(self) -> None:
        denied = PermissionError(errno.EACCES, "denied", "/proc/41/ns/mnt")
        factory, _rows = self.factory(
            ready=[False, True], failure=denied, fail_path="ns/mnt",
        )

        with self.assertRaises(PermissionError):
            census._capture_gui_supervisor_process(41, proc_factory=factory)

    def test_pid_reuse_between_first_and_pinned_stat_fails_closed(self) -> None:
        factory, _rows = self.factory(starts=[("R", 101), ("R", 102)])

        with self.assertRaisesRegex(census._LinuxProcRace, "start-changed-while-pinning"):
            census._capture_gui_supervisor_process(41, proc_factory=factory)

    def test_already_ready_pidfd_is_not_bound_to_a_later_first_proc_row(self) -> None:
        factory, _rows = self.factory(
            starts=[("R", 101), ("R", 101)],
            ready=[True],
        )

        with self.assertRaisesRegex(census._LinuxProcRace, "pidfd-ready-before-first-identity"):
            census._capture_gui_supervisor_process(41, proc_factory=factory)

    def test_stable_terminal_proc_row_is_distinct_from_pidfd_exit_proof(self) -> None:
        for state in ("Z", "X", "x"):
            with self.subTest(state=state):
                factory, rows = self.factory(
                    starts=[(state, 101), (state, 101)],
                    ready=[True],
                )

                row, proof = census._capture_gui_supervisor_process(41, proc_factory=factory)

                self.assertIsNone(proof)
                self.assertEqual(row, {
                    "pid": 41,
                    "state": state,
                    "ppid": 11,
                    "pgid": 12,
                    "sid": 13,
                    "start": 101,
                    "terminal_proc_observation": True,
                })
                self.assertEqual(rows[0].events, [
                    "pidfd-open", "open-pid-directory", "stat", "stat",
                    "close-pid-directory-and-pidfd",
                ])

    def test_terminal_cargo_row_does_not_become_unknown_active_occupancy(self) -> None:
        factory, _rows = self.factory(
            starts=[("Z", 101), ("Z", 101)],
            ready=[True],
        )

        records, issues, races = census._linux_processes(
            1000,
            [self.cargo_row()],
            census._ArgvBudget(),
            proc_root=self.proc_root,
            proc_factory=factory,
        )

        self.assertEqual(records, {})
        self.assertEqual(issues, {})
        self.assertEqual(races, {})

    def test_terminal_to_active_identity_is_refused_independent_of_pidfd_readiness(self) -> None:
        for ready in (False, True):
            with self.subTest(pidfd_ready=ready):
                factory, rows = self.factory(
                    starts=[("Z", 101), ("R", 101)],
                    ready=[ready],
                )

                with self.assertRaisesRegex(census._LinuxProcRace, "terminal-state-changed-while-pinning"):
                    census._capture_gui_supervisor_process(41, proc_factory=factory)
                # The contradictory proc observation is sufficient to refuse;
                # polling cannot turn a terminal task into a live identity.
                self.assertEqual(rows[0].events, [
                    "pidfd-open", "open-pid-directory", "stat", "stat",
                    "close-pid-directory-and-pidfd",
                ])

    def test_pid_reuse_after_payload_fails_closed(self) -> None:
        factory, _rows = self.factory(starts=[("R", 101), ("R", 101), ("R", 102)])

        with self.assertRaisesRegex(census._LinuxProcRace, "start-changed-during-proc-read"):
            census._capture_gui_supervisor_process(41, proc_factory=factory)

    def test_exit_after_full_gui_payload_is_a_proven_terminal_observation(self) -> None:
        factory, _rows = self.factory(
            starts=[("R", 101), ("R", 101), ("R", 101), ("Z", 101)],
            ready=[False, True],
        )

        row, proof = census._capture_gui_supervisor_process(41, proc_factory=factory)

        self.assertIsNone(row)
        self.assertIsNotNone(proof)
        assert proof is not None
        self.assertEqual((proof.first_start_ticks, proof.last_start_ticks), ("101", "101"))
        self.assertTrue(proof.pidfd_ready)
        self.assertIsNone(proof.missing_path)

    def test_hup_only_proves_exit_for_matching_terminal_gui_identity(self) -> None:
        factory, _rows = self.factory(
            starts=[("R", 101), ("R", 101), ("Z", 101)],
            ready=[0, census.select.POLLHUP],
            failure=FileNotFoundError(errno.ENOENT, "gone", "ns/mnt"),
            fail_path="ns/mnt",
        )

        row, proof = census._capture_gui_supervisor_process(41, proc_factory=factory)

        self.assertIsNone(row)
        self.assertIsNotNone(proof)
        assert proof is not None
        self.assertEqual((proof.first_start_ticks, proof.last_start_ticks), ("101", "101"))
        self.assertEqual(proof.last_state, "Z")
        self.assertTrue(proof.pidfd_ready)

    def test_pidfd_ready_but_active_replacement_is_not_silently_omitted(self) -> None:
        factory, _rows = self.factory(
            starts=[("R", 101), ("R", 101), ("R", 101), ("R", 102)],
            ready=[0, census.select.POLLHUP],
        )

        with self.assertRaisesRegex(census._LinuxProcRace, "pid-start-changed-after-pidfd-exit"):
            census._capture_gui_supervisor_process(41, proc_factory=factory)

    def test_missing_field_without_pidfd_support_is_not_called_an_exit(self) -> None:
        factory, _rows = self.factory(
            ready=[None, None],
            failure=FileNotFoundError(errno.ENOENT, "missing", "ns/mnt"),
            fail_path="ns/mnt",
        )

        with self.assertRaisesRegex(census._LinuxProcRace, "without-pidfd-exit-proof"):
            census._capture_gui_supervisor_process(41, proc_factory=factory)

    def test_census_retains_unproven_candidate_as_unknown_not_an_exit(self) -> None:
        missing = FileNotFoundError(errno.ENOENT, "missing", "exe")
        factory, _rows = self.factory(failure=missing, fail_path="exe")

        records, issues, races = census._linux_processes(
            1000, [self.cargo_row()], census._ArgvBudget(), proc_root=self.proc_root,
            proc_factory=factory
        )

        self.assertTrue(records[41].comm_candidate)
        self.assertFalse(records[41].identity_validated)
        self.assertEqual(issues, {})
        self.assertEqual(races, {})

    def test_census_marks_unproven_same_uid_non_candidate_snapshot_incomplete(self) -> None:
        missing = FileNotFoundError(errno.ENOENT, "missing", "exe")
        factory, _rows = self.factory(failure=missing, fail_path="exe")
        row = census._PsRow(41, 1, 41, 1000, "Mon Oct 12 00:00:00 2026", "python")

        records, issues, races = census._linux_processes(
            1000, [row], census._ArgvBudget(), proc_root=self.proc_root,
            proc_factory=factory
        )

        self.assertEqual(records, {})
        self.assertEqual(issues, {"same-uid-process-snapshot-incomplete": 1})
        self.assertEqual(races, {})

    def test_census_counts_only_pidfd_proven_exit_as_a_race(self) -> None:
        missing = FileNotFoundError(errno.ENOENT, "gone", "exe")
        factory, _rows = self.factory(
            starts=[("R", 101), ("R", 101), ("Z", 101)],
            ready=[0, census.select.POLLHUP], failure=missing, fail_path="exe",
        )

        records, issues, races = census._linux_processes(
            1000, [self.cargo_row()], census._ArgvBudget(), proc_root=self.proc_root,
            proc_factory=factory
        )

        self.assertEqual(records, {})
        self.assertEqual(issues, {})
        self.assertEqual(races, {"pid-exited-during-scan": 1})

    def test_stable_pf_kthread_missing_executable_is_skipped(self) -> None:
        kthread_flag = census._LINUX_PF_KTHREAD
        self.assertEqual(census._parse_linux_proc_stat(stat_text("S", 101, flags=kthread_flag)).flags, kthread_flag)
        missing = FileNotFoundError(errno.ENOENT, "no executable for kernel thread", "exe")
        factory, _rows = self.factory(
            flags=kthread_flag,
            status_uid=0,
            ready=[0, 0],
            failure=missing,
            fail_path="exe",
        )
        row = census._PsRow(41, 2, 0, 0, "Mon Oct 12 00:00:00 2026", "kworker/R-fixture")

        records, issues, races = census._linux_processes(
            0,
            [row],
            census._ArgvBudget(),
            proc_root=self.proc_root,
            proc_factory=factory,
        )

        self.assertEqual(records, {})
        self.assertEqual(issues, {})
        self.assertEqual(races, {})

    def test_kernel_thread_flag_must_match_before_and_after_missing_executable(self) -> None:
        kthread_flag = census._LINUX_PF_KTHREAD
        inconsistent_samples = [
            [("S", 101, 0), ("S", 101, kthread_flag), ("S", 101, kthread_flag)],
            [("S", 101, kthread_flag), ("S", 101, kthread_flag), ("S", 101, 0)],
        ]
        for starts in inconsistent_samples:
            with self.subTest(starts=starts):
                missing = FileNotFoundError(errno.ENOENT, "no executable for kernel thread", "exe")
                factory, _rows = self.factory(
                    starts=starts,
                    status_uid=0,
                    ready=[0, 0],
                    failure=missing,
                    fail_path="exe",
                )
                row = census._PsRow(41, 2, 0, 0, "Mon Oct 12 00:00:00 2026", "kworker/R-fixture")

                records, issues, races = census._linux_processes(
                    0,
                    [row],
                    census._ArgvBudget(),
                    proc_root=self.proc_root,
                    proc_factory=factory,
                )

                self.assertEqual(records, {})
                self.assertEqual(issues, {"same-uid-process-snapshot-incomplete": 1})
                self.assertEqual(races, {})

    def test_kernelish_name_and_parent_do_not_excuse_userspace_missing_executable(self) -> None:
        missing = FileNotFoundError(errno.ENOENT, "executable disappeared", "exe")
        factory, _rows = self.factory(
            flags=0,
            status_uid=0,
            ready=[0, 0],
            failure=missing,
            fail_path="exe",
        )
        row = census._PsRow(41, 2, 0, 0, "Mon Oct 12 00:00:00 2026", "kthreadd")

        records, issues, races = census._linux_processes(
            0,
            [row],
            census._ArgvBudget(),
            proc_root=self.proc_root,
            proc_factory=factory,
        )

        self.assertEqual(records, {})
        self.assertEqual(issues, {"same-uid-process-snapshot-incomplete": 1})
        self.assertEqual(races, {})

    def test_census_keeps_permission_denied_candidate_unknown(self) -> None:
        denied = PermissionError(errno.EACCES, "denied", "exe")
        factory, _rows = self.factory(failure=denied, fail_path="exe")

        records, issues, races = census._linux_processes(
            1000, [self.cargo_row()], census._ArgvBudget(), proc_root=self.proc_root,
            proc_factory=factory
        )

        self.assertTrue(records[41].comm_candidate)
        self.assertFalse(records[41].identity_validated)
        self.assertEqual(issues, {})
        self.assertEqual(races, {})


if __name__ == "__main__":
    unittest.main()
