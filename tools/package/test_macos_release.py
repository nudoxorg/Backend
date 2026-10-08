import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import macos_release as release


class ReleasePreflightTests(unittest.TestCase):
    def test_sign_preflight_stops_at_shared_finite_entry_bound(self):
        import finalize_macos_typescript_sdk as sdk
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);source=root/'source';source.mkdir()
            app=root/'release/package/Nudox.app';app.mkdir(parents=True)
            for number in range(5):(app/str(number)).write_bytes(b'')
            config={'source_root':str(source),'output_dir':str(root/'release')}
            original=sdk.bundle.bundle_entries
            visited=[]
            def capped(path):
                for item in original(path,maximum_entries=3):
                    visited.append(item[0]);yield item
            with patch.object(sdk.bundle,'bundle_entries',side_effect=capped), patch.object(release.shutil,'which',return_value=None), patch.object(release.subprocess,'check_output',return_value=''), patch.object(release,'run') as run:
                report=release.preflight(config,stage='sign')
            self.assertFalse(report['ready']);self.assertEqual(len(visited),3)
            self.assertTrue(any('finite entry bound' in item for item in report['blockers']))
            run.assert_not_called()

    def test_missing_signing_and_capacity_stop_before_build_or_output_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            config = {"source_root": str(source), "output_dir": str(root / "release"), "expected_revision": "a" * 40, "expected_tree": "b" * 40, "signing_identity": "Developer ID Application: Example", "notary_profile": "example", "minimum_os": "26.0", "expected_runner_sha256": "c" * 64, "expected_icon_sha256": "d" * 64}
            for key in release.INPUTS:
                path = root / key
                path.write_text("test input")
                config[key] = str(path)
            def output(command, **kwargs):
                if command[0] == "security":
                    return '1) abc "Apple Development: Example"\n'
                if command[-1] == "--porcelain":
                    return ""
                return ("b" if command[-1] == "HEAD^{tree}" else "a") * 40 + "\n"
            with patch.object(release.platform, "system", return_value="Darwin"), patch.object(release.platform, "machine", return_value="arm64"), patch.object(release.shutil, "which", return_value="/usr/bin/tool"), patch.object(release.shutil, "disk_usage") as disk, patch.object(release.subprocess, "check_output", side_effect=output), patch.object(release, "run") as run:
                disk.return_value.free = 25 * 1024**3
                report = release.preflight(config)
                self.assertFalse(report["ready"])
                self.assertTrue(any("admission floor" in item for item in report["blockers"]))
                self.assertTrue(any("Developer ID" in item for item in report["blockers"]))
                self.assertTrue(any("selected Apple /usr/bin/codesign" in item for item in report["blockers"]))
                with self.assertRaisesRegex(ValueError, "preflight failed"):
                    release.prepare(config)
                run.assert_not_called()
                self.assertFalse((root / "release").exists())


if __name__ == "__main__":
    unittest.main()
