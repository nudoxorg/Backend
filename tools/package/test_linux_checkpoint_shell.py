"""Execute the generated saved-file shell with transport and platform fixtures, not native QA."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import linux_release_package as package

TAG = "checkpoint-20261016-aaaaaaaaaa-linux-x64"


class CheckpointShellTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="linux checkpoint shell ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.private = self.root / "temporary with spaces"
        self.private.mkdir(mode=0o700)
        self.home = self.root / "fresh home"
        self.home.mkdir()
        self.body = self.root / "fixture-installer.py"
        self.body.write_text('''import json,os,pathlib,sys
path=pathlib.Path(os.environ['EXECUTION_LOG'])
rows=json.loads(path.read_text()) if path.exists() else []
rows.append({'argv':sys.argv[1:],'path':__file__,'saved':pathlib.Path(__file__).is_file(),'home':os.environ['HOME']})
path.write_text(json.dumps(rows))
sys.exit(int(os.environ.get('INSTALLER_STATUS','0')))
''')
        self.shell = self.root / "install.sh"
        self.shell.write_bytes(package.checkpoint_shell(TAG, self.body.read_bytes(), "2.39"))
        self.execution_log = self.root / "executions.json"
        self.curl_log = self.root / "curl.json"
        self.environment = {**os.environ, "PATH": str(self.bin) + os.pathsep + os.environ.get("PATH", ""),
                            "HOME": str(self.home), "TMPDIR": str(self.private),
                            "NUDOX_INSTALL_PYTHON": sys.executable, "PYTHONDONTWRITEBYTECODE": "1",
                            "PYTHONPATH": str(self.root), "CURL_BODY": str(self.body),
                            "CURL_LOG": str(self.curl_log), "EXECUTION_LOG": str(self.execution_log)}
        # The full shell and genuine CPython run on the test host. Only Linux/glibc
        # discovery is simulated, so these controls also run on a developer Mac.
        (self.root / "sitecustomize.py").write_text(
            'import os\nos.confstr=lambda _: os.environ.get("FIXTURE_GLIBC", "glibc 2.39")\n')
        self.script("uname", '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
        self.script("curl", "#!" + sys.executable + "\n" + '''import json,os,pathlib,sys
args=sys.argv[1:]
pathlib.Path(os.environ['CURL_LOG']).write_text(json.dumps(args))
sys.stdout.buffer.write(pathlib.Path(os.environ['CURL_BODY']).read_bytes())
sys.stdout.buffer.flush()
sys.exit(int(os.environ.get('CURL_STATUS','0')))
''')

    def script(self, name, source):
        path = self.bin / name
        path.write_text(source)
        path.chmod(0o755)

    def invoke(self, *args):
        result = subprocess.run(["/bin/sh", str(self.shell), *args], env=self.environment,
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(list(self.private.iterdir()), [], result.stderr)
        return result

    def test_generated_pins_syntax_and_help(self):
        text = self.shell.read_text()
        self.assertIn(TAG + "/install-linux-x64.py", text)
        self.assertIn(hashlib.sha256(self.body.read_bytes()).hexdigest(), text)
        self.assertIn("installer_bytes=" + str(self.body.stat().st_size), text)
        self.assertIsNone(re.search(r"@[A-Z][A-Z0-9_]*@", text))
        subprocess.run(["/bin/sh", "-n", str(self.shell)], check=True)
        result = self.invoke("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Linux x86_64 only", result.stdout)
        self.assertFalse(self.curl_log.exists())

    def test_saved_file_executes_twice_with_default_home_and_exact_quoted_arguments(self):
        prefix = self.root / "prefix with spaces ' quote"
        for arguments in [[], ["--prefix", str(prefix), "--allow-downgrade"]]:
            result = self.invoke(*arguments)
            self.assertEqual(result.returncode, 0, result.stderr)
        rows = json.loads(self.execution_log.read_bytes())
        self.assertEqual([row["argv"] for row in rows], [[], ["--prefix", str(prefix), "--allow-downgrade"]])
        self.assertTrue(all(row["saved"] and row["home"] == str(self.home) for row in rows))
        self.assertTrue(all(Path(row["path"]).is_relative_to(self.private) for row in rows))
        args = json.loads(self.curl_log.read_bytes())
        self.assertEqual(args[-1], f"https://github.com/nudoxorg/releases/releases/download/{TAG}/install-linux-x64.py")
        self.assertEqual(args[args.index("--proto") + 1], "=https")
        self.assertEqual(args[args.index("--proto-redir") + 1], "=https")
        self.assertEqual(args[args.index("--max-filesize") + 1], str(self.body.stat().st_size))
        self.assertEqual(args[args.index("--output") + 1], "-")

    def test_http_failure_and_wrong_sized_or_wrong_hash_body_never_execute(self):
        body = self.body.read_bytes()
        for label, downloaded, status in [("HTTP", body[:7], 22), ("partial", body[:-1], 0),
                                          ("wrong-hash", b"x" * len(body), 0), ("oversize", body + b"x", 0)]:
            with self.subTest(label=label):
                bad = self.root / (label + ".py")
                bad.write_bytes(downloaded)
                self.environment.update(CURL_BODY=str(bad), CURL_STATUS=str(status))
                result = self.invoke()
                self.assertEqual(result.returncode, status or 1, result.stderr)
                self.assertFalse(self.execution_log.exists())
                self.assertIn("installer was not executed", result.stderr)

    def test_installer_failure_status_is_preserved(self):
        self.environment["INSTALLER_STATUS"] = "23"
        result = self.invoke()
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(len(json.loads(self.execution_log.read_bytes())), 1)

    def test_unsupported_host_and_old_glibc_stop_before_download(self):
        self.script("uname", '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        result = self.invoke()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(self.curl_log.exists())
        self.script("uname", '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
        self.environment["FIXTURE_GLIBC"] = "glibc 2.38"
        result = self.invoke()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(self.curl_log.exists())
        self.assertIn("requires glibc 2.39", result.stderr)

    def test_measured_floor_and_closed_renderer_inputs(self):
        self.shell.write_bytes(package.checkpoint_shell(TAG, self.body.read_bytes(), "2.40"))
        result = self.invoke()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("requires glibc 2.40", result.stderr)
        self.assertFalse(self.curl_log.exists())
        for tag, body, floor in [(TAG + "'; touch bad", b"x", "2.39"), (TAG, b"", "2.39"),
                                 (TAG, b"x", "2.39'; exit 0"), (TAG, "not bytes", "2.39"), (TAG, b"x", "02.039")]:
            with self.subTest(tag=tag, floor=floor), self.assertRaises(ValueError):
                package.checkpoint_shell(tag, body, floor)

    def test_missing_and_unfilled_template_tokens_refuse(self):
        template = Path(package.__file__).with_name("install_linux_checkpoint.sh.in").read_text()
        for value in [template.replace("@INSTALLER_SHA256@", "missing"), template + "\n@UNFILLED@\n"]:
            with self.subTest(value=value[-30:]), patch.object(Path, "read_text", return_value=value), \
                 self.assertRaises(ValueError):
                package.checkpoint_shell(TAG, self.body.read_bytes(), "2.39")


if __name__ == "__main__":
    unittest.main()
