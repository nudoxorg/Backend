"""Missing/large/partial proc data must stay bounded and observational."""
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import perf_context


class ContextTests(unittest.TestCase):
    def test_missing_files_and_oversized_pressure_are_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mount = root / 'cgroup'
            own = mount / 'job/step'
            own.mkdir(parents=True)
            (own / 'cgroup.procs').write_text(f'{os.getpid()}\n')
            (mount / 'cpu.max').write_text('90000 100000\n')
            (own / 'cpu.max').write_text('50000 100000\n')
            (own / 'cpu.pressure').write_text('x' * 4096)
            (root / 'self').mkdir()
            (root / 'self/cgroup').write_text('0::/job/step\n')
            (root / 'self/mountinfo').write_text(
                f'29 23 0:26 / {mount} rw,nosuid - cgroup2 cgroup rw\n')
            (root / 'cpuinfo').write_text('model name : fixture\ncpu MHz : NaN\ncpu MHz : bad\ncpu MHz : 1000\ncpu MHz : 2000\n')
            with patch.dict('os.environ', {'FORGEJO_TOKEN': 'private-token'}):
                context = perf_context.system(root, mount)
            self.assertEqual(context['cpu_model'], 'fixture')
            self.assertEqual(context['observed_mhz'], {'min': 1000, 'max': 2000, 'samples': 2})
            self.assertEqual(context['cgroup_scope'], {
                'available': True, 'path': '/job/step', 'mount_root': '/',
                'verified_by': 'cgroup.procs'})
            self.assertEqual(context['files']['cpu_max']['text'], '50000 100000')
            self.assertTrue(context['files']['cpu_pressure']['truncated'])
            self.assertEqual(len(context['files']['cpu_pressure']['text']), perf_context.LIMIT)
            self.assertNotIn('private-token', json.dumps(context))
            self.assertLess(len(json.dumps(context)), 32768)

    def test_cgroup_namespace_root_maps_to_the_visible_mount_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'self').mkdir()
            (root / 'self/cgroup').write_text('0::/\n')
            (root / 'self/mountinfo').write_text(
                f'29 23 0:26 /host/worker {root} rw,nosuid - cgroup2 cgroup rw\n')
            (root / 'cpu.max').write_text('25000 100000\n')
            (root / 'cgroup.procs').write_text(f'{os.getpid()}\n')
            context = perf_context.system(root, root)
            self.assertEqual(context['cgroup_scope'], {
                'available': True, 'path': '/', 'mount_root': '/host/worker',
                'verified_by': 'cgroup.procs'})
            self.assertEqual(context['files']['cpu_max']['text'], '25000 100000')

    def test_mountinfo_octal_escaping_is_decoded_before_mount_matching(self):
        with tempfile.TemporaryDirectory(prefix='cgroup mount ') as directory:
            root = Path(directory)
            (root / 'self').mkdir()
            (root / 'self/cgroup').write_text('0::/worker\n')
            escaped_mount = str(root).replace(' ', '\\040')
            (root / 'self/mountinfo').write_text(
                f'29 23 0:26 / {escaped_mount} rw,nosuid - cgroup2 cgroup rw\n')
            worker = root / 'worker'
            worker.mkdir()
            (worker / 'cgroup.procs').write_text(f'{os.getpid()}\n')
            (worker / 'cpu.max').write_text('75000 100000\n')
            context = perf_context.system(root, root)
            self.assertEqual(context['files']['cpu_max']['text'], '75000 100000')

    def test_bind_mount_root_without_the_current_process_is_not_attributed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'self').mkdir()
            (root / 'self/cgroup').write_text('0::/\n')
            (root / 'self/mountinfo').write_text(
                f'29 23 0:26 /host/worker {root} rw,nosuid - cgroup2 cgroup rw\n')
            (root / 'cpu.max').write_text('not-the-process\n')
            (root / 'cgroup.procs').write_text(f'{os.getpid() + 1}\n')
            context = perf_context.system(root, root)
            self.assertEqual(context['cgroup_scope'], {
                'available': False, 'reason': 'process-membership-unverified'})
            self.assertEqual(context['files']['cpu_max'], {'available': False})

    def test_ambiguous_or_malformed_cgroup_membership_never_falls_back_to_mount_root(self):
        for membership, expected in [
            ('0::/../../host\n', 'membership-invalid'),
            ('0::/host-worker\n', 'membership-outside-mount-root'),
            ('', 'membership-ambiguous'),
        ]:
            with self.subTest(membership=membership), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'self').mkdir()
                (root / 'self/cgroup').write_text(membership)
                (root / 'self/mountinfo').write_text(
                    f'29 23 0:26 /worker {root} rw,nosuid - cgroup2 cgroup rw\n')
                (root / 'cpu.max').write_text('host-scope-value\n')
                context = perf_context.system(root, root)
                self.assertEqual(context['cgroup_scope'], {'available': False, 'reason': expected})
                self.assertEqual(context['files']['cpu_max'], {'available': False})

    def test_escaped_diagnostics_are_bounded_without_changing_source_or_input(self):
        phase = {'files': {key: {'text': '\x00' * 2048, 'truncated': False}
                           for key in perf_context.FILES}}
        original = {'schema': 1, 'source_sha': 'a' * 40, 'artifact_manifest_sha256': 'b' * 64,
                    'case': 'polyglot', 'start': phase, 'end': phase}
        encoded = perf_context.encode(original)
        self.assertLessEqual(len(encoded.encode('utf-8')), 65536)
        record = json.loads(encoded)
        self.assertTrue(record['observations_truncated'])
        self.assertEqual(record['source_sha'], original['source_sha'])
        self.assertEqual(record['artifact_manifest_sha256'], original['artifact_manifest_sha256'])
        self.assertEqual(len(original['start']['files']['cpu_max']['text']), 2048)

    def test_final_identity_fallback_is_utf8_bounded_and_hashes_full_case_names(self):
        for case in ('\x00' * 40000, '雪😀' * 40000, '\ud800' * 40000):
            with self.subTest(case_prefix=repr(case[:4])):
                source_sha = '\x00' * 40000
                artifact_sha = '雪😀' * 40000
                original = {'schema': 1, 'source_sha': source_sha,
                            'artifact_manifest_sha256': artifact_sha, 'case': case,
                            'start': {'files': {'cpu_max': {'text': '\x00' * 200000}}},
                            'end': {'files': {'cpu_max': {'text': '\x00' * 200000}}}}
                encoded = perf_context.encode(original)
                self.assertLessEqual(len(encoded.encode('utf-8')), 65536)
                record = json.loads(encoded)
                self.assertTrue(record['case_truncated'])
                self.assertEqual(record['case_sha256'], hashlib.sha256(
                    case.encode('utf-8', errors='surrogatepass')).hexdigest())
                self.assertEqual(record['source_sha_sha256'], hashlib.sha256(
                    source_sha.encode('utf-8', errors='surrogatepass')).hexdigest())
                self.assertEqual(record['artifact_manifest_sha256_sha256'], hashlib.sha256(
                    artifact_sha.encode('utf-8', errors='surrogatepass')).hexdigest())

    def test_thread_sample_handles_parentheses_vanished_threads_and_top_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            task = root / '123/task'
            task.mkdir(parents=True)
            for tid in range(1, 13):
                path = task / str(tid)
                path.mkdir()
                fields = ['S'] + ['0'] * 22
                fields[11], fields[12] = str(tid * 10), '10'
                (path / 'stat').write_text(f'{tid} (worker ) name) ' + ' '.join(fields))
            (task / '99').mkdir()  # disappeared before its stat could be read
            (task / '100').mkdir()
            (task / '100/stat').write_text('partial')
            record = perf_context.owner_threads(123, root, ticks=10)
            self.assertEqual(record['sampled'], 12)
            self.assertEqual(len(record['top']), 10)
            self.assertEqual(record['top'][0]['cpu_seconds'], 13)
            self.assertEqual(record['top'][0]['name'], 'worker ) name')
            self.assertEqual(perf_context.owner_threads(456, root), {'available': False})


if __name__ == '__main__':
    unittest.main()
