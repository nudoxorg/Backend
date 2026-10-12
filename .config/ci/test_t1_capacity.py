"""Pure boundary tests for T1 capacity admission; no Cargo is started."""

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import t1_capacity


class T1Capacity(unittest.TestCase):
    def test_exact_resource_floors_are_admitted(self):
        t1_capacity.validate_capacity(
            t1_capacity.MIN_FREE_DISK_BYTES,
            t1_capacity.MIN_AVAILABLE_MEMORY_BYTES,
        )

    def test_one_byte_below_disk_floor_is_refused(self):
        with self.assertRaisesRegex(t1_capacity.CapacityError, "disk"):
            t1_capacity.validate_capacity(
                t1_capacity.MIN_FREE_DISK_BYTES - 1,
                t1_capacity.MIN_AVAILABLE_MEMORY_BYTES,
            )

    def test_one_byte_below_memory_floor_is_refused(self):
        with self.assertRaisesRegex(t1_capacity.CapacityError, "memory"):
            t1_capacity.validate_capacity(
                t1_capacity.MIN_FREE_DISK_BYTES,
                t1_capacity.MIN_AVAILABLE_MEMORY_BYTES - 1,
            )

    def test_capacity_samples_fail_closed_on_missing_or_ambiguous_memory(self):
        for sample in (
            "MemTotal: 1000 kB\n",
            "MemAvailable: 1000 kB\nMemAvailable: 2000 kB\n",
            "MemAvailable: many kB\n",
            "MemAvailable: 1000 bytes\n",
        ):
            with self.subTest(sample=sample), self.assertRaises(t1_capacity.CapacityError):
                t1_capacity.parse_mem_available(sample)

    def test_capacity_sampling_fails_closed_on_unreadable_inputs(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            missing_meminfo = root / "missing-meminfo"
            with self.assertRaisesRegex(t1_capacity.CapacityError, "unable to sample"):
                t1_capacity.sample_capacity(root, missing_meminfo)

            meminfo = root / "meminfo"
            meminfo.write_text("MemAvailable: 8388608 kB\n", encoding="ascii")
            with patch.object(t1_capacity.shutil, "disk_usage", side_effect=OSError("probe denied")):
                with self.assertRaisesRegex(t1_capacity.CapacityError, "unable to sample"):
                    t1_capacity.sample_capacity(root, meminfo)

    def test_memavailable_converts_kib_to_bytes(self):
        self.assertEqual(t1_capacity.parse_mem_available("MemAvailable: 8192 kB\n"), 8 * 1024**2)

    def test_sample_uses_nearest_existing_target_filesystem(self):
        with tempfile.TemporaryDirectory() as scratch:
            target = Path(scratch) / "not-created" / "target"
            meminfo = Path(scratch) / "meminfo"
            meminfo.write_text("MemAvailable: 8388608 kB\n", encoding="ascii")
            with patch.object(
                t1_capacity.shutil,
                "disk_usage",
                return_value=type("Usage", (), {"free": t1_capacity.MIN_FREE_DISK_BYTES})(),
            ) as disk_usage:
                self.assertEqual(
                    t1_capacity.sample_capacity(target, meminfo),
                    (t1_capacity.MIN_FREE_DISK_BYTES, t1_capacity.MIN_AVAILABLE_MEMORY_BYTES),
                )
            disk_usage.assert_called_once_with(Path(scratch))

    def test_t1_target_must_not_overlap_fast_cache_eviction_root(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            fast = root / "fast"
            for target in (fast, fast / "t1", root):
                with self.subTest(target=target), self.assertRaisesRegex(
                    t1_capacity.CapacityError, "disjoint trees"
                ):
                    t1_capacity.validate_cache_separation(target, str(fast))

            t1_capacity.validate_cache_separation(root / "t1" / "target", str(fast))
            t1_capacity.validate_cache_separation(root / "target", str(fast))
            for t1_root in (fast, root):
                with self.subTest(t1_root=t1_root), self.assertRaisesRegex(
                    t1_capacity.CapacityError, "disjoint trees"
                ):
                    t1_capacity.validate_cache_separation(
                        root / "t1" / "target", str(fast), str(t1_root)
                    )


if __name__ == "__main__":
    unittest.main()
