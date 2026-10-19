from __future__ import annotations

import datetime as dt
import contextlib
import io
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import unittest
from collections.abc import Iterable, Mapping
from collections import Counter
from unittest import mock


SCRIPT = pathlib.Path(__file__).parents[1] / ".config/scripts/fleet-capacity-census.py"
SPEC = importlib.util.spec_from_file_location("fleet_capacity_census", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
fleet = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = fleet
SPEC.loader.exec_module(fleet)

BUILD_SCRIPT = pathlib.Path(__file__).parents[1] / ".config/scripts/build-census.py"
BUILD_SPEC = importlib.util.spec_from_file_location("build_census_for_fleet_test", BUILD_SCRIPT)
assert BUILD_SPEC is not None and BUILD_SPEC.loader is not None
build_census = importlib.util.module_from_spec(BUILD_SPEC)
sys.modules[BUILD_SPEC.name] = build_census
BUILD_SPEC.loader.exec_module(build_census)


def empty_samples_for_hosts(
    now: dt.datetime,
    hosts: Iterable[str] | Mapping[str, object],
) -> dict[str, dict[str, object]]:
    stamp = (now - dt.timedelta(seconds=1)).isoformat()
    result: dict[str, dict[str, object]] = {}
    for name in hosts:
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


def empty_samples(now: dt.datetime) -> dict[str, dict[str, object]]:
    return empty_samples_for_hosts(now, fleet.DEFAULT_HOSTS)


def host_configs(*, include_ilo_prod: bool = False) -> dict[str, dict[str, object]]:
    configured = dict(fleet.DEFAULT_HOSTS)
    if include_ilo_prod:
        configured.update(fleet.OPTIONAL_HOSTS)
    return {name: dict(config) for name, config in configured.items()}


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
    rustc_member_pids = sorted(pids) if kind == "orphan-rustc" else []
    rustc_count = len(rustc_member_pids)
    residual_slots = (
        1 if residual_member_pids else 0
    ) if kind == "cargo" else max(1, rustc_count) + int(bool(rustc_count) and len(pids) > rustc_count)
    slot_count = len(cargo_pids) + residual_slots if kind == "cargo" else residual_slots
    return {
        "pgid": pgid,
        "kind": kind,
        "classification": classification,
        "pids": pids,
        "cargo_pids": cargo_pids,
        "rustc_process_count": rustc_count,
        "requested_cargo_jobs": jobs,
        "job_limit_known": jobs is not None,
        "admission_slot_count": slot_count,
        "admission_slot_provenance": {
            "cargo_root_pids": sorted(cargo_pids),
            "residual_slot_count": residual_slots,
            "residual_member_pids": residual_member_pids,
            "rustc_member_pids": rustc_member_pids,
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

    def evaluate_prod_scope(
        self,
        destination: str = "local",
        *,
        hosts: dict[str, dict[str, object]] | None = None,
        samples: dict[str, dict[str, object]] | None = None,
        include_ilo_prod: bool = True,
        excluded_hosts: object = None,
        **options: object,
    ) -> dict[str, object]:
        active_hosts = hosts if hosts is not None else host_configs(include_ilo_prod=True)
        active_samples = samples if samples is not None else empty_samples_for_hosts(self.now, active_hosts)
        return fleet.evaluate_fleet(
            active_samples,
            destination=destination,
            requested_jobs=4,
            hosts=active_hosts,
            now=self.now,
            include_ilo_prod=include_ilo_prod,
            excluded_hosts=excluded_hosts,
            **options,
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

    def test_remote_local_cap_exception_keeps_local_active_cargo_job_limit(self) -> None:
        local_orphans = [compiler_group(7000 + index) for index in range(6)]

        def install_local_cargo(jobs: int) -> None:
            cargo = compiler_group(
                7100,
                kind="cargo",
                classification="build",
                jobs=jobs,
                cargo_pids=[7101],
                pids=[7101],
            )
            install_groups(
                self.samples,
                "local",
                [*local_orphans, cargo],
                [cargo_entry(7101, 7100, jobs)],
            )

        install_local_cargo(4)
        allowed = self.evaluate_ilo_scope()
        self.assertTrue(allowed["advisory_allowed"], allowed["reasons"])
        self.assertTrue(allowed["allow_local_over_cap_for_remote"])
        self.assertTrue(allowed["destination_memory_only_for_remote"])
        self.assertGreater(
            allowed["hosts"]["local"]["compiler_admission_slot_count"],
            allowed["hosts"]["local"]["host_admission_slot_limit"],
        )
        self.assertIn(
            "local:over-host-cap-remote-admission-authorized",
            allowed["limitations"],
        )

        install_local_cargo(6)
        refused = self.evaluate_ilo_scope()
        self.assertFalse(refused["advisory_allowed"])
        self.assertTrue(refused["allow_local_over_cap_for_remote"])
        self.assertTrue(refused["destination_memory_only_for_remote"])
        self.assertIn("local:active-cargo-process-jobs-exceed-4", refused["reasons"])
        self.assertIn(
            "local:over-host-cap-remote-admission-authorized",
            refused["limitations"],
        )

    def test_offline_mac_is_excluded_only_by_explicit_reported_ilo_scope(self) -> None:
        result = self.evaluate_ilo_scope()
        self.assertTrue(result["advisory_allowed"], result["reasons"])
        self.assertEqual(result["sample_scope"]["requested_hosts"], ["local", "ilo"])
        self.assertEqual(result["sample_scope"]["sampled_hosts"], ["local", "ilo"])
        self.assertEqual(result["sample_scope"]["excluded_hosts"], [{
            "host": "h16001mac", "reason": "approved temporary offline exception",
        }])
        self.assertFalse(result["sample_scope"]["full_fleet_sampled"])
        self.assertIsNone(result["sample_scope"]["exclusion_authorization"])
        self.assertTrue(result["sample_scope"]["fleet_limit_applies_across_sampled_hosts"])
        self.assertNotIn("h16001mac", result["hosts"])

        full = self.evaluate()
        self.assertTrue(full["sample_scope"]["full_fleet_sampled"])
        self.assertEqual(set(full["sample_scope"]["sampled_hosts"]), set(fleet.DEFAULT_HOSTS))

    def test_optional_ilo_prod_is_an_explicit_fourth_host(self) -> None:
        self.assertEqual(set(fleet.DEFAULT_HOSTS), {"local", "ilo", "h16001mac"})
        self.assertEqual(fleet.OPTIONAL_HOSTS["ilo-prod"]["limit"], 4)
        self.assertEqual(fleet.OPTIONAL_HOSTS["ilo-prod"]["ssh_target"], "ilo-prod")
        self.assertEqual(
            fleet.OPTIONAL_HOSTS["ilo-prod"]["python"],
            "/nix/store/jxyrvv4gbpnp3ap5iy7wxwl1sg4x2x88-python3-3.14.6/bin/python3",
        )

        hosts = host_configs(include_ilo_prod=True)
        samples = empty_samples_for_hosts(self.now, hosts)
        result = self.evaluate_prod_scope(hosts=hosts, samples=samples)

        self.assertTrue(result["advisory_allowed"], result["reasons"])
        self.assertEqual(set(result["sample_scope"]["configured_hosts"]), set(hosts))
        self.assertEqual(result["sample_scope"]["optional_hosts_enabled"], ["ilo-prod"])
        self.assertEqual(set(result["sample_scope"]["sampled_hosts"]), set(hosts))
        self.assertTrue(result["sample_scope"]["full_fleet_sampled"])
        self.assertEqual(result["hosts"]["ilo-prod"]["host_limit"], 4)
        # Older native samplers omit this optional field; absence remains the
        # observed zero-reserve value rather than an invented reserve.
        self.assertEqual(result["hosts"]["ilo-prod"]["root_disk_reserved_bytes"], 0)

        legacy = self.evaluate()
        self.assertTrue(legacy["advisory_allowed"], legacy["reasons"])
        self.assertEqual(set(legacy["sample_scope"]["configured_hosts"]), set(fleet.DEFAULT_HOSTS))
        self.assertEqual(legacy["sample_scope"]["optional_hosts_enabled"], [])
        self.assertEqual(len(legacy["sample_scope"]["configured_hosts"]), 3)

    def test_optional_ilo_prod_requires_opt_in_and_a_complete_four_host_set(self) -> None:
        hosts = host_configs(include_ilo_prod=True)
        four_samples = empty_samples_for_hosts(self.now, hosts)

        without_opt_in = self.evaluate_prod_scope(
            destination="ilo-prod", hosts=hosts, samples=four_samples,
            include_ilo_prod=False,
        )
        self.assertFalse(without_opt_in["advisory_allowed"])
        self.assertIn("fleet-sample-scope-invalid", without_opt_in["reasons"])
        self.assertFalse(without_opt_in["sample_scope"]["full_fleet_sampled"])

        missing_prod = {name: four_samples[name] for name in fleet.DEFAULT_HOSTS}
        incomplete = self.evaluate_prod_scope(hosts=hosts, samples=missing_prod)
        self.assertFalse(incomplete["advisory_allowed"])
        self.assertIn("fleet-host-set-incomplete-or-unexpected", incomplete["reasons"])
        self.assertFalse(incomplete["sample_scope"]["full_fleet_sampled"])

        invalid_policy = self.evaluate_prod_scope(
            hosts=hosts, samples=four_samples, include_ilo_prod=1,  # type: ignore[arg-type]
        )
        self.assertFalse(invalid_policy["advisory_allowed"])
        self.assertIn("fleet-optional-host-policy-invalid", invalid_policy["reasons"])

    def test_completion_counts_require_exact_ints_for_each_included_host(self) -> None:
        hosts = host_configs(include_ilo_prod=True)
        malformed_values = (False, True, 0.0, "0", None, 1)
        fields = ("return_code", "entries_omitted_count", "compiler_groups_omitted_count")

        for host in hosts:
            for field in fields:
                for value in malformed_values:
                    with self.subTest(host=host, field=field, value=value):
                        samples = empty_samples_for_hosts(self.now, hosts)
                        if field == "return_code":
                            samples[host][field] = value
                        else:
                            samples[host]["census"][field] = value
                        result = self.evaluate_prod_scope(hosts=hosts, samples=samples)
                        self.assertFalse(result["advisory_allowed"])
                        self.assertIn(f"{host}:incomplete-or-stale-native-census", result["reasons"])
                        self.assertFalse(result["hosts"][host]["complete"])

        # The stricter evidence types do not change the established default
        # three-host policy for well-formed census output.
        legacy = self.evaluate()
        self.assertTrue(legacy["advisory_allowed"], legacy["reasons"])
        self.assertEqual(set(legacy["sample_scope"]["configured_hosts"]), set(fleet.DEFAULT_HOSTS))

    def test_four_host_scope_rejects_stale_incomplete_truncated_and_invalid_resources(self) -> None:
        hosts = host_configs(include_ilo_prod=True)
        mutations = (
            lambda sample: sample.update(transport_complete=False),
            lambda sample: sample["census"].update(complete=False),
            lambda sample: sample["census"].update(resource_snapshot_complete=False),
            lambda sample: sample["census"].update(entries_omitted_count=1),
            lambda sample: sample["census"].update(compiler_groups_omitted_count=1),
            lambda sample: sample["census"].update(
                sample_finished_at_utc=(self.now - dt.timedelta(seconds=61)).isoformat()
            ),
        )
        for index, mutate in enumerate(mutations):
            with self.subTest(mutation=index):
                samples = empty_samples_for_hosts(self.now, hosts)
                mutate(samples["ilo-prod"])
                result = self.evaluate_prod_scope(hosts=hosts, samples=samples)
                self.assertFalse(result["advisory_allowed"])
                self.assertTrue(any(reason.startswith("ilo-prod:") for reason in result["reasons"]))

        for name in hosts:
            with self.subTest(host=name, field="available_memory_bytes"):
                samples = empty_samples_for_hosts(self.now, hosts)
                samples[name]["census"]["resource_snapshot"]["available_memory_bytes"] = True
                result = self.evaluate_prod_scope(hosts=hosts, samples=samples)
                self.assertFalse(result["advisory_allowed"])
                self.assertIn(f"{name}:available-memory-invalid-or-missing", result["reasons"])

            for value in (None, "17179869184", True, -1):
                with self.subTest(host=name, disk=value):
                    samples = empty_samples_for_hosts(self.now, hosts)
                    samples[name]["census"]["resource_snapshot"]["root_disk_available_bytes"] = value
                    # Destination is ILO: every participant still needs a
                    # numeric disk observation in the four-host witness.
                    result = self.evaluate_prod_scope(
                        destination="ilo", hosts=hosts, samples=samples,
                    )
                    self.assertFalse(result["advisory_allowed"])
                    self.assertIn(f"{name}:root-disk-invalid-or-missing", result["reasons"])

            for value in (True, -1):
                with self.subTest(host=name, reserve=value):
                    samples = empty_samples_for_hosts(self.now, hosts)
                    samples[name]["census"]["resource_snapshot"]["root_disk_reserved_bytes"] = value
                    result = self.evaluate_prod_scope(hosts=hosts, samples=samples)
                    self.assertFalse(result["advisory_allowed"])
                    self.assertIn(f"{name}:root-disk-reserve-invalid", result["reasons"])

    def test_ilo_prod_capacity_counts_proposed_sibling_and_global_slots(self) -> None:
        hosts = host_configs(include_ilo_prod=True)
        samples = empty_samples_for_hosts(self.now, hosts)

        install_groups(samples, "ilo-prod", [compiler_group(11000 + i) for i in range(3)])
        at_host_limit = self.evaluate_prod_scope(
            destination="ilo-prod", hosts=hosts, samples=samples,
        )
        self.assertTrue(at_host_limit["advisory_allowed"], at_host_limit["reasons"])
        self.assertEqual(at_host_limit["current_fleet_compiler_admission_slot_count"], 3)
        self.assertEqual(at_host_limit["host_compiler_admission_slot_limits"]["ilo-prod"], 4)

        samples = empty_samples_for_hosts(self.now, hosts)
        samples["ilo-prod"]["census"]["resource_snapshot"]["available_memory_bytes"] = fleet.MIN_AVAILABLE_MEMORY - 1
        below_memory_floor = self.evaluate_prod_scope(
            destination="ilo-prod", hosts=hosts, samples=samples,
        )
        self.assertFalse(below_memory_floor["advisory_allowed"])
        self.assertIn("ilo-prod:available-memory-below-8-gib-or-missing", below_memory_floor["reasons"])

        samples = empty_samples_for_hosts(self.now, hosts)
        samples["ilo-prod"]["census"]["resource_snapshot"]["root_disk_available_bytes"] = fleet.MIN_DESTINATION_DISK
        at_disk_floor = self.evaluate_prod_scope(
            destination="ilo-prod", hosts=hosts, samples=samples,
        )
        self.assertTrue(at_disk_floor["advisory_allowed"], at_disk_floor["reasons"])

        samples["ilo-prod"]["census"]["resource_snapshot"]["root_disk_reserved_bytes"] = fleet.MIN_DESTINATION_DISK + 1
        below_reserve = self.evaluate_prod_scope(
            destination="ilo-prod", hosts=hosts, samples=samples,
        )
        self.assertFalse(below_reserve["advisory_allowed"])
        self.assertIn("destination-root-disk-below-ci-reserve", below_reserve["reasons"])

        samples = empty_samples_for_hosts(self.now, hosts)
        install_groups(samples, "ilo-prod", [compiler_group(11100 + i) for i in range(4)])
        over_host_limit = self.evaluate_prod_scope(
            destination="ilo-prod", hosts=hosts, samples=samples,
        )
        self.assertFalse(over_host_limit["advisory_allowed"])
        self.assertIn("ilo-prod:host-compiler-admission-slot-limit-reached", over_host_limit["reasons"])

        hosts["ilo-prod"]["limit"] = 5
        samples = empty_samples_for_hosts(self.now, hosts)
        over_configuration_limit = self.evaluate_prod_scope(
            destination="ilo-prod", hosts=hosts, samples=samples,
        )
        self.assertFalse(over_configuration_limit["advisory_allowed"])
        self.assertIn("ilo-prod:host-limit-exceeds-4", over_configuration_limit["reasons"])

        hosts = host_configs(include_ilo_prod=True)
        samples = empty_samples_for_hosts(self.now, hosts)
        for host, count in (("ilo-prod", 4), ("ilo", 6), ("h16001mac", 6)):
            install_groups(samples, host, [compiler_group(11200 + 100 * len(host) + i) for i in range(count)])
        full = self.evaluate_prod_scope(destination="local", hosts=hosts, samples=samples)
        self.assertEqual(full["current_fleet_compiler_group_count"], 16)
        self.assertEqual(full["current_fleet_compiler_admission_slot_count"], 16)
        self.assertFalse(full["advisory_allowed"])
        self.assertIn("fleet-compiler-admission-slot-limit-reached", full["reasons"])

    def test_ilo_prod_counts_each_orphan_rustc_and_refuses_underreported_provenance(self) -> None:
        hosts = host_configs(include_ilo_prod=True)
        samples = empty_samples_for_hosts(self.now, hosts)
        records = {}
        for pid in range(11300, 11315):
            records[pid] = build_census.ProcessRecord(
                pid=pid, ppid=1, pgid=11300, start_token=f"test:{pid}",
                executable="/toolchain/bin/rustc", argv=None,
            )
        for pid in range(11400, 11416):
            records[pid] = build_census.ProcessRecord(
                pid=pid, ppid=1, pgid=11400, start_token=None,
                executable=None, argv=None, identity_validated=False,
                compiler_candidate=True,
            )
        snapshot = build_census.ProcessSnapshot(
            records=records,
            started_at_utc="2026-10-06T16:59:59+00:00",
            finished_at_utc="2026-10-06T16:59:59.1+00:00",
            complete=True,
            issue_counts={},
            race_counts={},
            effective_uid=0,
            visible_process_count=len(records),
            captured_argv_bytes=0,
            captured_argv_processes=0,
            argv_budget_exhausted=False,
        )
        groups = build_census.compiler_groups(snapshot, [])
        install_groups(samples, "ilo-prod", groups)

        result = self.evaluate_prod_scope(hosts=hosts, samples=samples)

        self.assertEqual(result["hosts"]["ilo-prod"]["compiler_admission_slot_count"], 31)
        self.assertEqual(result["current_fleet_compiler_admission_slot_count"], 31)
        self.assertFalse(result["advisory_allowed"])
        self.assertIn("ilo-prod:host-compiler-admission-slot-limit-reached", result["reasons"])
        self.assertIn("fleet-compiler-admission-slot-limit-reached", result["reasons"])

        # The wrapper must not accept the previous one-per-PGID encoding,
        # even when the caller supplies the full rustc member list.
        underreported_group = dict(groups[0])
        underreported_group["admission_slot_count"] = 1
        underreported_provenance = dict(underreported_group["admission_slot_provenance"])
        underreported_provenance["residual_slot_count"] = 1
        underreported_group["admission_slot_provenance"] = underreported_provenance
        install_groups(samples, "ilo-prod", [underreported_group])
        underreported = self.evaluate_prod_scope(hosts=hosts, samples=samples)
        self.assertIn("ilo-prod:compiler-admission-slot-provenance-invalid", underreported["reasons"])
        self.assertGreaterEqual(
            underreported["hosts"]["ilo-prod"]["compiler_admission_slot_count"], 15
        )

    def test_ilo_prod_sibling_and_unknown_residual_slots_count_conservatively(self) -> None:
        hosts = host_configs(include_ilo_prod=True)
        samples = empty_samples_for_hosts(self.now, hosts)
        siblings = compiler_group(
            12000,
            kind="cargo",
            classification="unknown",
            cargo_pids=[12001, 12002],
            pids=[12001, 12002, 12003],
            residual_member_pids=[12003],
        )
        unknown = compiler_group(12100, kind="unknown", classification="unknown")
        install_groups(
            samples,
            "ilo-prod",
            [siblings, unknown],
            [
                cargo_entry(12001, 12000, None, classification="unknown"),
                cargo_entry(12002, 12000, None, classification="unknown"),
            ],
        )

        result = self.evaluate_prod_scope(hosts=hosts, samples=samples)

        self.assertTrue(result["advisory_allowed"], result["reasons"])
        self.assertEqual(result["hosts"]["ilo-prod"]["compiler_group_count"], 2)
        self.assertEqual(result["hosts"]["ilo-prod"]["compiler_admission_slot_count"], 4)
        self.assertEqual(result["current_fleet_compiler_admission_slot_count"], 4)
        self.assertIn(
            "ilo-prod:active-cargo-process-jobs-unknown-counted-conservatively",
            result["limitations"],
        )
        self.assertIn(
            "ilo-prod:unresolved-compiler-group-counted-conservatively",
            result["limitations"],
        )

    def test_ilo_prod_mac_exclusion_is_opted_in_and_reported_as_partial_scope(self) -> None:
        hosts = host_configs(include_ilo_prod=True)
        reason = "2026-10-16 user request; fresh Mac-offline proof reviewed with managed permit"
        active_hosts, excluded_hosts = fleet._resolve_sample_scope(
            destination="ilo-prod",
            exclude_hosts=["h16001mac"],
            exclusion_reason=reason,
            hosts=hosts,
        )
        samples = empty_samples_for_hosts(self.now, active_hosts)
        result = self.evaluate_prod_scope(
            destination="ilo-prod",
            hosts=active_hosts,
            samples=samples,
            excluded_hosts=excluded_hosts,
        )

        self.assertTrue(result["advisory_allowed"], result["reasons"])
        self.assertEqual(set(result["sample_scope"]["configured_hosts"]), set(hosts))
        self.assertEqual(set(result["sample_scope"]["requested_hosts"]), {"local", "ilo", "ilo-prod"})
        self.assertEqual(set(result["sample_scope"]["sampled_hosts"]), {"local", "ilo", "ilo-prod"})
        self.assertEqual(result["sample_scope"]["excluded_hosts"], [{"host": "h16001mac", "reason": reason}])
        self.assertIn("2026-10-16", result["sample_scope"]["exclusion_authorization"])
        self.assertIn("fresh Mac-offline proof", result["sample_scope"]["exclusion_authorization"])
        self.assertFalse(result["sample_scope"]["full_fleet_sampled"])

        with self.assertRaises(ValueError):
            fleet._resolve_sample_scope(
                destination="ilo-prod", exclude_hosts=["h16001mac"],
                exclusion_reason=reason, hosts=fleet.DEFAULT_HOSTS,
            )
        for missing_reason in (None, "", " \t"):
            with self.subTest(reason=missing_reason):
                with self.assertRaises(ValueError):
                    fleet._resolve_sample_scope(
                        destination="ilo-prod", exclude_hosts=["h16001mac"],
                        exclusion_reason=missing_reason, hosts=hosts,
                    )

        all_samples = empty_samples_for_hosts(self.now, hosts)
        for bad_exclusion in (
            {"unknown-host": reason},
            {"local": reason},
        ):
            with self.subTest(exclusion=bad_exclusion):
                invalid = self.evaluate_prod_scope(
                    destination="ilo-prod", hosts=hosts, samples=all_samples,
                    excluded_hosts=bad_exclusion,
                )
                self.assertFalse(invalid["advisory_allowed"])
                self.assertIn("fleet-host-exclusion-not-authorized-for-this-scope", invalid["reasons"])

        without_prod_flag_hosts = host_configs(include_ilo_prod=True)
        no_flag = fleet.evaluate_fleet(
            empty_samples_for_hosts(self.now, without_prod_flag_hosts),
            destination="ilo-prod", requested_jobs=1,
            hosts=without_prod_flag_hosts, now=self.now,
            include_ilo_prod=False,
            excluded_hosts={"h16001mac": reason},
        )
        self.assertFalse(no_flag["advisory_allowed"])
        self.assertIn("fleet-host-exclusion-not-authorized-for-this-scope", no_flag["reasons"])

        prod_missing_from_active_scope = {name: hosts[name] for name in ("local", "ilo")}
        incomplete_exclusion = fleet.evaluate_fleet(
            empty_samples_for_hosts(self.now, prod_missing_from_active_scope),
            destination="ilo-prod", requested_jobs=1,
            hosts=prod_missing_from_active_scope, now=self.now,
            include_ilo_prod=True,
            excluded_hosts={"h16001mac": reason},
        )
        self.assertFalse(incomplete_exclusion["advisory_allowed"])
        self.assertIn("fleet-sample-scope-invalid", incomplete_exclusion["reasons"])

    def test_collect_once_samples_opted_in_ilo_prod_exactly_once(self) -> None:
        hosts = host_configs(include_ilo_prod=True)
        counts: Counter[str] = Counter()

        def fake_sample(name: str, host: object, source: bytes) -> dict[str, object]:
            counts[name] += 1
            return {"host": name, "transport_complete": True}

        result = fleet.collect_once(hosts, b"sampler bytes", sample_host=fake_sample)
        self.assertEqual(set(result), set(hosts))
        self.assertEqual(counts, Counter({name: 1 for name in hosts}))

    def test_cli_requires_prod_opt_in_before_sampling_and_collects_four_when_enabled(self) -> None:
        with mock.patch.object(fleet, "collect_once") as collect:
            with contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    fleet.main(["--destination", "ilo-prod"])
        self.assertEqual(error.exception.code, 2)
        collect.assert_not_called()

        now = dt.datetime.now(dt.timezone.utc)
        captured_hosts: dict[str, dict[str, object]] = {}

        def fake_collect(hosts: Mapping[str, Mapping[str, object]], source: bytes) -> dict[str, dict[str, object]]:
            captured_hosts.update({name: dict(config) for name, config in hosts.items()})
            return empty_samples_for_hosts(now, hosts)

        stdout = io.StringIO()
        with mock.patch.object(fleet, "collect_once", side_effect=fake_collect) as collect:
            with contextlib.redirect_stdout(stdout):
                return_code = fleet.main([
                    "--destination", "local", "--include-ilo-prod",
                    "--prod-target", "ilo-prod-test",
                    "--prod-python", "/nix/store/test-python3",
                ])
        self.assertEqual(return_code, 0)
        collect.assert_called_once()
        self.assertEqual(set(captured_hosts), {"local", "ilo", "h16001mac", "ilo-prod"})
        self.assertEqual(captured_hosts["ilo-prod"]["ssh_target"], "ilo-prod-test")
        self.assertEqual(captured_hosts["ilo-prod"]["python"], "/nix/store/test-python3")
        report = json.loads(stdout.getvalue())
        self.assertTrue(report["advisory_allowed"], report["reasons"])
        self.assertEqual(set(report["sample_scope"]["configured_hosts"]), set(captured_hosts))
        self.assertTrue(report["sample_scope"]["full_fleet_sampled"])

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
