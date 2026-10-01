"""Deterministic trace and adversarial tests for the standalone policy model."""

import unittest

from docs.prototypes.ir_materialization_policy_model import (
    ColdV2Replay,
    CorruptObject,
    Generation,
    LiveGenerationPolicy,
    Repository,
    Segment,
    StaleAncestry,
    V1RouteBaseline,
    run_ab_trace,
    run_trace,
)


def segment(payload: bytes, *, plane: str = "Core", key: int = 0, last: int | None = None) -> Segment:
    first_key = key.to_bytes(4, "big")
    last_key = (key if last is None else last).to_bytes(4, "big")
    return Segment(plane, first_key, last_key, payload)


def generation(
    commit: str,
    segments: tuple[Segment, ...],
    *,
    parent: str | None = None,
    input_claim: str = "input-a",
    changed_bytes: int = 0,
    changed_actions: int = 0,
) -> Generation:
    return Generation(
        target="target",
        commit=commit,
        build="build-a",
        image_facts="facts-a",
        input_claim=input_claim,
        segments=segments,
        parent=parent,
        changed_bytes=changed_bytes,
        changed_actions=changed_actions,
    )


def add_and_select(repository: Repository, item: Generation, ref: str = "main") -> None:
    repository.add(item)
    repository.set_ref(item.target, ref, item.commit)


def replay_repeatedly(policy: LiveGenerationPolicy, item: Generation, count: int = 3) -> None:
    policy.repository.set_ref(item.target, "main", item.commit)
    for _ in range(count):
        with policy.materialize(item.commit, "main") as replay:
            assert replay.content_root == item.content_root
            assert replay.generation_root == item.generation_root


class V1RouteBaselineTests(unittest.TestCase):
    def test_exact_hot_delta_pristine_and_corrupt_base_fallback(self) -> None:
        repository = Repository()
        shared = segment(b"shared", key=1)
        old_changed = segment(b"old", key=2)
        new_changed = segment(b"new", key=2)
        base = generation("base", (shared, old_changed))
        target = generation(
            "target-v1", (shared, new_changed), parent=base.commit, changed_bytes=3, changed_actions=1
        )
        repository.add(base)
        add_and_select(repository, target)

        routes = V1RouteBaseline(repository, hot_bytes=64, max_hops=2)
        self.assertEqual(routes.read(target.commit, shared), shared.payload)
        self.assertEqual(routes.read(target.commit, shared), shared.payload)
        self.assertEqual(routes.read(target.commit, shared), shared.payload)
        self.assertEqual(routes.read(target.commit, new_changed), new_changed.payload)
        self.assertEqual(routes.work.delta_reads, 2)
        self.assertEqual(routes.work.v1_hot_hits, 1)
        self.assertEqual(routes.work.pristine_reads, 1)

        # A broken predecessor bridge is an optimization miss. The selected
        # generation's independently mapped object remains the fallback.
        fallback = V1RouteBaseline(repository, hot_bytes=0, max_hops=2)
        repository.remove_mapping(base.commit, shared)
        self.assertEqual(fallback.read(target.commit, shared), shared.payload)
        self.assertEqual(fallback.work.delta_fallbacks, 1)
        self.assertEqual(fallback.work.pristine_reads, 1)

    def test_same_range_changed_identity_and_bounded_delta_hops(self) -> None:
        repository = Repository()
        original = segment(b"original", key=7)
        changed_a = segment(b"changed-a", key=7)
        changed_b = segment(b"changed-b", key=7)
        base = generation("h0", (original,))
        h1 = generation("h1", (changed_a,), parent="h0", changed_bytes=9, changed_actions=1)
        h2 = generation("h2", (changed_b,), parent="h1", changed_bytes=9, changed_actions=1)
        reverted = generation("h3", (original,), parent="h2", changed_bytes=9, changed_actions=1)
        for item in (base, h1, h2, reverted):
            repository.add(item)
        repository.set_ref("target", "main", reverted.commit)

        routes = V1RouteBaseline(repository, hot_bytes=0, max_hops=2)
        self.assertEqual(routes.read(reverted.commit, original), original.payload)
        self.assertEqual(routes.work.delta_reads, 0)
        self.assertEqual(routes.work.pristine_reads, 1)
        self.assertLessEqual(routes.work.delta_hops_considered, 2)
        self.assertNotEqual(original.descriptor, changed_a.descriptor)

    def test_large_changed_work_rejects_an_exact_but_fanout_heavy_delta(self) -> None:
        repository = Repository()
        unchanged = segment(b"tiny", key=3)
        base = generation("fanout-base", (unchanged,))
        target = generation(
            "fanout-target",
            (unchanged,),
            parent=base.commit,
            changed_bytes=100,
            changed_actions=700,
        )
        repository.add(base)
        add_and_select(repository, target)
        route = V1RouteBaseline(repository, hot_bytes=0, max_hops=4)
        self.assertEqual(route.read(target.commit, unchanged), unchanged.payload)
        self.assertEqual(route.work.delta_reads, 0)
        self.assertEqual(route.work.delta_fanout_fallbacks, 1)
        self.assertEqual(route.work.pristine_reads, 1)


class LiveGenerationPolicyTests(unittest.TestCase):
    def test_ab_trace_compares_exact_oracles_and_counts_three_routes(self) -> None:
        result = run_ab_trace()
        self.assertTrue(result["oracle_roots_match"])
        route = result["v1_segment_routes"]
        self.assertGreater(route["delta_reads"], 0)
        self.assertGreater(route["pristine_reads"], 0)
        self.assertGreater(route["v1_hot_hits"], 0)
        cold = result["v2_cold_replay"]
        live = result["v2_live_generation"]
        self.assertEqual(cold["semantic_verifier_runs"], 3)
        self.assertEqual(live["semantic_verifier_runs"], 2)
        self.assertEqual(live["generation_hot_hits"], 1)
        self.assertLess(live["physical_payload_bytes"], cold["physical_payload_bytes"])
        self.assertLessEqual(result["v2_live_peak_bytes"], 16_384)

    def test_repeated_exact_generation_matches_oracle_and_avoids_replay_work(self) -> None:
        repository = Repository()
        segments = tuple(segment(bytes([index]) * 128, key=index) for index in range(4))
        item = generation("repeated", segments)
        add_and_select(repository, item)

        cold = ColdV2Replay(repository)
        for _ in range(3):
            with cold.replay(item.commit, "main") as replay:
                self.assertEqual(replay.content_root, item.content_root)
                self.assertEqual(replay.generation_root, item.generation_root)

        policy = LiveGenerationPolicy(
            repository,
            max_bytes=16_384,
            max_entries=8,
            ghost_entries=16,
        )
        replay_repeatedly(policy, item, 3)
        entry = policy.entries[item.exact_key]
        self.assertEqual(entry.tier, "protected")
        self.assertEqual(policy.work.generation_hot_hits, 1)
        self.assertEqual(policy.work.semantic_verifier_runs, 2)
        self.assertEqual(cold.work.semantic_verifier_runs, 3)
        self.assertEqual(policy.work.spool_bytes_written, 0)
        self.assertEqual(cold.work.spool_bytes_written, sum(len(s.payload) for s in segments) * 3)
        self.assertLess(policy.work.physical_payload_bytes, cold.work.physical_payload_bytes)
        self.assertLessEqual(policy.total_bytes, policy.max_bytes)
        self.assertLessEqual(policy.peak_policy_bytes, policy.max_bytes)
        self.assertLessEqual(policy.entry_count, policy.max_entries)

    def test_same_content_different_generation_root_is_a_miss(self) -> None:
        repository = Repository()
        shared = segment(b"same exact payload", key=4)
        base = generation("input-one", (shared,), input_claim="input-one")
        next_version = generation(
            "input-two", (shared,), parent=base.commit, input_claim="input-two"
        )
        repository.add(base)
        repository.add(next_version)
        repository.set_ref("target", "main", next_version.commit)
        policy = LiveGenerationPolicy(repository, max_bytes=12_288, max_entries=8)

        replay_repeatedly(policy, base)
        self.assertEqual(base.content_root, next_version.content_root)
        self.assertNotEqual(base.generation_root, next_version.generation_root)
        repository.set_ref("target", "main", next_version.commit)
        hits_before = policy.work.replay_cache_hits
        with policy.materialize(next_version.commit, "main") as replay:
            self.assertEqual(replay.generation_root, next_version.generation_root)
            self.assertIsNone(replay.payloads)  # First exact use is still cold.
        self.assertEqual(policy.work.replay_cache_hits, hits_before)
        self.assertEqual(policy.work.delta_reads, 1)

    def test_changed_same_range_takes_pristine_and_exhausted_hops_are_bounded(self) -> None:
        repository = Repository()
        original = segment(b"before", key=9)
        changed = segment(b"after", key=9)
        base = generation("base", (original,))
        target = generation("changed", (changed,), parent=base.commit, changed_bytes=5, changed_actions=1)
        repository.add(base)
        repository.add(target)
        repository.set_ref("target", "main", target.commit)
        policy = LiveGenerationPolicy(repository, max_bytes=8192, max_entries=4)
        with policy.materialize(target.commit, "main"):
            pass
        self.assertEqual(policy.work.delta_reads, 0)
        self.assertEqual(policy.work.pristine_reads, 1)
        self.assertNotEqual(original.key, changed.key)

        hop_repository = Repository()
        middle_a = segment(b"middle-a", key=9)
        middle_b = segment(b"middle-b", key=9)
        chain = (
            generation("h0", (original,)),
            generation("h1", (middle_a,), parent="h0", changed_bytes=1, changed_actions=1),
            generation("h2", (middle_b,), parent="h1", changed_bytes=1, changed_actions=1),
            generation("h3", (original,), parent="h2", changed_bytes=1, changed_actions=1),
        )
        for item in chain:
            hop_repository.add(item)
        hop_repository.set_ref("target", "main", "h3")
        hop_policy = LiveGenerationPolicy(
            hop_repository,
            max_bytes=8192,
            max_entries=4,
            max_hops=2,
        )
        with hop_policy.materialize("h3", "main"):
            pass
        self.assertEqual(hop_policy.work.delta_reads, 0)
        self.assertEqual(hop_policy.work.pristine_reads, 1)
        self.assertLessEqual(hop_policy.work.delta_hops_considered, 2)

    def test_mutation_is_hidden_only_while_exact_live_bytes_are_available(self) -> None:
        repository = Repository()
        item = generation("mutable", (segment(b"verified bytes", key=1),))
        add_and_select(repository, item)
        policy = LiveGenerationPolicy(repository, max_bytes=8192, max_entries=4)
        replay_repeatedly(policy, item, 3)
        object_id = item.segments[0].object_id
        file_reads = policy.work.physical_payload_bytes
        repository.mutate_object(object_id, b"externally changed")

        with policy.materialize(item.commit, "main") as replay:
            self.assertEqual(replay.payloads[item.segments[0].key], b"verified bytes")
        self.assertEqual(policy.work.physical_payload_bytes, file_reads)

        cold_after_restart = LiveGenerationPolicy(repository, max_bytes=8192, max_entries=4)
        with self.assertRaises(CorruptObject):
            cold_after_restart.materialize(item.commit, "main")
        self.assertEqual(cold_after_restart.entry_count, 0)
        self.assertEqual(cold_after_restart.work.replay_cache_admissions, 0)

        cold_oracle = ColdV2Replay(repository)
        with self.assertRaises(CorruptObject):
            cold_oracle.replay(item.commit, "main")
        self.assertGreater(cold_oracle.work.object_hash_bytes, 0)

    def test_live_bytes_do_not_skip_fresh_locator_or_closure_metadata_check(self) -> None:
        repository = Repository()
        item = generation("metadata", (segment(b"payload", key=1),))
        add_and_select(repository, item)
        policy = LiveGenerationPolicy(repository, max_bytes=8192, max_entries=4)
        replay_repeatedly(policy, item, 3)
        hits_before = policy.work.replay_cache_hits
        repository.remove_mapping(item.commit, item.segments[0])
        with self.assertRaises(CorruptObject):
            policy.materialize(item.commit, "main")
        self.assertEqual(policy.work.replay_cache_hits, hits_before)
        self.assertEqual(repository.pins, {})

    def test_fresh_ancestry_and_generation_pin_survive_gc_window(self) -> None:
        repository = Repository()
        item = generation("pinned", (segment(b"payload", key=1),))
        add_and_select(repository, item)
        policy = LiveGenerationPolicy(repository, max_bytes=8192, max_entries=4)
        replay_repeatedly(policy, item, 3)
        lease = policy.materialize(item.commit, "main")
        object_id = item.segments[0].object_id
        repository.refs.pop((item.target, "main"))
        self.assertNotIn(object_id, repository.collect())
        with self.assertRaises(StaleAncestry):
            policy.materialize(item.commit, "main")
        lease.close()
        self.assertIn(object_id, repository.collect())
        self.assertEqual(repository.pins, {})
        self.assertEqual(policy.work.gc_pins_acquired, policy.work.gc_pins_released)

    def test_pinned_live_generation_cannot_be_replaced_under_entry_pressure(self) -> None:
        repository = Repository()
        hot = generation("pinned-hot", (segment(b"H" * 96, key=1),))
        scan = generation("pinned-scan", (segment(b"S" * 96, key=2),))
        repository.add(hot)
        repository.add(scan)
        repository.set_ref("target", "main", hot.commit)
        policy = LiveGenerationPolicy(repository, max_bytes=8192, max_entries=1)
        replay_repeatedly(policy, hot, 3)
        pinned = policy.materialize(hot.commit, "main")
        self.assertEqual(policy.entries[hot.exact_key].pins, 1)

        repository.set_ref("target", "scan", scan.commit)
        for _ in range(2):
            with policy.materialize(scan.commit, "scan"):
                pass
        self.assertIn(hot.exact_key, policy.entries)
        self.assertEqual(policy.entries[hot.exact_key].tier, "protected")
        self.assertEqual(policy.entries[hot.exact_key].pins, 1)
        self.assertNotIn(scan.exact_key, policy.entries)
        self.assertGreater(policy.work.replay_admission_rejections, 0)
        self.assertLessEqual(policy.total_bytes, policy.max_bytes)

        pinned.close()
        self.assertEqual(repository.pins, {})

    def test_ref_change_during_cold_materialization_does_not_admit(self) -> None:
        repository = Repository()
        item = generation("changing-ref", (segment(b"payload", key=1),))
        unrelated = generation("unrelated", (segment(b"other", key=2),))
        repository.add(item)
        repository.add(unrelated)
        repository.set_ref("target", "main", item.commit)
        policy = LiveGenerationPolicy(repository, max_bytes=8192, max_entries=4)
        repository.before_final_ancestry = lambda: repository.set_ref(
            "target", "main", unrelated.commit
        )
        with self.assertRaises(StaleAncestry):
            policy.materialize(item.commit, "main")
        self.assertEqual(policy.work.replay_cache_admissions, 0)
        self.assertEqual(policy.entry_count, 0)
        self.assertEqual(repository.pins, {})

    def test_oversize_falls_back_to_cold_spool_without_resident_bytes(self) -> None:
        repository = Repository()
        item = generation("oversize", (segment(b"x" * 50_000, key=1),))
        add_and_select(repository, item)
        policy = LiveGenerationPolicy(repository, max_bytes=4096, max_entries=2)
        with policy.materialize(item.commit, "main") as replay:
            self.assertEqual(replay.content_root, item.content_root)
            self.assertIsNone(replay.payloads)
        self.assertEqual(policy.work.semantic_verifier_runs, 1)
        self.assertGreater(policy.work.spool_bytes_written, 0)
        self.assertEqual(policy.entry_count, 0)
        self.assertLessEqual(policy.peak_policy_bytes, policy.max_bytes)

    def test_scan_trace_is_bounded_and_keeps_protected_hot_generation(self) -> None:
        trace = run_trace()
        self.assertGreater(trace["scan_over_budget_multiple"], 10)
        self.assertTrue(trace["protected_hot_generation_present"])
        self.assertLessEqual(trace["total_policy_bytes"], trace["budget_bytes"])
        self.assertLessEqual(trace["peak_policy_bytes"], trace["budget_bytes"])

    def test_repeated_full_scan_churns_probation_without_displacing_hot_entry(self) -> None:
        repository = Repository()
        hot = generation("hot", (segment(b"H" * 128, key=1), segment(b"I" * 128, key=2)))
        add_and_select(repository, hot)
        scan_items = [
            generation(f"scan-{index}", (segment(bytes([index + 10]) * 96, key=100 + index),))
            for index in range(4)
        ]
        for item in scan_items:
            repository.add(item)
        policy = LiveGenerationPolicy(
            repository,
            max_bytes=8192,
            max_entries=12,
            ghost_entries=16,
        )
        replay_repeatedly(policy, hot, 3)
        hot_tier_before = policy.entries[hot.exact_key].tier
        for _ in range(3):
            for item in scan_items:
                repository.set_ref(item.target, "scan", item.commit)
                with policy.materialize(item.commit, "scan"):
                    pass
        self.assertEqual(hot_tier_before, "protected")
        self.assertIn(hot.exact_key, policy.entries)
        self.assertEqual(policy.entries[hot.exact_key].tier, "protected")
        self.assertLessEqual(policy.total_bytes, policy.max_bytes)
        self.assertLessEqual(policy.entry_count, policy.max_entries)
        self.assertLessEqual(policy.peak_policy_bytes, policy.max_bytes)

    def test_count_min_false_positive_never_promotes_a_one_pass_key(self) -> None:
        repository = Repository()
        hot = generation("hot", (segment(b"H" * 128, key=1),))
        scan = generation("scan", (segment(b"S" * 128, key=2),))
        repository.add(hot)
        repository.add(scan)
        repository.set_ref("target", "main", hot.commit)
        policy = LiveGenerationPolicy(repository, max_bytes=8192, max_entries=8)
        replay_repeatedly(policy, hot, 3)
        hot_entry = policy.entries[hot.exact_key]

        scan_key = scan.exact_key
        indices = policy.sketch._indices(b"\0".join(str(value).encode() for value in scan_key))
        for index in indices:
            policy.sketch.cells[index] = 7
        repository.set_ref("target", "scan", scan.commit)
        with policy.materialize(scan.commit, "scan"):
            pass
        self.assertNotIn(scan_key, policy.entries)
        self.assertIs(policy.entries[hot.exact_key], hot_entry)
        self.assertEqual(hot_entry.tier, "protected")


if __name__ == "__main__":
    unittest.main()
