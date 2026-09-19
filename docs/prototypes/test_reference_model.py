import random
from collections import Counter
import unittest

from reference_model import (
    DeltaError,
    MapChange,
    MerkleMap,
    ObjectVersion,
    Trace,
    WorkspaceDelta,
    WorkspaceRoot,
    diff_nodes,
    encode_pack,
    decode_pack,
    join_delta,
    repack,
)


def v(value, **kw):
    return ObjectVersion(value, **kw)


class ReferenceModelTests(unittest.TestCase):
    def test_complete_values_and_history_independent_root(self):
        a = {"b": v(2, references=("r",)), "a": v(1, availability="unavailable")}
        one = MerkleMap.from_items(a)
        two = MerkleMap.empty()
        two = two.with_changes([MapChange("a", None, a["a"])])
        two = two.with_changes([MapChange("b", None, a["b"])])
        self.assertEqual(one.state_root, two.state_root)
        self.assertEqual(one.get("a"), a["a"])
        erased = one.with_changes([MapChange(k, val, None) for k, val in sorted(a.items())])
        self.assertEqual(erased.state_root, MerkleMap.empty().state_root)
        self.assertEqual(MerkleMap.empty().state_root, MerkleMap.from_items({}).state_root)

    def test_key_only_cuts_survive_value_edit(self):
        base = MerkleMap.from_items({f"k{i:03d}": v(i) for i in range(100)})
        changed = base.with_changes([MapChange("k050", v(50), v({"changed": True}))])
        self.assertEqual(base.leaf_cuts, changed.leaf_cuts)
        self.assertEqual(diff_nodes(base, changed)[1], 1)
        self.assertLessEqual(max(map(len, base.leaf_cuts)), 64)
        self.assertTrue(all(len(cut) >= 8 for cut in base.leaf_cuts[:-1]))

    def test_insert_delete_unequal_tree_shapes_are_counted(self):
        base = MerkleMap.from_items({f"k{i:03d}": v(i) for i in range(200)})
        inserted = base.with_changes([MapChange("k050x", None, v(99))])
        deleted = base.with_changes([MapChange("k050", v(50), None)])
        self.assertGreater(diff_nodes(base, inserted)[1], 0)
        self.assertGreater(diff_nodes(base, deleted)[1], 0)

    def test_version_freezes_mutable_payload_and_rejects_opaque(self):
        payload = {"items": [1]}
        version = v(payload)
        original = version.identity
        payload["items"].append(2)
        self.assertEqual(version.identity, original)
        self.assertEqual(version.value, {"items": [1]})
        version.value["items"].append(3)
        self.assertEqual(version.value, {"items": [1]})
        with self.assertRaises(TypeError):
            v(object())
        with self.assertRaises(TypeError):
            v({1: "ambiguous"})
        with self.assertRaises(TypeError):
            v(float("nan"))

    def test_delta_apply_compose_inverse_and_wrong_base(self):
        base = MerkleMap.from_items({"a": v(1), "b": v(2)})
        mid = base.with_changes([MapChange("a", v(1), v(3))])
        end = mid.with_changes([MapChange("b", v(2), None), MapChange("c", None, v(4))])
        first, second = base.delta_to(mid), mid.delta_to(end)
        self.assertEqual(first.compose(second).apply(base).state_root, end.state_root)
        self.assertEqual(first.inverse().apply(mid).state_root, base.state_root)
        self.assertEqual(first.compose(first.inverse()).changes, ())
        with self.assertRaises(DeltaError):
            first.apply(end)
        with self.assertRaises(DeltaError):
            first.compose(base.delta_to(end))
        with self.assertRaises(DeltaError):
            type(first)(first.base_root, first.target_root, (first.changes[0], first.changes[0]))
        with self.assertRaises(DeltaError):
            type(first)(first.base_root, first.target_root, (MapChange("z", None, v(1)), MapChange("a", None, v(2))))

    def test_workspace_root_is_atomic(self):
        maps = {"entities": MerkleMap.from_items({"e": v(1)}), "edges": MerkleMap.from_items({"x": v(2)})}
        target = dict(maps)
        target["entities"] = target["entities"].with_changes([MapChange("e", v(1), v(3))])
        target["edges"] = target["edges"].with_changes([MapChange("x", v(2), v(4))])
        delta = WorkspaceDelta(WorkspaceRoot.from_maps(maps), WorkspaceRoot.from_maps(target), tuple((name, maps[name].delta_to(target[name])) for name in sorted(maps)))
        self.assertEqual(WorkspaceRoot.from_maps(delta.apply(maps)).root, delta.target.root)
        with self.assertRaises(DeltaError):
            delta.apply({"entities": maps["entities"], "edges": target["edges"]})

    def test_old_state_join_includes_signed_cross_term(self):
        old_a = Counter({("k", "a0"): 1})
        old_b = Counter({("k", "b0"): 1})
        da = Counter({("k", "a1"): 1, ("k", "a0"): -1})
        db = Counter({("k", "b1"): 1, ("k", "b0"): -1})
        got = join_delta(old_a, da, old_b, db)
        expected = Counter({("a1", "b1"): 1, ("a0", "b0"): -1})
        self.assertEqual(got, expected)

    def test_repack_changes_physical_identity_only(self):
        mapping = MerkleMap.from_items({"a": v(1), "b": v(2)})
        pack_a, locations_a = repack(mapping, "plain")
        pack_b, locations_b = repack(mapping, "compressed")
        self.assertNotEqual(pack_a, pack_b)
        self.assertNotEqual(locations_a, locations_b)
        self.assertEqual(decode_pack(encode_pack(mapping, "plain")).state_root, mapping.state_root)
        self.assertEqual(decode_pack(encode_pack(mapping, "compressed")).values, mapping.values)
        self.assertNotEqual(encode_pack(mapping, "plain"), encode_pack(mapping, "compressed"))
        self.assertEqual(decode_pack(encode_pack(mapping, "compressed")).state_root, mapping.state_root)

    def test_trace_compaction_preserves_allowed_observations(self):
        trace = Trace.from_updates([("a", 0, 1), ("b", 1, 1), ("a", 2, -1), ("c", 3, 1)])
        compacted = trace.compact(2)
        for time in range(2, 5):
            self.assertEqual(compacted.observe(time), trace.observe(time))
        self.assertEqual(trace.observe(1), Counter({"a": 1, "b": 1}))
        # Historical state at t=1 requires the un-compacted trace/checkpoint.
        self.assertNotEqual(compacted.observe(1), trace.observe(1))

    def test_seeded_randomized_oracle(self):
        rng = random.Random(20260908)
        values = {}
        model = MerkleMap.empty()
        for _ in range(200):
            key = f"k{rng.randrange(30):02d}"
            before = values.get(key)
            after = None if rng.random() < 0.25 else v(rng.randrange(1000), availability="captured", references=("auth",))
            change = MapChange(key, before, after)
            model = model.with_changes([change])
            if after is None:
                values.pop(key, None)
            else:
                values[key] = after
            oracle = MerkleMap.from_items(values)
            self.assertEqual(model.state_root, oracle.state_root)
            self.assertEqual(model.leaf_cuts, oracle.leaf_cuts)

    def test_seeded_randomized_join_oracle(self):
        rng = random.Random(77)
        for _ in range(100):
            old_a = Counter({(str(rng.randrange(4)), f"a{i}"): 1 for i in range(4)})
            old_b = Counter({(str(rng.randrange(4)), f"b{i}"): 1 for i in range(4)})
            da = Counter({(str(rng.randrange(4)), "new_a"): 1, next(iter(old_a)): -1})
            db = Counter({(str(rng.randrange(4)), "new_b"): 1, next(iter(old_b)): -1})
            actual = join_delta(old_a, da, old_b, db)
            def joined(a, b):
                out = Counter()
                for (ka, va), wa in a.items():
                    for (kb, vb), wb in b.items():
                        if ka == kb:
                            out[(va, vb)] += wa * wb
                return out
            def addc(*counters):
                out = Counter()
                for counter in counters:
                    for row, weight in counter.items():
                        out[row] += weight
                return out
            new_a, new_b = addc(old_a, da), addc(old_b, db)
            expected = addc(joined(new_a, new_b), Counter({row: -weight for row, weight in joined(old_a, old_b).items()}))
            self.assertEqual(actual, expected)

    def test_ten_thousand_object_structural_counts(self):
        base = MerkleMap.from_items({f"object-{i:05d}": v(i) for i in range(10_000)})
        changed = base.with_changes([MapChange("object-05000", v(5000), v("new"))])
        visited, changed_leaves = diff_nodes(base, changed)
        self.assertEqual(changed_leaves, 1)
        self.assertLess(visited, 80)
        self.assertGreater(base.node_count, 150)


if __name__ == "__main__":
    unittest.main()
