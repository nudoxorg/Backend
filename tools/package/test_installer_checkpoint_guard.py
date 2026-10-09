"""Real file and two-process controls for standalone installer admission."""
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import time
import unittest

import test_install_preview as preview

mac, linux = preview.mac, preview.linux

ROOT = Path(__file__).parent


def marker(platform, date="20261009", source="a" * 40):
    prefix = "nudox-macos-arm64-" if platform == "macos-arm64" else "nudox-linux-x86_64-"
    return dict(version="0.0.0", tag=f"checkpoint-{date}-{source[:10]}-{platform}",
                source_sha=source, asset=prefix + source[:10] + ".tar.gz", sha256="c" * 64)


class GuardTests(unittest.TestCase):
    def test_existing_shared_install_parents_are_rejected_without_chmod(self):
        for module in (mac, linux):
            for relative in ("", "lib", "lib/nudox"):
                with self.subTest(platform=module.PLATFORM, relative=relative), tempfile.TemporaryDirectory() as temporary:
                    prefix = Path(temporary).resolve() / "prefix"
                    (prefix / "lib/nudox").mkdir(parents=True, mode=0o700)
                    directory = prefix / relative; directory.chmod(0o775)
                    with self.assertRaisesRegex(module.InstallError, "private owned directory"):
                        module.install(prefix, {}, {}, prefix / "not-downloaded.tar.gz")
                    self.assertEqual(directory.stat().st_mode & 0o777, 0o775)
                    self.assertFalse((prefix / "lib/nudox/.install.lock").exists())

    def test_unknown_malformed_and_shared_active_markers_cannot_be_overridden(self):
        for module in (mac, linux):
            for failure in ("unknown", "date", "source", "asset", "extra", "hardlink", "symlink", "writable", "oversize"):
                with self.subTest(platform=module.PLATFORM, failure=failure), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary).resolve(); active = root / "active"; active.mkdir()
                    current = root / "current"; current.symlink_to(active)
                    installed = marker(module.PLATFORM); incoming = marker(module.PLATFORM, "20261010", "b" * 40)
                    if failure == "unknown": installed["tag"] = "unknown-release"
                    if failure == "date": installed["tag"] = installed["tag"].replace("20261009", "20261340")
                    if failure == "source": installed["source_sha"] = "d" * 40
                    if failure == "asset": installed["asset"] = "wrong.tar.gz"
                    if failure == "extra": installed["extra"] = True
                    path = active / ".installed-release.json"; path.write_text(json.dumps(installed)); path.chmod(0o600)
                    if failure == "hardlink": os.link(path, root / "other-link")
                    if failure == "symlink": path.rename(root / "outside"); path.symlink_to(root / "outside")
                    if failure == "writable": path.chmod(0o666)
                    if failure == "oversize": path.write_bytes(b" " * (16 * 1024 + 1))
                    with self.assertRaises(module.InstallError): module.check_checkpoint_update(current, incoming, True)

    def test_linux_stable_marker_is_recognized_but_checkpoint_transition_is_explicit(self):
        with tempfile.TemporaryDirectory() as temporary:
            active = Path(temporary).resolve(); installed = marker("linux-x64")
            installed.update(version="0.2.0", tag="v0.2.0")
            (active / ".installed-release.json").write_text(json.dumps(installed))
            incoming = marker("linux-x64", "20261010", "b" * 40)
            with self.assertRaisesRegex(linux.InstallError, "unordered"): linux.check_checkpoint_update(active, incoming)
            linux.check_checkpoint_update(active, incoming, True)
            with self.assertRaises(mac.InstallError): mac._validate_marker(installed)

    def test_lease_rejects_shared_or_writable_files_and_releases_on_exception(self):
        for module in (mac, linux):
            for kind in ("hardlink", "writable", "exception"):
                with self.subTest(platform=module.PLATFORM, kind=kind), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary); lock = root / ".install.lock"; lock.touch(mode=0o600)
                    if kind == "hardlink": os.link(lock, root / "other-link")
                    if kind == "writable": lock.chmod(0o666)
                    if kind != "exception":
                        with self.assertRaises(module.InstallError):
                            with module._install_lease(root): self.fail("unsafe lease admitted")
                    else:
                        with self.assertRaisesRegex(RuntimeError, "control"):
                            with module._install_lease(root): raise RuntimeError("control")
                        with module._install_lease(root): pass

    def test_actual_two_python_installers_serialize_admission_through_publication(self):
        child_source = r'''
import importlib.util, json, pathlib, sys, time
spec=importlib.util.spec_from_file_location('installer',sys.argv[1]); module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
role,prefix,entry,manifest,archive,signal,release=sys.argv[2:]
original=module.safe_extract
if role=='newer':
 def extract(*args):
  pathlib.Path(signal).write_text('holding install lease in extraction')
  deadline=time.monotonic()+10
  while not pathlib.Path(release).exists():
   assert time.monotonic()<deadline
   time.sleep(.01)
  return original(*args)
 module.safe_extract=extract
print('START',flush=True)
try: module.install(pathlib.Path(prefix),json.loads(entry),json.loads(manifest),pathlib.Path(archive))
except module.InstallError as error: print(str(error),flush=True); sys.exit(3)
'''
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(); prefix = root / "prefix"
            def package(date, source):
                directory = root / date; directory.mkdir()
                archive = preview.Tests().archive(directory, source=source)
                import hashlib
                record = marker("macos-arm64", date, source)
                entry = {key: value for key, value in record.items() if key != "sha256"}
                return entry, dict(sha256=hashlib.sha256(archive.read_bytes()).hexdigest()), archive
            initial = package("20261009", "a" * 40); newer = package("20261011", "b" * 40); older = package("20261010", "c" * 40)
            mac.install(prefix, *initial); script = root / "child.py"; script.write_text(child_source)
            signal = root / "staging"; release = root / "release"; children = []
            def spawn(role, candidate):
                entry, manifest, archive = candidate
                child = subprocess.Popen([sys.executable, str(script), str(ROOT / 'install_macos_preview.py'), role,
                    str(prefix), json.dumps(entry), json.dumps(manifest), str(archive), str(signal), str(release)],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                children.append(child); return child
            try:
                first = spawn("newer", newer); deadline = time.monotonic() + 5
                while not signal.exists():
                    self.assertIsNone(first.poll()); self.assertLess(time.monotonic(), deadline); time.sleep(.01)
                second = spawn("older", older)
                self.assertTrue(select.select([second.stdout], [], [], 5)[0]); self.assertEqual(second.stdout.readline().strip(), "START")
                time.sleep(.15); self.assertIsNone(second.poll())
                release.touch(); out1, err1 = first.communicate(timeout=10); out2, err2 = second.communicate(timeout=10)
                self.assertEqual(first.returncode, 0, err1); self.assertEqual(second.returncode, 3, err2)
                self.assertIn("refusing older", out2)
                current = prefix / "lib/nudox/current"
                self.assertEqual(current.resolve().name, newer[0]["tag"])
            finally:
                release.touch()
                for child in children:
                    if child.poll() is None: child.kill()
                    child.communicate(timeout=10)
