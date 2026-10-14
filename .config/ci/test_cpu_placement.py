from pathlib import Path
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import cpu_placement


class PlacementTests(unittest.TestCase):
    def write(self, root, cpu, value, metric='cpufreq/cpuinfo_max_freq'):
        path = root/f'cpu{cpu}'/metric
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(str(value))

    def test_selects_highest_complete_group_inside_existing_grant(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for cpu, value in [(0, 4800000), (1, 4800000), (2, 3500000), (3, 3500000)]:
                self.write(root, cpu, value)
            record = cpu_placement.highest_capacity({1, 2, 3}, root)
            self.assertEqual(record['selected'], [1])
            self.assertEqual(record['reason'], 'heterogeneous')
            self.assertNotIn(0, record['allowed'])

    def test_flat_kernel_capacity_still_consults_advertised_frequency(self):
        # Actual worker reports 1024 for both core groups despite 4.8/3.5GHz maxima.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for cpu, frequency in [(0, 4800000), (12, 3500000)]:
                self.write(root, cpu, 1024, 'cpu_capacity')
                self.write(root, cpu, frequency)
            record = cpu_placement.highest_capacity({0, 12}, root)
            self.assertEqual(record['metric'], 'cpufreq/cpuinfo_max_freq')
            self.assertEqual(record['selected'], [0])

    def test_kernel_capacity_takes_precedence_over_frequency(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for cpu, capacity, frequency in [(7, 1024, 100), (9, 512, 200)]:
                self.write(root, cpu, capacity, 'cpu_capacity')
                self.write(root, cpu, frequency)
            record = cpu_placement.highest_capacity({7, 9}, root)
            self.assertEqual(record['selected'], [7])
            self.assertEqual(record['metric'], 'cpu_capacity')

    def test_partial_invalid_and_homogeneous_metadata_preserves_grant(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for invalid in [None, 'bad', '0', '-1', '9'*100, 'NaN']:
                self.write(root, 2, 100)
                if invalid is not None:
                    self.write(root, 4, invalid)
                record = cpu_placement.highest_capacity({2, 4}, root)
                self.assertEqual(record['selected'], [2, 4])
                self.assertEqual(record['reason'], 'complete-static-capacity-unavailable')
            self.write(root, 4, 100)
            self.assertEqual(cpu_placement.highest_capacity({2, 4}, root)['reason'], 'homogeneous')

    def test_affinity_is_restored_even_when_measurement_fails(self):
        record = {'selected': [7], 'allowed': [7, 9]}
        with patch.object(cpu_placement.os, 'sched_getaffinity', create=True, side_effect=[{7, 9}, {7}]), \
             patch.object(cpu_placement.os, 'sched_setaffinity', create=True) as setter, \
             patch.object(cpu_placement, 'highest_capacity', return_value=record):
            with self.assertRaisesRegex(ValueError, 'measurement failed'):
                with cpu_placement.measurement_placement():
                    raise ValueError('measurement failed')
            self.assertEqual([call.args for call in setter.call_args_list], [(0, {7}), (0, {7, 9})])

    def test_refused_affinity_is_an_error_not_an_ignored_policy(self):
        original = {7, 9}
        effective = {'cpus': original}
        calls = []

        def refuse_after_partial_change(process, cpus):
            selected = set(cpus)
            calls.append((process, selected))
            effective['cpus'] = selected
            if selected == {7}:
                raise PermissionError('fixture')

        with patch.object(cpu_placement.os, 'sched_getaffinity', create=True, return_value={7, 9}), \
             patch.object(cpu_placement.os, 'sched_setaffinity', create=True,
                          side_effect=refuse_after_partial_change), \
             patch.object(cpu_placement, 'highest_capacity', return_value={'selected': [7]}):
            with self.assertRaises(PermissionError):
                with cpu_placement.measurement_placement():
                    self.fail('measurement must not run after placement refusal')
        self.assertEqual(calls, [(0, {7}), (0, original)])
        self.assertEqual(effective['cpus'], original)

    def test_verification_oserror_restores_original_grant(self):
        original = {7, 9}
        verification_error = OSError('verification failed')
        record = {'selected': [7]}
        with patch.object(cpu_placement.os, 'sched_getaffinity', create=True,
                          side_effect=[original, verification_error]), \
             patch.object(cpu_placement.os, 'sched_setaffinity', create=True) as setter, \
             patch.object(cpu_placement, 'highest_capacity', return_value=record):
            with self.assertRaises(OSError) as raised:
                with cpu_placement.measurement_placement():
                    self.fail('measurement must not run after verification fails')
        self.assertIs(raised.exception, verification_error)
        self.assertEqual([call.args for call in setter.call_args_list],
                         [(0, {7}), (0, original)])

    def test_verification_keyboardinterrupt_restores_original_grant(self):
        original = {7, 9}
        cancellation = KeyboardInterrupt('verification cancelled')
        with patch.object(cpu_placement.os, 'sched_getaffinity', create=True,
                          side_effect=[original, cancellation]), \
             patch.object(cpu_placement.os, 'sched_setaffinity', create=True) as setter, \
             patch.object(cpu_placement, 'highest_capacity', return_value={'selected': [7]}):
            with self.assertRaises(KeyboardInterrupt) as raised:
                with cpu_placement.measurement_placement():
                    self.fail('measurement must not run after verification is cancelled')
        self.assertIs(raised.exception, cancellation)
        self.assertEqual([call.args for call in setter.call_args_list],
                         [(0, {7}), (0, original)])

    def test_mismatched_effective_grant_restores_original_and_refuses_measurement(self):
        original = {7, 9}
        with patch.object(cpu_placement.os, 'sched_getaffinity', create=True,
                          side_effect=[original, {9}]), \
             patch.object(cpu_placement.os, 'sched_setaffinity', create=True) as setter, \
             patch.object(cpu_placement, 'highest_capacity', return_value={'selected': [7]}):
            with self.assertRaisesRegex(RuntimeError, 'placement was not applied'):
                with cpu_placement.measurement_placement():
                    self.fail('measurement must not run after a mismatched grant')
        self.assertEqual([call.args for call in setter.call_args_list],
                         [(0, {7}), (0, original)])

    def test_body_cancellation_restores_original_grant(self):
        original = {7, 9}
        cancellation = KeyboardInterrupt('measurement cancelled')
        with patch.object(cpu_placement.os, 'sched_getaffinity', create=True,
                          side_effect=[original, {7}]), \
             patch.object(cpu_placement.os, 'sched_setaffinity', create=True) as setter, \
             patch.object(cpu_placement, 'highest_capacity', return_value={'selected': [7]}):
            with self.assertRaises(KeyboardInterrupt) as raised:
                with cpu_placement.measurement_placement():
                    raise cancellation
        self.assertIs(raised.exception, cancellation)
        self.assertEqual([call.args for call in setter.call_args_list],
                         [(0, {7}), (0, original)])

    def test_restore_failure_is_reported_without_masking_measurement_cancellation(self):
        original = {7, 9}
        cancellation = KeyboardInterrupt('measurement cancelled')
        calls = []

        def set_affinity(process, cpus):
            calls.append((process, set(cpus)))
            if len(calls) == 2:
                raise PermissionError(1, 'restore denied')

        with patch.object(cpu_placement.os, 'sched_getaffinity', create=True,
                          side_effect=[original, {7}]), \
             patch.object(cpu_placement.os, 'sched_setaffinity', create=True,
                          side_effect=set_affinity), \
             patch.object(cpu_placement, 'highest_capacity', return_value={'selected': [7]}):
            with self.assertRaises(KeyboardInterrupt) as raised:
                with cpu_placement.measurement_placement():
                    raise cancellation
        self.assertIs(raised.exception, cancellation)
        self.assertEqual(calls, [(0, {7}), (0, original)])
        self.assertTrue(any('PermissionError (errno 1)' in note
                            for note in raised.exception.__notes__))

    @unittest.skipUnless(hasattr(os, 'sched_getaffinity'), 'Linux affinity lifecycle')
    def test_real_child_inherits_placement_and_parent_recovers(self):
        original = set(os.sched_getaffinity(0))
        selected = {min(original)}
        with patch.object(cpu_placement, 'highest_capacity', return_value={'selected': sorted(selected)}):
            with cpu_placement.measurement_placement():
                child = subprocess.check_output([sys.executable, '-c',
                    'import json,os;print(json.dumps(sorted(os.sched_getaffinity(0))))'], text=True)
                self.assertEqual(set(json.loads(child)), selected)
        self.assertEqual(set(os.sched_getaffinity(0)), original)


if __name__ == '__main__':
    unittest.main()
