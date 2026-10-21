from __future__ import annotations

import dataclasses
import errno
import importlib.util
import pathlib
import sys
import tempfile
import types
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).parents[1]


def load_script(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / ".config/scripts" / filename)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


census = load_script("census_exiting_witness", "build-census.py")
fleet = load_script("fleet_exiting_witness", "fleet-capacity-census.py")


def stat(**changes):
    fields = ["D", "11", "12", "13", "0", "0", str(0x40010C)] + ["0"] * 12 + ["101"]
    fields[17] = "1"  # num_threads is field 20, starttime is field 22.
    return dataclasses.replace(census._parse_linux_proc_stat("41 (nix) " + " ".join(fields)), **changes)


def status(**changes):
    fields = {
        "Name": "nix", "State": "D (disk sleep)", "Tgid": "41", "Pid": "41", "PPid": "11",
        "Uid": "1000\t1000\t1000\t1000", "Kthread": "0", "Threads": "1",
        "voluntary_ctxt_switches": "10", "nonvoluntary_ctxt_switches": "2",
    }
    fields.update(changes)
    return "".join(f"{key}:\t{value}\n" for key, value in fields.items())


class Proc:
    """Scripted proc payloads only: no real /proc, pidfd, ps or child process."""

    def __init__(self, *, stats=None, statuses=None, ready=None, error=None, executable=None):
        self.stats = list(stats if stats is not None else [stat()] * 3)
        self.statuses = list(statuses if statuses is not None else [status()] * 2)
        self.ready = list(ready if ready is not None else [False] * 12)
        self.error = error or FileNotFoundError(errno.ENOENT, "absent", "/proc/41/exe")
        self.executable = executable
        self.events = []

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return None

    def pin_pidfd(self):
        self.events.append("pin")

    def read_stat(self):
        self.events.append("stat")
        return self.stats.pop(0)

    def pidfd_ready(self):
        self.events.append("pidfd")
        return self.ready.pop(0)

    def read_text(self, path):
        self.events.append(path)
        assert path == "status"
        value = self.statuses.pop(0)
        if isinstance(value, BaseException):
            raise value
        return value

    def readlink(self, path):
        self.events.append(path)
        assert path == "exe"
        if self.executable is not None:
            return self.executable
        raise self.error

    def read_bytes(self, path, _limit):
        self.events.append(path)
        assert path == "cmdline"
        return b"cargo\0build\0-j64\0"


class LinuxExitingWitnessTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="census-exiting-fixture-")
        self.addCleanup(self.directory.cleanup)
        self.proc_root = pathlib.Path(self.directory.name)
        boot = self.proc_root / "sys/kernel/random/boot_id"
        boot.parent.mkdir(parents=True)
        boot.write_text("fixture-boot\n", encoding="ascii")

    def capture(self, proc, hint="nix", *, release="6.18.38"):
        row = census._PsRow(41, 11, 12, 1000, "Thu Oct 15 01:24:22 2026", hint)
        details = []
        with mock.patch.object(census.os, "uname", return_value=types.SimpleNamespace(release=release)):
            records, issues, races = census._linux_processes(
                1000, [row], census._ArgvBudget(), proc_root=self.proc_root,
                proc_factory=lambda _pid: proc, issue_details=details,
            )
        return records, issues, races, details

    def assert_refused(self, proc):
        records, issues, races, details = self.capture(proc)
        self.assertEqual(records, {})
        self.assertEqual(sum(issues.values()), 1)
        self.assertEqual(races, {}, "missing proof cannot become a terminal/exit race")
        self.assertEqual(len(details), 1, "additive issue diagnostics remain present")
        self.assertEqual(details[0]["pid"], 41)

    @staticmethod
    def groups(records):
        snapshot = census.ProcessSnapshot(
            records, "2026-10-15T06:00:00Z", "2026-10-15T06:00:01Z", True,
            {}, {}, 1000, len(records), 0, 0, False,
        )
        return census.compiler_groups(snapshot, census.cargo_entries(snapshot))

    def test_single_exiting_task_remains_identity_validated_unknown_occupied_group(self):
        proc = Proc()
        records, issues, races, details = self.capture(proc)
        self.assertEqual((issues, races, details), ({}, {}, []))
        record = records[41]
        self.assertEqual(record.start_token, "linux:fixture-boot:101")
        self.assertEqual((record.ppid, record.pgid, record.state), (11, 12, "D"))
        self.assertTrue(record.identity_validated)
        self.assertTrue(record.unknown_executable_candidate)
        self.assertIsNone(record.executable)
        self.assertIsNone(record.argv)
        group, = self.groups(records)
        self.assertEqual((group["kind"], group["classification"], group["pgid"]), ("unknown", "unknown", 12))
        self.assertEqual(group["pids"], [41])
        self.assertEqual(group["admission_slot_count"], 1)
        self.assertFalse(group["job_limit_known"])
        self.assertEqual(proc.events.count("status"), 2)
        self.assertNotIn("cmdline", proc.events)

    def test_only_exact_audited_kernel_release_can_supply_a_witness(self):
        for release in ("6.18.37", "6.18.39", "6.18.54", "6.18.38-custom", "6.18.38+",
                        "6.18.38\n", " 6.18.38", "6.18.038", "", None, b"6.18.38", 61838):
            with self.subTest(release=release):
                proc = Proc()
                records, issues, races, details = self.capture(proc, release=release)
                self.assertEqual(records, {})
                self.assertEqual(issues, {"same-uid-process-snapshot-incomplete": 1})
                self.assertEqual(races, {})
                self.assertEqual(details[0]["reason"], "proc-path-missing-without-pidfd-exit-proof")
                self.assertEqual(proc.events.count("status"), 1, "unsupported kernels cannot enter the witness")

    def test_missing_or_unavailable_uname_release_refuses_the_new_proof(self):
        for result in (types.SimpleNamespace(), object()):
            with self.subTest(result=result), mock.patch.object(census.os, "uname", return_value=result):
                self.assertFalse(census._linux_exiting_witness_kernel_supported())
        with mock.patch.object(census.os, "uname", side_effect=OSError(errno.EIO, "unavailable")):
            self.assertFalse(census._linux_exiting_witness_kernel_supported())

    def test_cargo_compiler_and_runtime_hints_are_preserved_not_hidden_in_generic_unknown(self):
        for hint, kind, flag in (
            ("cargo", "cargo", "comm_candidate"),
            ("rustc", "orphan-rustc", "compiler_candidate"),
            ("backend-locald", "runtime-owner", "runtime_owner_candidate"),
            ("backend-desktop", "runtime-owner", "runtime_owner_candidate"),
        ):
            with self.subTest(hint=hint):
                records, issues, races, _ = self.capture(Proc(), hint)
                self.assertEqual((issues, races), ({}, {}))
                self.assertTrue(getattr(records[41], flag))
                self.assertFalse(records[41].unknown_executable_candidate)
                group, = self.groups(records)
                self.assertEqual((group["kind"], group["classification"]), (kind, "unknown"))
                self.assertEqual(group["admission_slot_count"], 1)
                self.assertFalse(group["job_limit_known"])

    def test_any_stat_identity_group_state_flag_or_thread_change_refuses(self):
        changes = (
            {"start_ticks": "202"}, {"ppid": 99}, {"pgid": 99}, {"session": 99},
            {"state": "R"}, {"flags": 0x400108}, {"flags": 0x40010D},
            {"thread_count": None}, {"thread_count": 0}, {"thread_count": 2},
        )
        for position in range(3):
            for change in changes:
                with self.subTest(position=position, change=change):
                    stats = [stat()] * 3
                    stats[position] = stat(**change)
                    self.assert_refused(Proc(stats=stats))

    def test_raw_stable_pf_exiting_and_stat_thread_count_alone_are_not_proof(self):
        # Mixed-field stat/de_thread counterexample: status still has an mm or
        # observes a sibling, even though all stat rows claim PF_EXITING + 1.
        for text in (status(VmSize="0 kB"), status(Threads="2"), "Uid:\t1000\t1000\t1000\t1000\n"):
            with self.subTest(status=text):
                self.assert_refused(Proc(statuses=[text, text]))
        self.assert_refused(Proc(stats=[stat(flags=0x400108)] * 3))
        self.assert_refused(Proc(stats=[stat(thread_count=None)] * 3))

    def test_each_status_requires_complete_duplicate_free_fields_and_matching_identity(self):
        for key in (
            "Name", "State", "Pid", "Tgid", "PPid", "Uid", "Kthread", "Threads",
            "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches",
        ):
            for position in (0, 1):
                for invalid in (
                    "".join(line + "\n" for line in status().splitlines() if not line.startswith(key + ":")),
                    status() + key + ":\t1\n",
                ):
                    with self.subTest(key=key, position=position, invalid=invalid):
                        texts = [status()] * 2
                        texts[position] = invalid
                        self.assert_refused(Proc(statuses=texts))

    def test_status_identity_threads_kernel_thread_and_uid_changes_refuse(self):
        for change in (
            {"Pid": "42"}, {"Tgid": "42"}, {"PPid": "12"}, {"State": "R (running)"},
            {"State": ""}, {"Threads": "2"}, {"Threads": "0"}, {"Kthread": "1"},
            {"Uid": "1000 1001 1000 1000"}, {"Uid": "1000 1000"},
            {"Uid": "1000 1000 -1 1000"}, {"Uid": "1000 1000 1000 1001"},
            {"Threads": "+1"}, {"nonvoluntary_ctxt_switches": "missing"},
        ):
            for position in (0, 1):
                if position == 0 and change == {"Uid": "1000 1001 1000 1000"}:
                    continue  # The pre-existing UID-scope race is tested separately.
                with self.subTest(change=change, position=position):
                    texts = [status()] * 2
                    texts[position] = status(**change)
                    self.assert_refused(Proc(statuses=texts))

    def test_initial_uid_scope_change_keeps_the_existing_race_without_creating_a_witness(self):
        proc = Proc(statuses=[status(Uid="1000 1001 1000 1000")])
        records, issues, races, details = self.capture(proc)
        self.assertEqual((records, issues, details), ({}, {}, []))
        self.assertEqual(races, {"pid-uid-changed-during-scan": 1})
        self.assertNotIn("exe", proc.events, "initial UID mismatch retains the existing scoped read behavior")

    def test_zero_memory_values_and_mmu_or_nommu_memory_fields_are_not_no_mm(self):
        for key in ("VmSize", "VmPeak", "VmFuture", "RssAnon", "Mem", "Slack", "Shared",
                    "CoreDumping", "HugetlbPages", "THP_enabled", "untag_mask"):
            for position in (0, 1):
                with self.subTest(key=key, position=position):
                    texts = [status()] * 2
                    texts[position] = status(**{key: "0"})
                    self.assert_refused(Proc(statuses=texts))

    def test_bounded_status_requires_newline_no_nul_and_well_formed_keys(self):
        for text in (status().rstrip("\n"), status() + "bad-line\n", status() + "X:\t\0\n",
                     status() + "bad key:\t0\n", status() + "X:\t" + "x" * census.MAX_RESOURCE_OUTPUT_BYTES + "\n"):
            with self.subTest(length=len(text)):
                self.assert_refused(Proc(statuses=[text, text]))

    def test_unavailable_or_ready_pidfd_is_not_a_live_exiting_witness(self):
        for ready in ([None] * 12, [True] * 12, [False, None] + [False] * 12,
                      [False, False, True] + [False] * 12):
            with self.subTest(ready=ready):
                self.assert_refused(Proc(ready=ready))

    def test_permission_io_and_other_missing_payloads_stay_strict(self):
        for error in (PermissionError(errno.EACCES, "denied", "/proc/41/exe"),
                      OSError(errno.EIO, "io", "/proc/41/exe"),
                      FileNotFoundError(errno.EACCES, "not absence", "/proc/41/exe")):
            with self.subTest(error=error):
                self.assert_refused(Proc(error=error))
        for position in (0, 1):
            texts = [status()] * 2
            texts[position] = FileNotFoundError(errno.ENOENT, "missing", "/proc/41/status")
            self.assert_refused(Proc(statuses=texts))

    def test_accessible_cargo_j64_is_still_detected_and_refused_by_unchanged_fleet_cap(self):
        records, issues, races, _ = self.capture(Proc(executable="/toolchain/cargo", statuses=[status(VmSize="1234 kB")]))
        self.assertEqual((issues, races), ({}, {}))
        entry, = census.cargo_entries(census.ProcessSnapshot(
            records, "", "", True, {}, {}, 1000, 1, 0, 0, False,
        ))
        self.assertEqual((entry.classification, entry.requested_jobs), ("build", 64))
        group, = self.groups(records)
        reasons = []
        fleet._validate_cargo_group_joins(group, host="fixture", entry_by_pid={41: [{
            "pgid": 12, "classification": "build", "identity_validated": True,
            "start_token": records[41].start_token, "requested_cargo_jobs": 64,
        }]}, group_coverage={}, reasons=reasons, limitations=[])
        self.assertIn("fixture:active-cargo-process-jobs-exceed-4", reasons)


if __name__ == "__main__":
    unittest.main()
