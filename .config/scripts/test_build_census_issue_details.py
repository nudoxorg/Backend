"""Bounded diagnostics for fail-closed Linux process snapshot issues."""

import contextlib
import errno
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest


_spec = importlib.util.spec_from_file_location(
    "build_census_issue_details",
    Path(__file__).with_name("build-census.py"),
)
census = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = census
_spec.loader.exec_module(census)


class _MissingExeProc:
    def __init__(self, pid):
        self.pid = pid
        self.proc_path = Path("/proc") / str(pid)
        self.pidfd_open_error = None
        self.directory_open_error = None

    def __enter__(self):
        return self

    def __exit__(self, _kind, _value, _traceback):
        return None

    def pin_pidfd(self):
        return None

    def read_stat(self):
        return census._LinuxProcStat(
            state="S",
            ppid=1,
            pgid=self.pid,
            session=self.pid,
            flags=0,
            start_ticks="138598638",
        )

    def pidfd_ready(self):
        return False

    def read_text(self, relative_path, _limit=census.MAX_RESOURCE_OUTPUT_BYTES):
        if relative_path == "status":
            return "Name:\tworker\nUid:\t0\t0\t0\t0\n"
        raise AssertionError(f"unexpected proc read: {relative_path}")

    def readlink(self, relative_path):
        if relative_path != "exe":
            raise AssertionError(f"unexpected proc link: {relative_path}")
        raise FileNotFoundError(errno.ENOENT, "No such file or directory", relative_path)


class LinuxProcessIssueDetailTests(unittest.TestCase):
    def rows(self, count):
        return [
            census._PsRow(
                pid=4100 + index,
                ppid=1,
                pgid=4100 + index,
                effective_uid=0,
                started="Thu Oct 15 01:24:22 2026",
                ucomm="worker",
            )
            for index in range(count)
        ]

    def scan(self, count):
        with tempfile.TemporaryDirectory() as temporary:
            proc_root = Path(temporary)
            boot_id = proc_root / "sys/kernel/random/boot_id"
            boot_id.parent.mkdir(parents=True)
            boot_id.write_text("test-boot-id\n")
            details = []
            result = census._linux_processes(
                0,
                self.rows(count),
                census._ArgvBudget(),
                proc_root=proc_root,
                proc_factory=_MissingExeProc,
                issue_details=details,
            )
            return result, details

    def test_missing_executable_without_pidfd_exit_proof_stays_incomplete_and_is_identified(self):
        (records, issues, races), details = self.scan(1)

        self.assertEqual(records, {})
        self.assertEqual(issues, {"same-uid-process-snapshot-incomplete": 1})
        self.assertEqual(races, {})
        self.assertEqual(len(details), 1)
        self.assertEqual(
            details[0],
            {
                "category": "same-uid-process-snapshot-incomplete",
                "pid": 4100,
                "ps_started": "Thu Oct 15 01:24:22 2026",
                "start_token": "linux:test-boot-id:138598638",
                "stage": "read-executable-link",
                "path_class": "proc-executable-link",
                "error_kind": "_LinuxProcRace",
                "errno": errno.ESTALE,
                "errno_name": "ESTALE",
                "cause_kind": "FileNotFoundError",
                "cause_errno": errno.ENOENT,
                "cause_errno_name": "ENOENT",
                "pidfd_ready": False,
                "reason": "proc-path-missing-without-pidfd-exit-proof",
            },
        )
        self.assertNotIn("argv", details[0])
        self.assertNotIn("executable", details[0])

    def test_issue_details_are_bounded_without_changing_issue_counts(self):
        (records, issues, races), details = self.scan(census.MAX_REPORTED_PROCESS_ISSUES + 1)

        self.assertEqual(records, {})
        self.assertEqual(
            issues,
            {"same-uid-process-snapshot-incomplete": census.MAX_REPORTED_PROCESS_ISSUES + 1},
        )
        self.assertEqual(races, {})
        self.assertEqual(len(details), census.MAX_REPORTED_PROCESS_ISSUES)

    def test_linux_json_exposes_bounded_issue_details_without_admitting_incomplete_snapshot(self):
        details = {
            "category": "same-uid-process-snapshot-incomplete",
            "pid": 4100,
            "ps_started": "Thu Oct 15 01:24:22 2026",
            "start_token": "linux:test-boot-id:138598638",
            "stage": "read-executable-link",
            "path_class": "proc-executable-link",
            "error_kind": "_LinuxProcRace",
            "errno": errno.ESTALE,
            "errno_name": "ESTALE",
            "cause_kind": "FileNotFoundError",
            "cause_errno": errno.ENOENT,
            "cause_errno_name": "ENOENT",
            "pidfd_ready": False,
            "reason": "proc-path-missing-without-pidfd-exit-proof",
        }
        snapshot = census.ProcessSnapshot(
            records={},
            started_at_utc="2026-10-15T05:00:00+00:00",
            finished_at_utc="2026-10-15T05:00:01+00:00",
            complete=False,
            issue_counts={"same-uid-process-snapshot-incomplete": 2},
            race_counts={},
            effective_uid=0,
            visible_process_count=1,
            captured_argv_bytes=0,
            captured_argv_processes=0,
            argv_budget_exhausted=False,
            issue_details=(details,),
            issue_details_omitted_count=1,
        )
        original_snapshot = census._snapshot_now
        original_platform = sys.platform
        try:
            sys.platform = "linux"
            census._snapshot_now = lambda: snapshot
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                result = census.main([])
        finally:
            census._snapshot_now = original_snapshot
            sys.platform = original_platform

        report = json.loads(output.getvalue())
        self.assertEqual(result, 75)
        self.assertFalse(report["complete"])
        self.assertEqual(report["snapshot_issue_counts"], {"same-uid-process-snapshot-incomplete": 2})
        self.assertEqual(report["snapshot_issue_details"], [details])
        self.assertEqual(report["snapshot_issue_details_omitted_count"], 1)


if __name__ == "__main__":
    unittest.main()
