from __future__ import annotations

import ctypes
import errno
import importlib.util
import json
import pathlib
import sys
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).parents[1] / ".config/scripts/build-census.py"
SPEC = importlib.util.spec_from_file_location("build_census", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
census = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = census
SPEC.loader.exec_module(census)


def process(
    pid: int,
    argv: tuple[str, ...] | None,
    *,
    ppid: int = 1,
    pgid: int = 1,
    start: str | None = None,
    executable: str = "/toolchain/bin/cargo",
    argv_error: str | None = None,
    state: str | None = None,
) -> census.ProcessRecord:
    return census.ProcessRecord(
        pid=pid,
        ppid=ppid,
        pgid=pgid,
        start_token=start or f"test:{pid}:start",
        executable=executable,
        argv=argv,
        argv_error=argv_error,
        state=state,
    )


class CargoCensusTests(unittest.TestCase):
    def test_unreadable_same_uid_executable_is_retained_as_unknown_occupied_group(self) -> None:
        row = census._PsRow(11939, 1, 8800, 501, "Wed Oct 7 13:46:26 2026", "browser_crashpad")

        def pidinfo(_pid: int, _flavor: int, _arg: int, output: object, _size: int) -> int:
            info = ctypes.cast(output, ctypes.POINTER(census._ProcBsdInfo)).contents
            info.pid = row.pid
            info.ppid = row.ppid
            info.uid = 501
            info.pgid = row.pgid
            info.start_sec = 1_791_380_786
            info.start_usec = 289_586
            info.status = 2
            return ctypes.sizeof(info)

        def pidpath(_pid: int, _buffer: object, _size: int) -> int:
            ctypes.set_errno(errno.ENOENT)
            return 0

        proc = mock.Mock()
        proc.proc_pidinfo = pidinfo
        proc.proc_pidpath = pidpath
        libc = mock.Mock()
        libc.sysctl = lambda *_args: 0

        def load_library(path: str, **_kwargs: object) -> object:
            return proc if path.endswith("libproc.dylib") else libc

        with mock.patch.object(census.ctypes, "CDLL", side_effect=load_library):
            records, issues, races = census._darwin_processes(
                501,
                [row],
                census._ArgvBudget(),
            )

        self.assertEqual(issues, {})
        self.assertEqual(races, {})
        record = records[row.pid]
        self.assertTrue(record.identity_validated)
        self.assertEqual(record.start_token, "darwin:1791380786:289586")
        self.assertEqual((record.ppid, record.pgid), (row.ppid, row.pgid))
        self.assertIsNone(record.executable)
        self.assertTrue(record.unknown_executable_candidate)

        snapshot = census.ProcessSnapshot(
            records=records,
            started_at_utc="2026-10-08T05:09:05+00:00",
            finished_at_utc="2026-10-08T05:09:05.1+00:00",
            complete=not issues,
            issue_counts=issues,
            race_counts=races,
            effective_uid=501,
            visible_process_count=1,
            captured_argv_bytes=0,
            captured_argv_processes=0,
            argv_budget_exhausted=False,
        )
        groups = census.compiler_groups(snapshot, census.cargo_entries(snapshot))
        self.assertEqual(groups[0]["kind"], "unknown")
        self.assertEqual(groups[0]["classification"], "unknown")
        self.assertEqual(groups[0]["pgid"], row.pgid)
        self.assertEqual(groups[0]["pids"], [row.pid])

    def test_compiler_groups_count_known_and_foreign_runtime_owners_by_process_group(self) -> None:
        known = census.ProcessRecord(
            pid=701,
            ppid=1,
            pgid=700,
            start_token="known-runtime-start",
            executable="/private/bin/backend-locald",
            argv=None,
        )
        foreign_row = census._PsRow(702, 1, 702, 501, "Fri Oct 2 03:04:26 2026", "locald")
        unknown = census._unknown_runtime_owner_candidate(foreign_row)
        snapshot = census.ProcessSnapshot(
            records={known.pid: known, unknown.pid: unknown},
            started_at_utc="2026-10-06T17:00:00+00:00",
            finished_at_utc="2026-10-06T17:00:01+00:00",
            complete=True,
            issue_counts={},
            race_counts={},
            effective_uid=1000,
            visible_process_count=2,
            captured_argv_bytes=0,
            captured_argv_processes=0,
            argv_budget_exhausted=False,
        )

        groups = census.compiler_groups(snapshot, [])

        self.assertEqual([group["pgid"] for group in groups], [700, 702])
        self.assertEqual(groups[0]["kind"], "runtime-owner")
        self.assertEqual(groups[0]["classification"], "runtime-owner")
        self.assertEqual(groups[1]["kind"], "runtime-owner")
        self.assertEqual(groups[1]["classification"], "unknown")

    def test_host_process_rows_parse_darwin_and_linux_fixed_start_fields(self) -> None:
        rows = census._parse_ps_rows(
            "  41  12  41  501 Fri Oct  2 03:04:26 2026 cargo\n"
            "  42   1  42  0   Mon Sep 28 16:17:33 2026 launchd   \n"
        )
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0], census._PsRow(41, 12, 41, 501, "Fri Oct 2 03:04:26 2026", "cargo"))
        self.assertEqual(rows[1].effective_uid, 0)
        self.assertEqual(rows[1].ucomm, "launchd")
        sentinel = census._parse_ps_rows("43 1 43 -2 Fri Oct 2 03:04:26 2026 cargo\n")
        self.assertIsNone(sentinel[0].effective_uid)

    def test_process_table_parser_rejects_malformed_and_duplicate_pid_rows(self) -> None:
        with self.assertRaises(census.CensusError):
            census._parse_ps_rows("not a process row\n")
        repeated = "41 12 41 501 Fri Oct 2 03:04:26 2026 cargo\n" * 2
        with self.assertRaises(census.CensusError):
            census._parse_ps_rows(repeated)

    def test_process_table_parser_enforces_aggregate_entry_ceiling(self) -> None:
        two_rows = "41 12 41 501 Fri Oct 2 03:04:26 2026 cargo\n" * 2
        with mock.patch.object(census, "MAX_PROCESS_COUNT", 1):
            with self.assertRaisesRegex(census.CensusError, "process-count-limit"):
                census._parse_ps_rows(two_rows)

    def test_argv_capture_budget_is_aggregate_and_stays_exhausted(self) -> None:
        budget = census._ArgvBudget()
        with (
            mock.patch.object(census, "MAX_SNAPSHOT_ARG_BYTES", 5),
            mock.patch.object(census, "MAX_SNAPSHOT_ARG_PROCESSES", 2),
        ):
            self.assertTrue(budget.admit(3))
            self.assertTrue(budget.admit(2))
            self.assertFalse(budget.admit(0))
            self.assertTrue(budget.exhausted)
            self.assertFalse(budget.admit(1))
        self.assertEqual((budget.captured_bytes, budget.captured_processes), (5, 2))

    def test_foreign_cargo_hint_without_native_details_is_unknown_and_counts(self) -> None:
        row = census._PsRow(140, 1, 140, 42, "Fri Oct 2 03:04:26 2026", "cargo")
        record = census._unknown_cargo_candidate(row)
        entry = census.classify_cargo_process(record)
        self.assertIsNotNone(entry)
        assert entry is not None
        self.assertEqual((entry.classification, entry.reason), ("unknown", "cargo-candidate-details-unavailable"))
        self.assertIsNone(record.start_token)
        self.assertFalse(record.identity_validated)
        self.assertFalse(census.is_owned_by(140, census.ProcessIdentity(1, "init"), {140: record}))
        result = census.decide_capacity([entry], snapshot_complete=True)
        self.assertEqual(result["unknown_cargo_processes"], 1)

    def test_process_name_hint_never_overrides_validated_non_cargo_executable(self) -> None:
        record = census.ProcessRecord(
            pid=150,
            ppid=1,
            pgid=150,
            start_token="validated-start",
            executable="/usr/local/bin/cargo-wrapper",
            argv=("cargo", "build"),
            comm_candidate=True,
        )
        self.assertIsNone(census.classify_cargo_process(record))

    def test_real_argv_boundaries_do_not_promote_option_values_to_verbs(self) -> None:
        record = process(
            41,
            (
                "/toolchain/bin/cargo",
                "+stable",
                "--config",
                "token='private build value'",
                "metadata",
                "--manifest-path",
                "/private/work tree/Cargo.toml",
                "--token=never-print-this",
                "--offline",
            ),
        )

        entry = census.classify_cargo_process(record)
        self.assertIsNotNone(entry)
        assert entry is not None
        self.assertEqual((entry.classification, entry.verb), ("introspection", "metadata"))
        safe = census._safe_argv(record, entry.verb)
        encoded = json.dumps(safe)
        self.assertNotIn("private build value", encoded)
        self.assertNotIn("/private/work tree", encoded)
        self.assertNotIn("never-print-this", encoded)
        self.assertIn("metadata", safe["tokens"])

    def test_mixed_queries_builds_and_unknowns_use_conservative_occupancy(self) -> None:
        records = [
            process(10, ("cargo", "build", "--offline")),
            process(11, ("cargo", "metadata", "--offline")),
            process(12, ("cargo", "future-command")),
            process(13, ("cargo", "-V")),
        ]
        entries = [entry for record in records if (entry := census.classify_cargo_process(record))]

        result = census.decide_capacity(entries, snapshot_complete=True)
        self.assertEqual(result["known_build_processes"], 1)
        self.assertEqual(result["introspection_processes"], 2)
        self.assertEqual(result["unknown_cargo_processes"], 1)
        self.assertEqual(result["conservative_local_occupancy"], 2)
        self.assertTrue(result["snapshot_allows_new_local_build"])

        full = [
            *entries,
            census.classify_cargo_process(process(14, ("cargo", "check"))),
        ]
        full = [entry for entry in full if entry is not None]
        result = census.decide_capacity(full, snapshot_complete=True)
        self.assertEqual(result["conservative_local_occupancy"], 3)
        self.assertFalse(result["snapshot_allows_new_local_build"])
        self.assertEqual(result["decision_reason"], "local-build-capacity-full")

    def test_unknown_option_before_verb_is_not_guessed_from_its_value(self) -> None:
        entry = census.classify_cargo_process(
            process(20, ("cargo", "--future-option", "build", "--offline"))
        )
        self.assertIsNotNone(entry)
        assert entry is not None
        self.assertEqual(entry.classification, "unknown")
        self.assertIsNone(entry.verb)
        self.assertEqual(entry.reason, "unrecognized-option-before-verb")

    def test_root_owned_build_counts_but_is_not_mistaken_for_this_run(self) -> None:
        root_driver = process(50, ("cargo", "build"), ppid=1)
        init = process(1, ("init",), ppid=0, executable="/sbin/launchd")
        records = {1: init, 50: root_driver}
        entry = census.classify_cargo_process(root_driver)
        self.assertIsNotNone(entry)
        assert entry is not None
        self.assertEqual(entry.classification, "build")
        self.assertFalse(census.is_owned_by(50, census.ProcessIdentity(99, "run-owner"), records))
        self.assertTrue(census.ancestry_for(50, records)[1])

    def test_shared_process_group_does_not_make_a_sibling_run_owned(self) -> None:
        owner = process(60, ("driver",), ppid=1, pgid=500, executable="/bin/driver")
        unrelated_driver = process(61, ("other-driver",), ppid=1, pgid=500, executable="/bin/other")
        cargo = process(62, ("cargo", "build"), ppid=61, pgid=500)
        records = {60: owner, 61: unrelated_driver, 62: cargo}

        self.assertFalse(census.is_owned_by(62, census.ProcessIdentity(60, owner.start_token), records))

    def test_pid_reuse_does_not_satisfy_run_owner_identity(self) -> None:
        owner_reused = process(70, ("driver",), ppid=1, start="new-owner-start", executable="/bin/driver")
        child = process(71, ("cargo", "test"), ppid=70)
        records = {70: owner_reused, 71: child}

        self.assertFalse(census.is_owned_by(71, census.ProcessIdentity(70, "old-owner-start"), records))
        self.assertTrue(census.is_owned_by(71, census.ProcessIdentity(70, "new-owner-start"), records))

    def test_missing_parent_fails_closed_for_ownership_and_ancestry(self) -> None:
        orphan = process(80, ("cargo", "run"), ppid=79)
        records = {80: orphan}

        ancestry, complete, reason = census.ancestry_for(80, records)
        self.assertEqual(ancestry, [])
        self.assertFalse(complete)
        self.assertEqual(reason, "missing-parent")
        self.assertFalse(census.is_owned_by(80, census.ProcessIdentity(77, "owner"), records))

    def test_unreadable_argv_counts_as_unknown_and_snapshot_failure_refuses(self) -> None:
        unreadable = process(
            90,
            None,
            argv_error="argv-byte-limit",
        )
        entry = census.classify_cargo_process(unreadable)
        self.assertIsNotNone(entry)
        assert entry is not None
        self.assertEqual(entry.classification, "unknown")
        result = census.decide_capacity(
            [
                census.classify_cargo_process(process(91, ("cargo", "build"))),
                census.classify_cargo_process(process(92, ("cargo", "check"))),
                entry,
            ],
            snapshot_complete=True,
        )
        self.assertFalse(result["snapshot_allows_new_local_build"])
        self.assertEqual(result["unknown_cargo_processes"], 1)
        incomplete = census.decide_capacity([], snapshot_complete=False)
        self.assertFalse(incomplete["snapshot_allows_new_local_build"])
        self.assertEqual(incomplete["decision_reason"], "snapshot-incomplete")

    def test_non_cargo_executable_is_not_classified_from_process_argv(self) -> None:
        shell_script = process(
            100,
            ("/bin/bash", "/tmp/cargo-wrapped", "build"),
            executable="/bin/bash",
        )
        self.assertIsNone(census.classify_cargo_process(shell_script))

    def test_exited_process_does_not_consume_a_build_slot(self) -> None:
        exited = process(110, ("cargo", "build"), state="Z")
        entry = census.classify_cargo_process(exited)
        self.assertIsNotNone(entry)
        assert entry is not None
        self.assertEqual(entry.classification, "exited")
        result = census.decide_capacity([entry], snapshot_complete=True)
        self.assertEqual(result["known_build_processes"], 0)
        self.assertEqual(result["exited_cargo_processes"], 1)
        self.assertTrue(result["snapshot_allows_new_local_build"])


if __name__ == "__main__":
    unittest.main()
