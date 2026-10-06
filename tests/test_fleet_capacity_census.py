from __future__ import annotations

import datetime as dt
import importlib.util
import pathlib
import sys
import unittest
from collections import Counter


SCRIPT = pathlib.Path(__file__).parents[1] / ".config/scripts/fleet-capacity-census.py"
SPEC = importlib.util.spec_from_file_location("fleet_capacity_census", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
fleet = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = fleet
SPEC.loader.exec_module(fleet)


def empty_samples(now: dt.datetime) -> dict[str, dict[str, object]]:
    stamp = (now - dt.timedelta(seconds=1)).isoformat()
    result: dict[str, dict[str, object]] = {}
    for name in fleet.DEFAULT_HOSTS:
        result[name] = {
            "host": name,
            "transport_complete": True,
            "return_code": 0,
            "census_sha256": "a" * 64,
            "census": {
                "schema": "build-census.v1",
                "complete": True,
                "resource_snapshot_complete": True,
                "sample_started_at_utc": stamp,
                "sample_finished_at_utc": stamp,
                "entries_omitted_count": 0,
                "compiler_groups_omitted_count": 0,
                "visible_process_count": 100,
                "entries": [],
                "compiler_groups": [],
                "snapshot_issue_counts": {},
                "snapshot_race_counts": {},
                "resource_snapshot": {
                    "sampled_at_utc": stamp,
                    "available_memory_bytes": 9 * fleet.GIB,
                    "available_memory_source": "test",
                    "root_disk_available_bytes": 20 * fleet.GIB,
                },
            },
        }
    return result


def compiler_group(pgid: int, *, kind: str = "orphan-rustc", classification: str = "orphan-rustc", jobs: int | None = None) -> dict[str, object]:
    return {
        "pgid": pgid,
        "kind": kind,
        "classification": classification,
        "pids": [pgid],
        "cargo_pids": [pgid] if kind == "cargo" else [],
        "rustc_process_count": 1,
        "requested_cargo_jobs": jobs,
        "job_limit_known": jobs is not None,
    }


class FleetCapacityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.now = dt.datetime(2026, 10, 6, 17, 0, tzinfo=dt.timezone.utc)
        self.samples = empty_samples(self.now)

    def evaluate(self, destination: str = "local") -> dict[str, object]:
        return fleet.evaluate_fleet(
            self.samples,
            destination=destination,
            requested_jobs=4,
            now=self.now,
        )

    def test_ilo_python_uses_the_durable_nix_gc_root(self) -> None:
        self.assertEqual(
            fleet.DEFAULT_HOSTS["ilo"]["python"],
            "/root/nudox-corpus-20261006/tool-recovery/roots/fleet-census-python3-3.14.6/bin/python3",
        )

    def test_complete_fresh_empty_fleet_allows_advisory_without_reserving_slot(self) -> None:
        result = self.evaluate()
        self.assertTrue(result["advisory_allowed"])
        self.assertFalse(result["slot_reserved"])
        self.assertTrue(result["managed_host_permit_required"])
        self.assertEqual(result["current_fleet_compiler_group_count"], 0)

    def test_unreachable_incomplete_stale_or_truncated_host_fails_closed(self) -> None:
        self.samples["ilo"]["transport_complete"] = False
        result = self.evaluate()
        self.assertFalse(result["advisory_allowed"])
        self.assertTrue(any("ilo:unreachable" in reason for reason in result["reasons"]))

        self.samples = empty_samples(self.now)
        self.samples["h16001mac"]["census"]["compiler_groups_omitted_count"] = 1
        self.assertFalse(self.evaluate()["advisory_allowed"])

        self.samples = empty_samples(self.now)
        self.samples["ilo"]["census"]["sample_finished_at_utc"] = (
            self.now - dt.timedelta(seconds=61)
        ).isoformat()
        self.assertFalse(self.evaluate()["advisory_allowed"])

    def test_memory_and_destination_disk_floor_apply_to_every_admission(self) -> None:
        self.samples["ilo"]["census"]["resource_snapshot"]["available_memory_bytes"] = 8 * fleet.GIB - 1
        self.assertFalse(self.evaluate()["advisory_allowed"])

        self.samples = empty_samples(self.now)
        self.samples["local"]["census"]["resource_snapshot"]["root_disk_available_bytes"] = 16 * fleet.GIB - 1
        self.assertFalse(self.evaluate()["advisory_allowed"])

    def test_host_and_total_group_limits_include_the_proposed_slot(self) -> None:
        self.samples["local"]["census"]["compiler_groups"] = [compiler_group(100 + index) for index in range(5)]
        self.assertFalse(self.evaluate("local")["advisory_allowed"])

        self.samples = empty_samples(self.now)
        for host_index, (host, count) in enumerate((("local", 4), ("ilo", 6), ("h16001mac", 6)), start=1):
            self.samples[host]["census"]["compiler_groups"] = [
                compiler_group(1000 * host_index + offset)
                for offset in range(count)
            ]
        result = self.evaluate("local")
        self.assertEqual(result["current_fleet_compiler_group_count"], 16)
        self.assertFalse(result["advisory_allowed"])

    def test_unknown_processes_count_conservatively_and_known_overlarge_jobs_refuse(self) -> None:
        self.samples["ilo"]["census"]["compiler_groups"] = [
            compiler_group(2200, kind="cargo", classification="unknown")
        ]
        result = self.evaluate("h16001mac")
        self.assertTrue(result["advisory_allowed"])
        self.assertEqual(result["current_fleet_compiler_group_count"], 1)
        self.assertTrue(any("counted-conservatively" in item for item in result["limitations"]))

        self.samples = empty_samples(self.now)
        self.samples["ilo"]["census"]["compiler_groups"] = [
            compiler_group(2201, kind="cargo", classification="build", jobs=5)
        ]
        self.assertFalse(self.evaluate("h16001mac")["advisory_allowed"])

        self.samples = empty_samples(self.now)
        self.samples["h16001mac"]["census"]["compiler_groups"] = [
            compiler_group(2202, kind="runtime-owner", classification="runtime-owner")
        ]
        runtime_result = self.evaluate("h16001mac")
        self.assertTrue(runtime_result["advisory_allowed"])
        self.assertEqual(runtime_result["current_fleet_compiler_group_count"], 1)

        self.samples = empty_samples(self.now)
        result = fleet.evaluate_fleet(
            self.samples,
            destination="h16001mac",
            requested_jobs=5,
            now=self.now,
        )
        self.assertFalse(result["advisory_allowed"])

    def test_collect_once_calls_each_configured_host_exactly_once(self) -> None:
        counts: Counter[str] = Counter()

        def fake_sample(name: str, host: object, source: bytes) -> dict[str, object]:
            counts[name] += 1
            return {"host": name, "transport_complete": True}

        result = fleet.collect_once(
            fleet.DEFAULT_HOSTS,
            b"sampler bytes",
            sample_host=fake_sample,
        )
        self.assertEqual(set(result), set(fleet.DEFAULT_HOSTS))
        self.assertEqual(counts, Counter({name: 1 for name in fleet.DEFAULT_HOSTS}))

    def test_authorized_remote_exception_retains_every_other_gate(self) -> None:
        self.samples["local"]["census"]["compiler_groups"] = [
            compiler_group(3000 + index) for index in range(9)
        ]

        def authorized(destination: str = "h16001mac") -> dict[str, object]:
            return fleet.evaluate_fleet(
                self.samples, destination=destination, requested_jobs=4,
                now=self.now, allow_local_over_cap_for_remote=True,
            )

        self.assertFalse(self.evaluate("h16001mac")["advisory_allowed"])
        result = authorized()
        self.assertTrue(result["advisory_allowed"])
        self.assertEqual(result["current_fleet_compiler_group_count"], 9)
        self.assertIn("local:over-host-cap-remote-admission-authorized", result["limitations"])
        self.assertFalse(authorized("local")["advisory_allowed"])

        for host in fleet.DEFAULT_HOSTS:
            resources = self.samples[host]["census"]["resource_snapshot"]
            resources["available_memory_bytes"] = fleet.MIN_AVAILABLE_MEMORY - 1
            self.assertFalse(authorized()["advisory_allowed"], host)
            resources["available_memory_bytes"] = 9 * fleet.GIB

        self.samples["h16001mac"]["census"]["compiler_groups"] = [
            compiler_group(4000 + index) for index in range(8)
        ]
        self.assertFalse(authorized()["advisory_allowed"])
        self.samples["h16001mac"]["census"]["compiler_groups"] = [
            compiler_group(5000, kind="cargo", classification="build", jobs=5)
        ]
        self.assertFalse(authorized()["advisory_allowed"])
        self.samples["h16001mac"]["census"]["compiler_groups"] = []
        self.samples["ilo"]["transport_complete"] = False
        self.assertFalse(authorized()["advisory_allowed"])


if __name__ == "__main__":
    unittest.main()
