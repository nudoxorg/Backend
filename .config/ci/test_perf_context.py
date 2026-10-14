"""Missing/large/partial proc data must stay bounded and observational."""
from pathlib import Path
import json
import tempfile
import unittest
from unittest.mock import patch

import perf_context


class ContextTests(unittest.TestCase):
    def test_missing_files_and_oversized_pressure_are_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'cpu.pressure').write_text('x' * 4096)
            (root / 'cpuinfo').write_text('model name : fixture\ncpu MHz : NaN\ncpu MHz : bad\ncpu MHz : 1000\ncpu MHz : 2000\n')
            with patch.dict('os.environ', {'FORGEJO_TOKEN': 'private-token'}):
                context = perf_context.system(root, root)
            self.assertEqual(context['cpu_model'], 'fixture')
            self.assertEqual(context['observed_mhz'], {'min': 1000, 'max': 2000, 'samples': 2})
            self.assertEqual(context['files']['cpu_max'], {'available': False})
            self.assertTrue(context['files']['cpu_pressure']['truncated'])
            self.assertEqual(len(context['files']['cpu_pressure']['text']), perf_context.LIMIT)
            self.assertNotIn('private-token', json.dumps(context))
            self.assertLess(len(json.dumps(context)), 32768)

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
