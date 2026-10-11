from __future__ import annotations

import datetime as dt
import importlib.util
import os
import pathlib
import subprocess
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


def compiler_group(
    pgid: int,
    *,
    kind: str = "orphan-rustc",
    classification: str = "orphan-rustc",
    jobs: int | None = None,
    cargo_pids: list[int] | None = None,
    pids: list[int] | None = None,
    residual_member_pids: list[int] | None = None,
) -> dict[str, object]:
    cargo_pids = cargo_pids if cargo_pids is not None else ([pgid] if kind == "cargo" else [])
    pids = pids if pids is not None else sorted(set([pgid, *cargo_pids]))
    residual_member_pids = residual_member_pids if residual_member_pids is not None else ([] if kind == "cargo" else [pgid])
    residual_slots = 1 if kind != "cargo" or residual_member_pids else 0
    slot_count = len(cargo_pids) + residual_slots if kind == "cargo" else 1
    return {
        "pgid": pgid,
        "kind": kind,
        "classification": classification,
        "pids": pids,
        "cargo_pids": cargo_pids,
        "rustc_process_count": 1,
        "requested_cargo_jobs": jobs,
        "job_limit_known": jobs is not None,
        "admission_slot_count": slot_count,
        "admission_slot_provenance": {
            "cargo_root_pids": sorted(cargo_pids),
            "residual_slot_count": residual_slots,
            "residual_member_pids": residual_member_pids,
            "residual_reason": (
                "unattributed-active-members" if kind == "cargo" and residual_slots
                else "non-cargo-process-group" if kind != "cargo"
                else None
            ),
        },
    }


def cargo_entry(pid: int, pgid: int, jobs: int | None, *, classification: str = "build") -> dict[str, object]:
    return {
        "pid": pid,
        "pgid": pgid,
        "classification": classification,
        "identity_validated": True,
        "start_token": f"test:{pid}:start",
        "requested_cargo_jobs": jobs,
    }


def install_groups(
    samples: dict[str, dict[str, object]],
    host: str,
    groups: list[dict[str, object]],
    entries: list[dict[str, object]] | None = None,
) -> None:
    census = samples[host]["census"]
    assert isinstance(census, dict)
    census["compiler_groups"] = groups
    census["entries"] = entries if entries is not None else []


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

    def evaluate_ilo_scope(
        self,
        *,
        hosts: dict[str, dict[str, object]] | None = None,
        samples: dict[str, dict[str, object]] | None = None,
        excluded_hosts: object = None,
    ) -> dict[str, object]:
        active_hosts = hosts or {name: fleet.DEFAULT_HOSTS[name] for name in ("local", "ilo")}
        active_samples = samples or {name: self.samples[name] for name in active_hosts}
        if excluded_hosts is None:
            excluded_hosts = {"h16001mac": "approved temporary offline exception"}
        return fleet.evaluate_fleet(
            active_samples,
            destination="ilo",
            requested_jobs=1,
            hosts=active_hosts,
            now=self.now,
            allow_local_over_cap_for_remote=True,
            destination_memory_only_for_remote=True,
            excluded_hosts=excluded_hosts,
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
        self.assertFalse(result["sample_scope"]["full_fleet_sampled"])

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
        install_groups(self.samples, "local", [compiler_group(100 + index) for index in range(5)])
        self.assertFalse(self.evaluate("local")["advisory_allowed"])

        self.samples = empty_samples(self.now)
        for host_index, (host, count) in enumerate((("local", 4), ("ilo", 6), ("h16001mac", 6)), start=1):
            install_groups(self.samples, host, [
                compiler_group(1000 * host_index + offset)
                for offset in range(count)
            ])
        result = self.evaluate("local")
        self.assertEqual(result["current_fleet_compiler_group_count"], 16)
        self.assertEqual(result["current_fleet_compiler_admission_slot_count"], 16)
        self.assertFalse(result["advisory_allowed"])

    def test_unknown_executable_groups_are_valid_and_count_toward_capacity(self) -> None:
        install_groups(
            self.samples,
            "local",
            [compiler_group(8000 + index, kind="unknown", classification="unknown") for index in range(5)],
        )

        result = self.evaluate("local")

        self.assertEqual(result["current_fleet_compiler_group_count"], 5)
        self.assertFalse(result["advisory_allowed"])
        self.assertIn("local:host-compiler-admission-slot-limit-reached", result["reasons"])
        self.assertIn("local:unresolved-compiler-group-counted-conservatively", result["limitations"])

    def test_unknown_processes_count_conservatively_and_known_overlarge_jobs_refuse(self) -> None:
        install_groups(
            self.samples,
            "ilo",
            [compiler_group(2200, kind="cargo", classification="unknown")],
            [cargo_entry(2200, 2200, None, classification="unknown")],
        )
        result = self.evaluate("h16001mac")
        self.assertTrue(result["advisory_allowed"])
        self.assertEqual(result["current_fleet_compiler_group_count"], 1)
        self.assertTrue(any("counted-conservatively" in item for item in result["limitations"]))

        self.samples = empty_samples(self.now)
        install_groups(
            self.samples,
            "ilo",
            [compiler_group(2201, kind="cargo", classification="build", jobs=5)],
            [cargo_entry(2201, 2201, 5)],
        )
        self.assertFalse(self.evaluate("h16001mac")["advisory_allowed"])

        self.samples = empty_samples(self.now)
        install_groups(
            self.samples,
            "h16001mac",
            [compiler_group(2202, kind="runtime-owner", classification="runtime-owner")],
        )
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

    def test_two_sibling_cargo_checks_jobs_four_each_use_two_slots_and_are_allowed(self) -> None:
        group = compiler_group(
            9000,
            kind="cargo",
            classification="build",
            jobs=8,
            cargo_pids=[9001, 9002],
            pids=[9001, 9002],
        )
        install_groups(
            self.samples,
            "ilo",
            [group],
            [cargo_entry(9001, 9000, 4), cargo_entry(9002, 9000, 4)],
        )

        result = self.evaluate("h16001mac")

        self.assertTrue(result["advisory_allowed"], result["reasons"])
        self.assertEqual(result["hosts"]["ilo"]["compiler_group_count"], 1)
        self.assertEqual(result["hosts"]["ilo"]["compiler_admission_slot_count"], 2)
        self.assertEqual(result["current_fleet_compiler_group_count"], 1)
        self.assertEqual(result["current_fleet_compiler_admission_slot_count"], 2)
        self.assertEqual(result["hosts"]["ilo"]["compiler_groups"][0]["requested_cargo_jobs"], 8)
        self.assertIn("compatibility aliases only", result["capacity_compatibility_note"])

    def test_per_process_jobs_cap_refuses_four_plus_five_siblings(self) -> None:
        group = compiler_group(
            9100,
            kind="cargo",
            classification="build",
            jobs=9,
            cargo_pids=[9101, 9102],
            pids=[9101, 9102],
        )
        install_groups(
            self.samples,
            "ilo",
            [group],
            [cargo_entry(9101, 9100, 4), cargo_entry(9102, 9100, 5)],
        )

        result = self.evaluate("h16001mac")

        self.assertFalse(result["advisory_allowed"])
        self.assertIn("ilo:active-cargo-process-jobs-exceed-4", result["reasons"])

    def test_sibling_slots_count_toward_host_and_fleet_limits(self) -> None:
        sibling_group = compiler_group(
            9200,
            kind="cargo",
            classification="build",
            jobs=8,
            cargo_pids=[9201, 9202],
            pids=[9201, 9202],
        )
        sibling_rows = [cargo_entry(9201, 9200, 4), cargo_entry(9202, 9200, 4)]

        # Five local slots already fill the local host cap; the two Cargo
        # roots count separately even though they share one PGID.
        install_groups(
            self.samples,
            "local",
            [sibling_group, *[compiler_group(9210 + index) for index in range(3)]],
            sibling_rows,
        )
        local = self.evaluate("local")
        self.assertFalse(local["advisory_allowed"])
        self.assertEqual(local["hosts"]["local"]["compiler_group_count"], 4)
        self.assertEqual(local["hosts"]["local"]["compiler_admission_slot_count"], 5)
        self.assertLessEqual(
            local["hosts"]["local"]["compiler_group_count"] + 1,
            local["hosts"]["local"]["host_limit"],
        )
        self.assertIn("local:host-compiler-admission-slot-limit-reached", local["reasons"])

        # ILO 8 + local 5 + Mac 3 = 16 occupied slots. The proposed Mac slot
        # must be refused at the fleet limit, even though ILO's siblings are
        # represented by a single raw PGID group.
        self.samples = empty_samples(self.now)
        install_groups(
            self.samples,
            "ilo",
            [sibling_group, *[compiler_group(9220 + index) for index in range(6)]],
            sibling_rows,
        )
        install_groups(self.samples, "local", [compiler_group(9300 + index) for index in range(5)])
        install_groups(self.samples, "h16001mac", [compiler_group(9400 + index) for index in range(3)])
        fleet_full = self.evaluate("h16001mac")
        self.assertEqual(fleet_full["current_fleet_compiler_group_count"], 15)
        self.assertEqual(fleet_full["current_fleet_compiler_admission_slot_count"], 16)
        self.assertLessEqual(
            fleet_full["current_fleet_compiler_group_count"] + 1,
            fleet_full["fleet_compiler_group_limit"],
        )
        self.assertFalse(fleet_full["advisory_allowed"])
        self.assertIn("fleet-compiler-admission-slot-limit-reached", fleet_full["reasons"])

        # One fewer existing slot permits the proposed slot exactly at 16.
        install_groups(self.samples, "h16001mac", [compiler_group(9400 + index) for index in range(2)])
        exactly_full = self.evaluate("h16001mac")
        self.assertEqual(exactly_full["current_fleet_compiler_admission_slot_count"], 15)
        self.assertTrue(exactly_full["advisory_allowed"], exactly_full["reasons"])

    def test_missing_and_duplicate_cargo_entry_joins_refuse(self) -> None:
        group = compiler_group(
            9500,
            kind="cargo",
            classification="build",
            jobs=4,
            cargo_pids=[9501],
            pids=[9501],
        )
        install_groups(self.samples, "ilo", [group], [])
        missing = self.evaluate("h16001mac")
        self.assertFalse(missing["advisory_allowed"])
        self.assertIn("ilo:cargo-entry-pid-join-missing-or-ambiguous", missing["reasons"])

        duplicate = cargo_entry(9501, 9500, 4)
        install_groups(self.samples, "ilo", [group], [duplicate, dict(duplicate)])
        ambiguous = self.evaluate("h16001mac")
        self.assertFalse(ambiguous["advisory_allowed"])
        self.assertIn("ilo:cargo-entry-pid-join-ambiguous", ambiguous["reasons"])

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
        install_groups(self.samples, "local", [compiler_group(3000 + index) for index in range(9)])

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

    def test_offline_mac_is_excluded_only_by_explicit_reported_ilo_scope(self) -> None:
        result = self.evaluate_ilo_scope()
        self.assertTrue(result["advisory_allowed"], result["reasons"])
        self.assertEqual(result["sample_scope"]["requested_hosts"], ["local", "ilo"])
        self.assertEqual(result["sample_scope"]["sampled_hosts"], ["local", "ilo"])
        self.assertEqual(result["sample_scope"]["excluded_hosts"], [{
            "host": "h16001mac", "reason": "approved temporary offline exception",
        }])
        self.assertFalse(result["sample_scope"]["full_fleet_sampled"])
        self.assertTrue(result["sample_scope"]["fleet_limit_applies_across_sampled_hosts"])
        self.assertNotIn("h16001mac", result["hosts"])

        full = self.evaluate()
        self.assertTrue(full["sample_scope"]["full_fleet_sampled"])
        self.assertEqual(set(full["sample_scope"]["sampled_hosts"]), set(fleet.DEFAULT_HOSTS))

    def test_exclusion_resolver_rejects_invalid_host_destination_and_reason(self) -> None:
        invalid = (
            ("local", ["h16001mac"], "offline"),
            ("h16001mac", ["h16001mac"], "offline"),
            ("ilo", ["ilo"], "offline"),
            ("ilo", ["h16001mac", "h16001mac"], "offline"),
            ("ilo", ["h16001mac"], None),
            ("ilo", ["h16001mac"], ""),
            ("ilo", ["h16001mac"], " \t"),
            ("ilo", ["h16001mac"], "x" * 501),
            ("ilo", ["h16001mac"], 9),
            ("ilo", [], "reason without an exclusion"),
        )
        for destination, excluded, reason in invalid:
            with self.subTest(destination=destination, excluded=excluded, reason=reason):
                with self.assertRaises(ValueError):
                    fleet._resolve_sample_scope(
                        destination=destination,
                        exclude_hosts=excluded,
                        exclusion_reason=reason,
                    )

        active, omitted = fleet._resolve_sample_scope(
            destination="ilo", exclude_hosts=["h16001mac"],
            exclusion_reason="fresh offline evidence",
        )
        self.assertEqual(set(active), {"local", "ilo"})
        self.assertEqual(omitted, {"h16001mac": "fresh offline evidence"})

    def test_scope_requires_noncolliding_samples_for_every_included_host(self) -> None:
        full_hosts = dict(fleet.DEFAULT_HOSTS)
        collision = self.evaluate_ilo_scope(hosts=full_hosts)
        self.assertFalse(collision["advisory_allowed"])
        self.assertIn("fleet-sample-scope-invalid", collision["reasons"])

        active_hosts = {name: fleet.DEFAULT_HOSTS[name] for name in ("local", "ilo")}
        missing_sample = self.evaluate_ilo_scope(
            hosts=active_hosts,
            samples={"ilo": self.samples["ilo"]},
        )
        self.assertFalse(missing_sample["advisory_allowed"])
        self.assertIn("fleet-host-set-incomplete-or-unexpected", missing_sample["reasons"])
        self.assertNotIn("h16001mac", missing_sample["sample_scope"]["sampled_hosts"])

        missing_exclusion = self.evaluate_ilo_scope(
            hosts=active_hosts,
            samples={name: self.samples[name] for name in active_hosts},
            excluded_hosts={},
        )
        self.assertFalse(missing_exclusion["advisory_allowed"])
        self.assertIn("fleet-sample-scope-invalid", missing_exclusion["reasons"])

        unexpected_sample = fleet.evaluate_fleet(
            {**self.samples, "unexpected": self.samples["local"]},
            destination="ilo", requested_jobs=1, hosts=fleet.DEFAULT_HOSTS,
            now=self.now,
        )
        self.assertFalse(unexpected_sample["advisory_allowed"])
        self.assertFalse(unexpected_sample["sample_scope"]["full_fleet_sampled"])

    def test_scope_refuses_stale_incomplete_and_nonnumeric_live_samples(self) -> None:
        mutations = (
            lambda sample: sample.update(transport_complete=False),
            lambda sample: sample["census"].update(complete=False),
            lambda sample: sample["census"].update(resource_snapshot_complete=False),
            lambda sample: sample["census"].update(compiler_groups_omitted_count=1),
            lambda sample: sample["census"].update(
                sample_started_at_utc=(self.now - dt.timedelta(seconds=61)).isoformat()
            ),
        )
        for host in ("local", "ilo"):
            for index, mutate in enumerate(mutations):
                with self.subTest(host=host, mutation=index):
                    self.samples = empty_samples(self.now)
                    mutate(self.samples[host])
                    self.assertFalse(self.evaluate_ilo_scope()["advisory_allowed"])

        for value in (None, -1, True, "8589934592"):
            with self.subTest(memory=value):
                self.samples = empty_samples(self.now)
                self.samples["local"]["census"]["resource_snapshot"]["available_memory_bytes"] = value
                report = self.evaluate_ilo_scope()
                self.assertFalse(report["advisory_allowed"])
                self.assertIn("local:available-memory-invalid-or-missing", report["reasons"])

        for value in (None, -1, True, "17179869184", 16 * fleet.GIB - 1):
            with self.subTest(destination_disk=value):
                self.samples = empty_samples(self.now)
                self.samples["ilo"]["census"]["resource_snapshot"]["root_disk_available_bytes"] = value
                self.assertFalse(self.evaluate_ilo_scope()["advisory_allowed"])

    def test_scoped_memory_disk_host_and_fleet_reservations_keep_exact_floors(self) -> None:
        self.samples = empty_samples(self.now)
        self.samples["ilo"]["census"]["resource_snapshot"]["available_memory_bytes"] = fleet.MIN_AVAILABLE_MEMORY - 1
        below_memory = self.evaluate_ilo_scope()
        self.assertFalse(below_memory["advisory_allowed"])
        self.assertIn("ilo:available-memory-below-8-gib-or-missing", below_memory["reasons"])

        self.samples = empty_samples(self.now)
        self.samples["ilo"]["census"]["resource_snapshot"]["root_disk_available_bytes"] = fleet.MIN_DESTINATION_DISK - 1
        below_disk = self.evaluate_ilo_scope()
        self.assertFalse(below_disk["advisory_allowed"])
        self.assertIn("destination-root-disk-below-16-gib-or-missing", below_disk["reasons"])

        # Eight occupied ILO slots leave no room for the proposed ninth slot.
        self.samples = empty_samples(self.now)
        scoped = {name: self.samples[name] for name in ("local", "ilo")}
        install_groups(scoped, "ilo", [compiler_group(6000 + i) for i in range(8)])
        destination_full = self.evaluate_ilo_scope(samples=scoped)
        self.assertFalse(destination_full["advisory_allowed"])
        self.assertIn("ilo:host-compiler-admission-slot-limit-reached", destination_full["reasons"])

        # Fifteen reachable current slots plus one proposed slot is exactly 16;
        # ILO has seven current slots plus the proposed slot, exactly its cap.
        self.samples = empty_samples(self.now)
        scoped = {name: self.samples[name] for name in ("local", "ilo")}
        install_groups(scoped, "local", [compiler_group(6100 + i) for i in range(8)])
        install_groups(scoped, "ilo", [compiler_group(6200 + i) for i in range(7)])
        exactly_full = self.evaluate_ilo_scope(samples=scoped)
        self.assertEqual(exactly_full["current_fleet_compiler_admission_slot_count"], 15)
        self.assertTrue(exactly_full["advisory_allowed"], exactly_full["reasons"])

        install_groups(scoped, "ilo", [compiler_group(6300 + i) for i in range(8)])
        over_fleet_and_destination = self.evaluate_ilo_scope(samples=scoped)
        self.assertFalse(over_fleet_and_destination["advisory_allowed"])
        self.assertIn("fleet-compiler-admission-slot-limit-reached", over_fleet_and_destination["reasons"])
        self.assertIn("ilo:host-compiler-admission-slot-limit-reached", over_fleet_and_destination["reasons"])

    def test_recovery_entry_accepts_only_a_validated_cpython(self) -> None:
        launcher = SCRIPT.with_name("fleet-census-recovery")
        good_env = {**os.environ, "NUDOX_FLEET_CPYTHON": sys.executable}
        good = subprocess.run(
            [str(launcher), "--help"], check=False, capture_output=True, text=True,
            env=good_env, timeout=10,
        )
        self.assertEqual(good.returncode, 0, good.stderr)
        self.assertIn("--exclude-host", good.stdout)

        bad_env = {**os.environ, "NUDOX_FLEET_CPYTHON": "/bin/sh"}
        bad = subprocess.run(
            [str(launcher), "--help"], check=False, capture_output=True, text=True,
            env=bad_env, timeout=10,
        )
        self.assertEqual(bad.returncode, 78)
        self.assertIn("requires CPython >= 3.10", bad.stderr)


if __name__ == "__main__":
    unittest.main()
