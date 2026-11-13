"""Development helper orchestration tests; no .NET compiler is simulated as evidence."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import build_roslyn_development as builder


class BuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='roslyn recipe ')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'source'
        project = self.source / builder.PROJECT
        project.parent.mkdir(parents=True)
        project.write_text('<Project/>')
        project.with_name('packages.lock.json').write_text('{}')
        self.dotnet = self.root / 'dotnet'
        self.dotnet.write_text('''#!/usr/bin/env python3
import json, pathlib, sys
root = pathlib.Path(__file__).parent
with (root / 'calls.jsonl').open('a') as stream: stream.write(json.dumps(sys.argv[1:]) + '\\n')
if (root / 'fail').exists(): sys.exit(7)
if sys.argv[1] == 'publish':
    out = pathlib.Path(sys.argv[sys.argv.index('-o') + 1]); out.mkdir()
    for name in ('oracle.dll', 'oracle.deps.json', 'oracle.runtimeconfig.json', 'Microsoft.CodeAnalysis.dll', 'Microsoft.CodeAnalysis.CSharp.dll', 'extra-dependency.dll'):
        (out / name).write_text('artifact')
''')
        self.dotnet.chmod(0o755)
        self.output = self.root / 'helper output'

    def test_locked_isolated_publish_keeps_complete_closure_and_environment(self):
        environment = builder.build(self.source, self.output, self.dotnet)
        calls = [json.loads(line) for line in (self.root / 'calls.jsonl').read_text().splitlines()]
        self.assertIn('--locked-mode', calls[0])
        self.assertIn('--source', calls[0])
        self.assertIn('--no-restore', calls[1])
        self.assertIn('-p:UseSharedCompilation=false', calls[1])
        self.assertTrue(all(any(arg.startswith('-p:BaseIntermediateOutputPath=') for arg in call) for call in calls))
        self.assertTrue((self.output / 'extra-dependency.dll').is_file())
        self.assertFalse((self.source / builder.PROJECT).parent.joinpath('obj').exists())
        result = subprocess.run(['sh', '-c', '. "$1"; test "$NUDOX_DOTNET" = "$2" && test "$NUDOX_ROSLYN_HELPER" = "$3"',
                                 'sh', str(environment), str(self.dotnet.resolve()), str(self.output / 'oracle.dll')])
        self.assertEqual(result.returncode, 0)

    def test_failure_leaves_no_partial_output_and_preserves_existing_output(self):
        (self.root / 'fail').touch()
        with self.assertRaises(subprocess.CalledProcessError):
            builder.build(self.source, self.output, self.dotnet)
        self.assertFalse(self.output.exists())
        self.assertFalse(list(self.root.glob('.roslyn-build-*')))
        self.output.mkdir()
        marker = self.output / 'keep'
        marker.write_text('existing')
        with self.assertRaises(ValueError):
            builder.build(self.source, self.output, self.dotnet)
        self.assertEqual(marker.read_text(), 'existing')

    def test_downloads_are_explicit(self):
        builder.build(self.source, self.output, self.dotnet, allow_downloads=True)
        calls = [json.loads(line) for line in (self.root / 'calls.jsonl').read_text().splitlines()]
        self.assertNotIn('--source', calls[0])


if __name__ == '__main__':
    unittest.main()
