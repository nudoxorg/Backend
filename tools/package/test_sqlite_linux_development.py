import json
from pathlib import Path
import unittest
from unittest.mock import patch

import sqlite_linux_development as phase


class OwnerCustodyTests(unittest.TestCase):
    def setUp(self):
        self.source = Path('/checkout')
        self.images = {name: {'path': '/' + name, 'bytes': 3, 'sha256': name}
                       for name in ('worker', 'dependency', 'observer')}
        self.record = {'schema': phase.SCHEMA, 'source': {'commit': 'abc'},
                       'sqlite_lock_sha256': 'lock', **self.images, 'tools': {},
                       'dependency_source': {'path': '/source-lib', 'bytes': 2, 'sha256': 'lib'},
                       'worker_raw': {'path': '/raw-worker', 'bytes': 4, 'sha256': 'lock'},
                       'artifact_identity': 'anchor', 'basis_json': 'basis'}

    def run_check(self, *, changed=None, observation=('anchor', 'basis')):
        def artifact(path):
            image = self.images[path.name].copy()
            if path.name == changed:
                image['sha256'] = 'changed'
            return image
        def digest(path, maximum):
            return (2, 'lib') if path == Path('/source-lib') else (4, 'lock')
        with patch.object(phase.common, 'clean_source_snapshot', return_value={'commit': 'abc'}), \
             patch.object(phase.common, 'artifact_identity', side_effect=artifact), \
             patch.object(phase.package, 'admit_file_digest', side_effect=digest), \
             patch.object(phase.elf, 'bounded_process', return_value=json.dumps({}).encode()), \
             patch.object(phase.sqlite, 'validate_observation', return_value=observation):
            phase.verify_after_owner(self.record, source=self.source, env={}, deadline=100)

    def test_rechecks_paired_artifacts_and_basis(self):
        self.run_check()
        for changed in ('worker', 'dependency', 'observer'):
            with self.subTest(changed=changed), self.assertRaises(Exception):
                self.run_check(changed=changed)
        with self.assertRaises(Exception):
            self.run_check(observation=('other-anchor', 'basis'))
        with self.assertRaises(Exception):
            self.run_check(observation=('anchor', 'other-basis'))


if __name__ == '__main__':
    unittest.main()
