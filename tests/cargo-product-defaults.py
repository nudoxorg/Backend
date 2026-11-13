#!/usr/bin/env python3
"""Check effective product-build features without compiling the GUI."""
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]


def cargo(*arguments):
    return subprocess.check_output(['cargo', *arguments], cwd=ROOT, text=True)


class ProductBuildTests(unittest.TestCase):
    def test_plain_build_keeps_qa_features_out_and_workspace_qa_stays_available(self):
        metadata = json.loads(cargo('metadata', '--no-deps', '--locked', '--offline', '--format-version', '1'))
        names = {package['id']: package['name'] for package in metadata['packages']}
        self.assertEqual({names[identity] for identity in metadata['workspace_default_members']},
                         {'backend-cli', 'backend-mcp', 'backend-locald', 'backend-desktop'})
        self.assertIn('backend-gui-harness', {names[identity] for identity in metadata['workspace_members']})
        arguments = ('tree', '--locked', '--offline', '--edges', 'normal,build,features',
                     '--invert', 'gpui-ce', '--depth', '4')
        product = cargo(*arguments)
        self.assertNotIn('gpui-ce feature "test-support"', product)
        self.assertNotIn('backend-desktop feature "visual-harness"', product)
        self.assertNotIn('backend-gui-harness', product)
        # Control: QA still resolves its real feature graph explicitly. The
        # product assertion would fail for this graph, as it did before the fix.
        workspace = cargo(*arguments, '--workspace')
        self.assertIn('gpui-ce feature "test-support"', workspace)
        self.assertIn('backend-gui-harness', workspace)


if __name__ == '__main__':
    unittest.main()
