import os
import json
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent / "report_routes"))
import route


def wait_gone(pid, timeout=4):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.05)
    raise AssertionError(f"process {pid} did not retire")


class RouteCleanupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="route-cleanup-test-")
        base = Path(self.temp.name)
        self.bin_dir = base / "bin"
        self.bin_dir.mkdir()
        self.out = base / "out"
        self.out.mkdir()
        self.route = route.Route(self.bin_dir, self.out)

    def tearDown(self):
        if self.route.root.exists():
            self.route.stop()
        self.temp.cleanup()

    def test_stop_retires_exited_group_leader_descendant_and_preserves_substring_peer(self):
        if not (hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal")):
            self.skipTest("Linux pidfd is required to retire descendants after their leader exits")
        workspace_lookalike = str(self.route.workspace) + ".unrelated"
        unrelated = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(60)", workspace_lookalike],
            start_new_session=True,
        )
        leader = subprocess.Popen(
            [sys.executable, "-c", (
                "import subprocess,sys; "
                "child=subprocess.Popen([sys.executable,'-c',"
                "'import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print(\"READY\",flush=True); time.sleep(60)'],"
                "stdout=subprocess.PIPE,text=True); child.stdout.readline(); "
                "print(child.pid,flush=True)"
            )],
            stdout=subprocess.PIPE, text=True, start_new_session=True,
        )
        self.route._remember_spawned_process(leader)
        child_pid = int(leader.stdout.readline())
        leader.stdout.close()
        leader.wait(timeout=4)  # The group leader is gone; its child remains.
        self.route.owner = leader

        self.route.stop()

        wait_gone(child_pid)
        self.assertEqual(unrelated.poll(), None, "workspace substring must not select an unrelated process")
        self.assertFalse(self.route.root.exists(), "private fixture is retired after proven group cleanup")
        unrelated.terminate()
        unrelated.wait(timeout=4)

    def test_stop_retires_live_owned_group_on_posix(self):
        leader = subprocess.Popen(
            [sys.executable, "-c", (
                "import subprocess,sys,time; "
                "child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); "
                "print(child.pid,flush=True); time.sleep(60)"
            )],
            stdout=subprocess.PIPE, text=True, start_new_session=True,
        )
        self.route._remember_spawned_process(leader)
        child_pid = int(leader.stdout.readline())
        leader.stdout.close()
        self.route.owner = leader
        self.route.stop()
        wait_gone(child_pid)
        self.assertFalse(self.route.root.exists())

    def test_spawn_identity_failure_refuses_numeric_group_signal(self):
        leader = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            start_new_session=True,
        )
        identity = {"leader_fd": None, "unproved": True}
        proof, error = route._retire_spawned_group(leader, identity, grace=0.1, timeout=1)
        self.assertIsNone(proof)
        self.assertIn("refusing to signal", error)
        self.assertIsNone(leader.poll(), "failed identity proof must leave the process untouched")
        leader.terminate()
        leader.wait(timeout=4)

    def test_retire_for_reopen_proves_exit_closes_pidfd_and_reuses_secret(self):
        if not (hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal")):
            self.skipTest("Linux pidfd is required to verify reopen retirement and descriptor closure")
        owner_image = self.bin_dir / "backend-locald"
        owner_image.write_text(
            "#!/usr/bin/env python3\n"
            "import pathlib,sys,time\n"
            "pathlib.Path(sys.argv[sys.argv.index('--endpoint')+1]).touch()\n"
            "time.sleep(60)\n"
        )
        owner_image.chmod(0o755)
        project = self.out / "project"
        project.mkdir()
        self.route.start_owner(project)
        old_proc = self.route.owner
        old_pid = old_proc.pid
        old_fd = self.route.spawned_processes[old_pid]["leader_fd"]
        secret = (self.route.workspace / "authority.secret").read_bytes()

        proofs = self.route.retire_for_reopen()

        self.assertTrue(any(item["proof"] == "owned-popen-session-pidfds" for item in proofs))
        self.assertIsNone(self.route.owner)
        self.assertNotIn(old_pid, self.route.spawned_processes)
        with self.assertRaises(OSError):
            os.fstat(old_fd)
        wait_gone(old_pid)
        self.assertTrue(self.route.root.exists(), "reopen retirement preserves the private route state")
        self.route.endpoint.unlink()

        self.route.start_owner(project)

        self.assertNotEqual(self.route.owner.pid, old_pid)
        self.assertEqual((self.route.workspace / "authority.secret").read_bytes(), secret)

    def test_authority_secret_is_reused_and_invalid_existing_file_is_refused(self):
        owner_image = self.bin_dir / "backend-locald"
        owner_image.write_text(
            "#!/usr/bin/env python3\n"
            "import pathlib,sys,time\n"
            "args=sys.argv\n"
            "endpoint=args[args.index('--endpoint')+1]\n"
            "pathlib.Path(endpoint).touch()\n"
            "time.sleep(60)\n"
        )
        owner_image.chmod(0o755)
        project = self.out / "project"
        project.mkdir()
        self.route.start_owner(project)
        secret = self.route.workspace / "authority.secret"
        first = secret.read_bytes()

        self.route.retire_for_reopen()
        self.route.endpoint.unlink()
        self.route.start_owner(project)

        self.assertEqual(secret.read_bytes(), first)
        self.assertEqual(secret.stat().st_nlink, 1)
        self.assertEqual(secret.stat().st_mode & 0o077, 0)

        hardlink = self.route.workspace / "authority-copy"
        os.link(secret, hardlink)
        with self.assertRaises(RuntimeError):
            route.Route._ensure_authority_secret(secret)
        fifo = self.route.workspace / "authority-fifo"
        os.mkfifo(fifo, 0o600)
        with self.assertRaises(RuntimeError):
            route.Route._ensure_authority_secret(fifo)

    def test_autostart_owner_is_retired_only_with_kernel_identity_support(self):
        if not (hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal")):
            self.skipTest("Linux pidfd is required to test detached owner retirement")
        self.route.auto_start = True
        owner_image = self.bin_dir / "backend-locald"
        owner_image.symlink_to(Path(sys.executable).resolve())
        endpoint = self.route.root / "XDG_RUNTIME_DIR" / ("backend-v2-" + "a" * 24 + ".sock")
        workspace = self.route.workspace
        owner_code = (
            "import pathlib,sys,time; "
            "pathlib.Path(sys.argv[sys.argv.index('--endpoint')+1]).touch(); time.sleep(60)"
        )
        cli_image = self.bin_dir / "backend-cli"
        cli_image.write_text(
            "#!/usr/bin/env python3\n"
            "import json,os,subprocess,sys\n"
            f"open({str(self.out / 'cli-argv.json')!r},'w').write(json.dumps(sys.argv[1:]))\n"
            f"owner={str(owner_image.resolve())!r}\n"
            f"endpoint={str(endpoint)!r}\n"
            f"workspace={str(workspace)!r}\n"
            f"code={owner_code!r}\n"
            "subprocess.Popen([owner,'-c',code,'--endpoint',endpoint,'--workspace',workspace,"
            "'--authority-secret-file',workspace+'/authority.secret'],env=os.environ,"
            "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,"
            "start_new_session=True)\n"
            "print('{}')\n"
        )
        cli_image.chmod(0o755)
        project = self.out / "project"
        project.mkdir()
        self.route.cli("autostart", ["status"], project)
        self.assertNotIn("--endpoint", json.loads((self.out / "cli-argv.json").read_text()),
                         "the client request must exercise the runtime default endpoint")
        self.assertTrue(self.route.autostart_owners, "the exact private owner identity was pinned")
        self.route.stop()
        self.assertFalse(self.route.root.exists())
        self.assertTrue(any(p["proof"].startswith("linux-pidfd") for p in self.route.records[-1]["retired"]))

    def test_empty_autostart_discovery_retains_private_fixture(self):
        self.route.auto_start = True
        self.route.autostart_attempted = True
        self.route.stop()
        self.assertTrue(self.route.root.exists(), "empty process discovery is not retirement proof")
        self.assertTrue(self.route.records[-1]["unretired"])
        shutil.rmtree(self.route.root, ignore_errors=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
