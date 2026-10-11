"""Admission-policy controls; these never sample hosts or launch workloads."""

import copy
import datetime as dt
import importlib.util
import json
from pathlib import Path
import unittest


_spec = importlib.util.spec_from_file_location(
    "fleet_capacity_census", Path(__file__).with_name("fleet-capacity-census.py")
)
fleet = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fleet)


class FleetAdmissionTests(unittest.TestCase):
    def setUp(self):
        self.now = dt.datetime(2026, 10, 8, tzinfo=dt.timezone.utc)
        stamp = self.now.isoformat()
        self.samples = {
            name: {
                "transport_complete": True,
                "return_code": 0,
                "census": {
                    "schema": "build-census.v1",
                    "complete": True,
                    "resource_snapshot_complete": True,
                    "sample_started_at_utc": stamp,
                    "sample_finished_at_utc": stamp,
                    "entries_omitted_count": 0,
                    "entries": [],
                    "compiler_groups_omitted_count": 0,
                    "compiler_groups": [],
                    "resource_snapshot": {
                        "sampled_at_utc": stamp,
                        "available_memory_bytes": 16 * fleet.GIB,
                        "root_disk_available_bytes": 32 * fleet.GIB,
                    },
                },
            }
            for name in fleet.DEFAULT_HOSTS
        }

    def report(self, destination="ilo", **options):
        return fleet.evaluate_fleet(
            self.samples, destination=destination, requested_jobs=2,
            now=self.now, max_age_seconds=30, **options,
        )

    def resources(self, name):
        return self.samples[name]["census"]["resource_snapshot"]

    def groups(self, name, count):
        self.samples[name]["census"]["compiler_groups"] = [
            {"pgid": number + 1, "kind": "orphan-rustc", "classification": "orphan-rustc",
             "pids": [number + 1], "cargo_pids": [],
             "requested_cargo_jobs": None, "job_limit_known": False,
             "admission_slot_count": 1,
             "admission_slot_provenance": {
                 "cargo_root_pids": [], "residual_slot_count": 1,
                 "residual_member_pids": [number + 1],
                 "residual_reason": "non-cargo-process-group",
             }}
            for number in range(count)
        ]

    def test_default_requires_memory_floor_on_every_host(self):
        self.resources("local")["available_memory_bytes"] = 4 * fleet.GIB
        report = self.report()
        self.assertFalse(report["advisory_allowed"])
        self.assertEqual(report["memory_guard_scope"], "every-host")
        self.assertIn("local:available-memory-below-8-gib-or-missing", report["reasons"])

    def test_authorized_remote_memory_scope_is_explicit_and_does_not_reserve(self):
        self.resources("local")["available_memory_bytes"] = 4 * fleet.GIB
        report = self.report(destination_memory_only_for_remote=True)
        self.assertTrue(report["advisory_allowed"], report["reasons"])
        self.assertEqual(report["memory_guard_scope"], "destination")
        self.assertIsNone(report["minimum_available_memory_bytes_each_host"])
        self.assertEqual(report["minimum_available_memory_bytes_destination"], 8 * fleet.GIB)
        self.assertEqual(
            {name: host["memory_floor_applies"] for name, host in report["hosts"].items()},
            {"local": False, "ilo": True, "h16001mac": False},
        )
        self.assertIn("local:below-memory-floor-destination-only-admission-authorized", report["limitations"])
        self.assertFalse(report["slot_reserved"])
        self.assertTrue(report["managed_host_permit_required"])
        self.assertEqual(report["processes_signaled"], [])

    def test_remote_flag_cannot_weaken_local_admission(self):
        self.resources("h16001mac")["available_memory_bytes"] = 4 * fleet.GIB
        report = self.report(destination="local", destination_memory_only_for_remote=True)
        self.assertFalse(report["advisory_allowed"])
        self.assertEqual(report["memory_guard_scope"], "every-host")
        self.assertFalse(report["destination_memory_only_for_remote"])

    def test_destination_memory_floor_is_retained_for_both_remotes(self):
        for destination in ("ilo", "h16001mac"):
            with self.subTest(destination=destination):
                self.resources(destination)["available_memory_bytes"] = 8 * fleet.GIB - 1
                report = self.report(destination, destination_memory_only_for_remote=True)
                self.assertFalse(report["advisory_allowed"])
                self.assertIn(destination + ":available-memory-below-8-gib-or-missing", report["reasons"])
                self.resources(destination)["available_memory_bytes"] = 16 * fleet.GIB

    def test_all_hosts_still_require_numeric_nonnegative_memory_samples(self):
        for value in (None, -1, True, "8589934592"):
            with self.subTest(value=value):
                self.resources("local")["available_memory_bytes"] = value
                report = self.report(destination_memory_only_for_remote=True)
                self.assertFalse(report["advisory_allowed"])
                self.assertIn("local:available-memory-invalid-or-missing", report["reasons"])

    def test_all_hosts_still_require_complete_fresh_snapshots(self):
        original = copy.deepcopy(self.samples)
        mutations = (
            lambda sample: sample.update(transport_complete=False),
            lambda sample: sample["census"].update(entries_omitted_count=1),
            lambda sample: sample["census"].update(compiler_groups_omitted_count=1),
            lambda sample: sample["census"].update(resource_snapshot_complete=False),
            lambda sample: sample["census"].update(
                sample_started_at_utc=(self.now - dt.timedelta(seconds=31)).isoformat()
            ),
        )
        for index, mutate in enumerate(mutations):
            with self.subTest(mutation=index):
                self.samples = copy.deepcopy(original)
                mutate(self.samples["local"])
                self.assertFalse(self.report(destination_memory_only_for_remote=True)["advisory_allowed"])
        self.samples = copy.deepcopy(original)
        del self.samples["local"]
        self.assertFalse(self.report(destination_memory_only_for_remote=True)["advisory_allowed"])

    def test_destination_disk_floor_is_retained(self):
        self.resources("ilo")["root_disk_available_bytes"] = 16 * fleet.GIB - 1
        report = self.report(destination_memory_only_for_remote=True)
        self.assertFalse(report["advisory_allowed"])
        self.assertIn("destination-root-disk-below-16-gib-or-missing", report["reasons"])

    def test_destination_preserves_ci_disk_reserve(self):
        self.resources("ilo")["root_disk_reserved_bytes"] = 180 * fleet.GIB
        self.resources("ilo")["root_disk_available_bytes"] = 179 * fleet.GIB
        report = self.report(destination_memory_only_for_remote=True)
        self.assertFalse(report["advisory_allowed"])
        self.assertIn("destination-root-disk-below-ci-reserve", report["reasons"])
        self.resources("ilo")["root_disk_available_bytes"] = 180 * fleet.GIB
        self.assertTrue(self.report(destination_memory_only_for_remote=True)["advisory_allowed"])

    def test_fleet_capacity_includes_the_proposed_workload(self):
        self.groups("local", 5)
        self.groups("ilo", 7)
        self.groups("h16001mac", 4)
        report = self.report(destination_memory_only_for_remote=True)
        self.assertFalse(report["advisory_allowed"])
        self.assertIn("fleet-compiler-admission-slot-limit-reached", report["reasons"])

    def test_remote_cap_distinguishes_full_other_host_from_destination(self):
        self.groups("h16001mac", 8)
        self.assertTrue(self.report(destination_memory_only_for_remote=True)["advisory_allowed"])
        report = self.report("h16001mac", destination_memory_only_for_remote=True)
        self.assertFalse(report["advisory_allowed"])
        self.assertIn("h16001mac:host-compiler-admission-slot-limit-reached", report["reasons"])

    def test_local_cap_waiver_requires_its_separate_authorization(self):
        self.groups("local", 6)
        self.resources("local")["available_memory_bytes"] = 4 * fleet.GIB
        self.assertFalse(self.report(destination_memory_only_for_remote=True)["advisory_allowed"])
        report = self.report(destination_memory_only_for_remote=True,
                             allow_local_over_cap_for_remote=True)
        self.assertTrue(report["advisory_allowed"], report["reasons"])
        self.assertIn("local:over-host-cap-remote-admission-authorized", report["limitations"])

    def test_existing_cargo_job_limit_is_retained(self):
        self.samples["h16001mac"]["census"]["compiler_groups"] = [{
            "pgid": 1, "kind": "cargo", "classification": "build",
            "pids": [2], "cargo_pids": [2],
            "requested_cargo_jobs": 5, "job_limit_known": True,
            "admission_slot_count": 1,
            "admission_slot_provenance": {
                "cargo_root_pids": [2], "residual_slot_count": 0,
                "residual_member_pids": [], "residual_reason": None,
            },
        }]
        self.samples["h16001mac"]["census"]["entries"] = [{
            "pid": 2, "pgid": 1, "classification": "build",
            "identity_validated": True, "start_token": "test:2:start",
            "requested_cargo_jobs": 5,
        }]
        report = self.report(destination_memory_only_for_remote=True)
        self.assertFalse(report["advisory_allowed"])
        self.assertIn("h16001mac:active-cargo-process-jobs-exceed-4", report["reasons"])

    def test_explicit_mac_exclusion_is_reported_as_partial_scope(self):
        hosts = {name: fleet.DEFAULT_HOSTS[name] for name in ("local", "ilo")}
        samples = {name: self.samples[name] for name in hosts}
        report = fleet.evaluate_fleet(
            samples, destination="ilo", requested_jobs=1, hosts=hosts,
            excluded_hosts={"h16001mac": "approved temporary offline exception"},
            now=self.now, destination_memory_only_for_remote=True,
        )
        self.assertTrue(report["advisory_allowed"], report["reasons"])
        self.assertEqual(report["sample_scope"]["requested_hosts"], ["local", "ilo"])
        self.assertEqual(report["sample_scope"]["sampled_hosts"], ["local", "ilo"])
        self.assertEqual(report["sample_scope"]["excluded_hosts"], [{
            "host": "h16001mac", "reason": "approved temporary offline exception",
        }])
        self.assertFalse(report["sample_scope"]["full_fleet_sampled"])
        self.assertNotIn("h16001mac", report["hosts"])

    def test_cli_scope_resolver_requires_the_one_explicit_approved_exclusion(self):
        active, excluded = fleet._resolve_sample_scope(
            destination="ilo", exclude_hosts=["h16001mac"],
            exclusion_reason="offline evidence", hosts=fleet.DEFAULT_HOSTS,
        )
        self.assertEqual(set(active), {"local", "ilo"})
        self.assertEqual(excluded, {"h16001mac": "offline evidence"})

        invalid_requests = (
            ("local", ["h16001mac"], "offline evidence"),
            ("h16001mac", ["h16001mac"], "offline evidence"),
            ("ilo", ["ilo"], "offline evidence"),
            ("ilo", ["h16001mac", "h16001mac"], "offline evidence"),
            ("ilo", ["h16001mac"], None),
            ("ilo", ["h16001mac"], "  "),
            ("ilo", ["h16001mac"], "x" * 501),
            ("ilo", ["h16001mac"], 42),
            ("ilo", [], "reason without exclusion"),
        )
        for destination, excluded_hosts, reason in invalid_requests:
            with self.subTest(destination=destination, excluded_hosts=excluded_hosts, reason=reason):
                with self.assertRaises(ValueError):
                    fleet._resolve_sample_scope(
                        destination=destination, exclude_hosts=excluded_hosts,
                        exclusion_reason=reason, hosts=fleet.DEFAULT_HOSTS,
                    )

    def test_missing_or_colliding_excluded_samples_refuse_without_false_sampling(self):
        partial_hosts = {name: fleet.DEFAULT_HOSTS[name] for name in ("local", "ilo")}
        excluded = {"h16001mac": "approved temporary offline exception"}
        collision = fleet.evaluate_fleet(
            self.samples, destination="ilo", requested_jobs=1, hosts=fleet.DEFAULT_HOSTS,
            excluded_hosts=excluded, now=self.now,
            destination_memory_only_for_remote=True,
        )
        self.assertFalse(collision["advisory_allowed"])
        self.assertIn("fleet-sample-scope-invalid", collision["reasons"])

        missing_live_host = fleet.evaluate_fleet(
            {"ilo": self.samples["ilo"]}, destination="ilo", requested_jobs=1,
            hosts=partial_hosts, excluded_hosts=excluded, now=self.now,
            destination_memory_only_for_remote=True,
        )
        self.assertFalse(missing_live_host["advisory_allowed"])
        self.assertIn("fleet-host-set-incomplete-or-unexpected", missing_live_host["reasons"])
        self.assertNotIn("h16001mac", missing_live_host["sample_scope"]["sampled_hosts"])

        no_exclusion_reason = fleet.evaluate_fleet(
            {name: self.samples[name] for name in partial_hosts},
            destination="ilo", requested_jobs=1, hosts=partial_hosts, now=self.now,
            destination_memory_only_for_remote=True,
        )
        self.assertFalse(no_exclusion_reason["advisory_allowed"])
        self.assertIn("fleet-sample-scope-invalid", no_exclusion_reason["reasons"])

    def test_invalid_nonstring_exclusion_reason_fails_and_remains_json_safe(self):
        hosts = {name: fleet.DEFAULT_HOSTS[name] for name in ("local", "ilo")}
        samples = {name: self.samples[name] for name in hosts}
        report = fleet.evaluate_fleet(
            samples, destination="ilo", requested_jobs=1, hosts=hosts,
            excluded_hosts={"h16001mac": 42}, now=self.now,
            destination_memory_only_for_remote=True,
        )
        self.assertFalse(report["advisory_allowed"])
        self.assertIn("h16001mac:host-exclusion-reason-invalid", report["reasons"])
        self.assertIsNone(report["sample_scope"]["excluded_hosts"][0]["reason"])
        json.dumps(report)


if __name__ == "__main__":
    unittest.main()
