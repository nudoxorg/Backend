"""Kernel lifecycle controls for the private SQL journey harnesses."""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

from sql_journey_safety import OwnedProcesses, _retire_spawned_group, remove_private_root


class OwnedProcessTests(unittest.TestCase):
    def test_run_collects_output_and_retires_owned_child(self):
        owner = OwnedProcesses()
        result = owner.run([sys.executable, '-c', 'import time; print("ready", flush=True); time.sleep(.2)'], timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b'ready\n')
        self.assertFalse(owner.owned)
        self.assertEqual(len(owner.proofs), 1)

    def test_crash_retires_only_owned_live_child(self):
        owner = OwnedProcesses()
        unrelated = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(20)'])
        try:
            process = owner.spawn([sys.executable, '-c', 'import time; time.sleep(20)'])
            if owner.subreaper:
                with self.assertRaisesRegex(RuntimeError, 'untracked child'):
                    owner.retire(process, crash=True)
            else:
                owner.retire(process, crash=True)
            self.assertIsNotNone(process.returncode)
            self.assertIsNone(unrelated.poll(), 'unrelated child must not be signalled')
            self.assertFalse(owner.owned)
        finally:
            unrelated.terminate()
            unrelated.wait(timeout=5)
        if owner.subreaper:
            self.assertEqual(owner.retire_all(), [])

    def test_retired_leader_never_receives_numeric_group_signal(self):
        process = mock.Mock(pid=12345, returncode=0)
        with mock.patch('sql_journey_safety._wait_pidfd', return_value=True), \
             mock.patch('sql_journey_safety._linux_group_members', return_value=[]), \
             mock.patch('sql_journey_safety.os.killpg') as signal_group:
            proof, error = _retire_spawned_group(process, {'leader_fd': 10})
        self.assertIsNone(error)
        self.assertEqual(proof['pid'], 12345)
        signal_group.assert_not_called()

    def test_unproved_identity_neither_signals_nor_authorizes_cleanup(self):
        process = mock.Mock(pid=12345)
        with mock.patch('sql_journey_safety.os.killpg') as signal_group:
            proof, error = _retire_spawned_group(process, {'unproved': True})
        self.assertIsNone(proof)
        self.assertIn('identity', error)
        signal_group.assert_not_called()

    def test_missing_build_receipt_preserves_failure_without_starting_owner(self):
        checkout = Path(__file__).resolve().parents[2]
        script = checkout / 'tools/package/sql_minimal_publication.py'
        if not script.is_file():
            self.skipTest('full source checkout required')
        with tempfile.TemporaryDirectory() as base:
            root = Path(base)
            bundle = root / 'bundle'
            bundle.mkdir()
            out = root / 'output'
            result = subprocess.run([sys.executable, str(script), '--bundle', str(bundle), '--output', str(out),
                                     '--build-receipt', str(root / 'missing.json'), '--source-checkout', str(checkout)],
                                    capture_output=True, timeout=15)
            self.assertNotEqual(result.returncode, 0)
            receipt = json.loads((out / 'receipt.json').read_text())
            self.assertEqual(receipt['result'], 'failed')
            self.assertFalse(receipt['cleanup']['removed'])
            self.assertFalse(receipt['commands'])
            private = Path(receipt['private_root'])
            self.assertEqual(private.parent.resolve(), root.resolve())
            self.assertTrue(private.is_dir())
            self.assertFalse((private / 'state' / 'authority.secret').exists())

    def test_both_entrypoints_record_preflight_and_private_setup_failures(self):
        checkout = Path(__file__).resolve().parents[2]
        scripts = [checkout / 'tools/package/sql_public_journey.py', checkout / 'tools/package/sql_minimal_publication.py']
        if not all(script.is_file() for script in scripts):
            self.skipTest('full source checkout required')
        sources = scripts + [checkout / 'tools/package/sql_journey_safety.py']
        before = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources}
        wrapper = r"""import os,pathlib,runpy,sys,tempfile
script,mode,bundle,output,checkout,private_tmp = sys.argv[1:]
sys.path.insert(0,str(pathlib.Path(script).parent))
import sql_journey_safety as safety
if mode == 'custody':
    def refuse(): raise RuntimeError('simulated custody refusal')
    safety.OwnedProcesses = refuse
elif mode == 'secret':
    safety.verified_build = lambda *args: {'test_receipt': 'admitted'}
    original = pathlib.Path.write_bytes
    def write_then_refuse(path, value):
        result = original(path, value)
        if path.name == 'authority.secret': raise OSError('simulated secret setup failure')
        return result
    pathlib.Path.write_bytes = write_then_refuse
os.environ['TMPDIR'] = private_tmp
tempfile.tempdir = private_tmp
sys.argv = [script,'--bundle',str(pathlib.Path(bundle)/'missing') if mode == 'bundle' else bundle,'--output',output,'--build-receipt',str(pathlib.Path(private_tmp)/'missing-receipt.json'),'--source-checkout',checkout]
runpy.run_path(script,run_name='__main__')
"""
        with tempfile.TemporaryDirectory() as base:
            root = Path(base).resolve()
            bundle = root / 'bundle'
            bundle.mkdir()
            for name in ('backend-cli', 'backend-locald', 'backend-mcp'):
                (bundle / name).write_bytes(b'fixture never executed')
            for script in scripts:
                for mode in ('bundle', 'receipt', 'custody', 'secret'):
                    with self.subTest(script=script.name, mode=mode):
                        out = root / (script.stem + '-' + mode)
                        result = subprocess.run([sys.executable, '-c', wrapper, str(script), mode,
                                                 str(bundle), str(out), str(checkout), str(root)],
                                                capture_output=True, timeout=15)
                        self.assertNotEqual(result.returncode, 0, result.stdout.decode(errors='replace'))
                        path = out / ('journey.json' if script.name == 'sql_public_journey.py' else 'receipt.json')
                        saved = json.loads(path.read_text())
                        if isinstance(saved, list):
                            failures = [item for item in saved if 'failure' in item]
                            cleanup = [item['cleanup'] for item in saved if 'cleanup' in item][-1]
                            self.assertTrue(failures)
                            private = failures[-1]['private_root']
                            message = failures[-1]['failure']
                            self.assertFalse(any('argv' in item for item in saved), 'owner/CLI must not launch')
                        else:
                            self.assertEqual(saved['result'], 'failed')
                            cleanup = saved['cleanup']
                            private = saved['private_root']
                            message = saved['error']
                            self.assertFalse(saved['commands'], 'owner/CLI must not launch')
                        self.assertFalse(cleanup['removed'], 'failure must not claim fixture removal')
                        if mode == 'custody': self.assertIn('simulated custody refusal', message)
                        if mode == 'secret':
                            self.assertIn('simulated secret setup failure', message)
                            self.assertIsNotNone(private)
                            secret = Path(private) / 'state' / 'authority.secret'
                            self.assertTrue(secret.is_file(), 'allocated private fixture and secret must be retained')
                        elif private is not None:
                            self.assertTrue(Path(private).is_dir())
                        self.assertFalse(list(out.glob('owner-*.stderr')), 'owner must not launch')
            for script in scripts:
                existing = root / (script.stem + '-existing')
                existing.mkdir()
                marker = existing / 'keep.txt'
                marker.write_text('existing data')
                result = subprocess.run([sys.executable, str(script), '--bundle', str(bundle), '--output', str(existing),
                                         '--build-receipt', str(root / 'missing.json'), '--source-checkout', str(checkout)],
                                        capture_output=True, timeout=15)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(marker.read_text(), 'existing data')
                self.assertEqual(sorted(path.name for path in existing.iterdir()), ['keep.txt'])
            self.assertEqual(before, {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources},
                             'preflight failures must not mutate source files')

    def test_private_root_identity_must_match_before_removal(self):
        with tempfile.TemporaryDirectory() as base:
            root = Path(base) / 'private'
            root.mkdir()
            (root / 'authority.secret').write_bytes(b'private')
            info = root.stat()
            with self.assertRaisesRegex(RuntimeError, 'identity'):
                remove_private_root(root, (info.st_dev, info.st_ino + 1))
            self.assertTrue((root / 'authority.secret').exists())
            remove_private_root(root, (info.st_dev, info.st_ino))
            self.assertFalse(root.exists())

    @unittest.skipUnless(hasattr(os, 'pidfd_open') and hasattr(signal, 'pidfd_send_signal'),
                         'Linux pidfd required to prove post-leader descendant retirement')
    def test_linux_child_survives_leader_until_owned_retirement(self):
        owner = OwnedProcesses()
        with tempfile.TemporaryDirectory() as base:
            marker = Path(base) / 'child.pid'
            code = '''import os,sys,time
pid=os.fork()
if pid == 0:
    open(sys.argv[1], 'w').write(str(os.getpid()))
    time.sleep(30)
else:
    time.sleep(.4)
'''
            process = owner.spawn([sys.executable, '-c', code, str(marker)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.02)
            self.assertTrue(marker.exists(), 'descendant must start')
            while not owner.exited(process) and time.monotonic() < deadline:
                time.sleep(.02)
            self.assertTrue(owner.exited(process), 'leader must exit before retirement')
            owner.retire(process)
            self.assertFalse(owner.owned)
            self.assertEqual(process.returncode, 0)
            self.assertTrue(any(proof['proof'] == 'owned-popen-session-pidfds' for proof in owner.proofs))
            self.assertEqual(owner.retire_all(), [])

    @unittest.skipUnless(sys.platform.startswith('linux') and hasattr(os, 'pidfd_open') and hasattr(signal, 'pidfd_send_signal'),
                         'Linux subreaper and pidfd required')
    def test_linux_setsid_escape_is_adopted_and_retired(self):
        owner = OwnedProcesses()
        with tempfile.TemporaryDirectory() as base:
            marker = Path(base) / 'escaped.pid'
            code = """import os,sys,time
pid=os.fork()
if pid == 0:
    os.setsid()
    open(sys.argv[1], 'w').write(str(os.getpid()))
    time.sleep(30)
else:
    time.sleep(.4)
"""
            process = owner.spawn([sys.executable, '-c', code, str(marker)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.02)
            self.assertTrue(marker.exists(), 'escaped child must start')
            while not owner.exited(process) and time.monotonic() < deadline:
                time.sleep(.02)
            self.assertTrue(owner.exited(process), 'leader must exit first')
            owner.retire(process)
            failures = owner.retire_all()
            self.assertEqual(failures, [])
            self.assertTrue(any(proof['proof'] == 'adopted-token-pidfd' for proof in owner.proofs), owner.proofs)
            with self.assertRaises(ProcessLookupError):
                os.kill(int(marker.read_text()), 0)

    @unittest.skipUnless(sys.platform.startswith('linux') and hasattr(os, 'pidfd_open') and hasattr(signal, 'pidfd_send_signal'),
                         'Linux subreaper and pidfd required')
    def test_linux_foreign_child_is_never_signalled_and_blocks_cleanup(self):
        owner = OwnedProcesses()
        foreign = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        try:
            failures = owner.retire_all()
            self.assertTrue(failures, 'untracked child must block deletion')
            self.assertIsNone(foreign.poll(), 'foreign child must remain alive')
        finally:
            foreign.terminate()
            foreign.wait(timeout=5)
        self.assertEqual(owner.retire_all(), [])

    @unittest.skipIf(sys.platform.startswith('linux'), 'Darwin/non-Linux refusal')
    def test_non_linux_never_claims_complete_descendant_retirement(self):
        owner = OwnedProcesses()
        failures = owner.retire_all()
        self.assertTrue(failures)
        self.assertEqual(failures[0]['kind'], 'descendant-proof')


if __name__ == '__main__':
    unittest.main()
