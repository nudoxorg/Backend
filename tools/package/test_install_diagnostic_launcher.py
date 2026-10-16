"""Shell delivery controls with Python transport/package fixtures, not native QA."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

import test_install_forgejo_preview as mirror_tests

ROOT = Path(__file__).parent
LAUNCHER = ROOT / "install_diagnostic_preview.sh"
BOOTSTRAP = mirror_tests.PUBLISHED_BOOTSTRAP_FIXTURE
GLOBAL_LAUNCHER = ROOT / "install_preview.sh"
GLOBAL_BOOTSTRAP = ROOT / "fixtures/install-preview-bootstrap-c533dfe34b7d137a-pinned.py"
GLOBAL_BOOTSTRAP_URL = "https://github.com/nudoxorg/Backend/releases/download/nudox-preview-installer/install-bootstrap-c533dfe34b7d137a.py"
GLOBAL_BOOTSTRAP_SHA256 = "c533dfe34b7d137a08f1569e217cdfd44fc733c8584f0b619c57a23b27cc0209"
GLOBAL_BOOTSTRAP_BYTES = 6033


class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="launcher fixture ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.private = self.root / "temporary"
        self.private.mkdir(mode=0o700)
        self.environment = {**os.environ, "PATH": str(self.bin) + os.pathsep + os.environ.get("PATH", ""),
                            "TMPDIR": str(self.private), "PYTHONDONTWRITEBYTECODE": "1",
                            "NUDOX_INSTALL_PYTHON": sys.executable,
                            "CURL_BODY": str(BOOTSTRAP), "CURL_LOG": str(self.root / "curl.json")}
        self.script("uname", "#!/bin/sh\ncase \"$1\" in -s) echo Darwin;; -m) echo arm64;; esac\n")
        self.script("curl", "#!" + sys.executable + "\n" + '''import json,os,pathlib,sys
args=sys.argv[1:]
pathlib.Path(os.environ['CURL_LOG']).write_text(json.dumps(args))
body=pathlib.Path(os.environ['CURL_BODY']).read_bytes()
if os.environ.get('CURL_PARTIAL'): body=body[:71]
sys.stdout.buffer.write(body)
sys.stdout.buffer.flush()
sys.exit(int(os.environ.get('CURL_STATUS','0')))
''')

    def script(self, name, source):
        path = self.bin / name
        path.write_text(source)
        path.chmod(0o755)
        return path

    def run_launcher(self, *args, stdin=False, timeout=25):
        result = subprocess.run(["/bin/sh", "-s", "--", *args] if stdin else ["/bin/sh", str(LAUNCHER), *args],
                                input=LAUNCHER.read_text() if stdin else None, env=self.environment,
                                capture_output=True, text=True, timeout=timeout)
        # Apple's /usr/bin/python3 shim may create an unrelated xcrun_db here.
        self.assertEqual([p for p in self.private.iterdir() if p.name != "xcrun_db"], [], result.stderr)
        return result

    def run_global_launcher(self, *args, timeout=25):
        result = subprocess.run(["/bin/sh", str(GLOBAL_LAUNCHER), *args], env=self.environment,
                                capture_output=True, text=True, timeout=timeout)
        self.assertEqual([p for p in self.private.iterdir() if p.name != "xcrun_db"], [], result.stderr)
        return result

    def prepare_global_success_transport(self):
        self.script("uname", "#!/bin/sh\ncase \"$1\" in -s) echo Linux;; -m) echo x86_64;; esac\n")
        self.environment.update(CURL_BODY=str(GLOBAL_BOOTSTRAP), CURL_STATUS="0")
        channel = json.loads((ROOT / "preview-channel.json").read_text())
        installer = self.root / "platform installer.py"
        installer_body = b"import json,os,sys\nopen(os.environ['CHILD_ARGS'],'w').write(json.dumps(sys.argv[1:]))\n"
        installer.write_bytes(installer_body)
        installer_entry = channel["platforms"]["linux-x64"]["installer"]
        installer_entry["sha256"] = hashlib.sha256(installer_body).hexdigest()
        installer_entry["bytes"] = len(installer_body)
        channel_path = self.root / "github channel.json"
        channel_path.write_text(json.dumps(channel))
        mapping = {"https://github.com/nudoxorg/Backend/releases/download/nudox-preview-installer/preview-channel.json": str(channel_path),
                   channel["platforms"]["linux-x64"]["installer"]["url"]: str(installer)}
        mapping_path = self.root / "url mapping.json"
        mapping_path.write_text(json.dumps(mapping))
        (self.root / "sitecustomize.py").write_text('''import io,json,os,pathlib,platform,urllib.request
platform.system=lambda: 'Linux'
platform.machine=lambda: 'x86_64'
mapping=json.loads(pathlib.Path(os.environ['FIXTURE_MAPPING']).read_text())
class Response(io.BytesIO):
 def __init__(self,url): super().__init__(pathlib.Path(mapping[url]).read_bytes()); self.url=url
 def geturl(self): return self.url
class Opener:
 def open(self,request,timeout):
  url=request.full_url
  with open(os.environ['FIXTURE_REQUESTS'],'a') as log: log.write(url+'\\n')
  return Response(url)
urllib.request.build_opener=lambda *handlers: Opener()
''')
        self.environment.update(PYTHONPATH=str(self.root), FIXTURE_MAPPING=str(mapping_path),
                                FIXTURE_REQUESTS=str(self.root / "requests.log"),
                                CHILD_ARGS=str(self.root / "child args.json"))
        return channel, installer

    def test_github_bootstrap_shell_is_exact_reviewed_body_with_only_pin_and_copy_changes(self):
        source = LAUNCHER.read_text()
        expected = source
        replacements = [
            ("# Download and execute the immutable R2 Python bootstrap as a saved file.",
             "# Download and execute the immutable GitHub diagnostic-preview bootstrap as a saved file."),
            ("# This delivery repair still installs the October 6/8 diagnostic binaries.",
             "# The current GitHub channel remains explicitly diagnostic and records known limits."),
            ("https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/install.py",
             GLOBAL_BOOTSTRAP_URL),
            ("36f0ea62e3b49cadebcef0f89e735639530c0ba1686e62d4dcbfea96294174f5", GLOBAL_BOOTSTRAP_SHA256),
            ("installer_bytes=13426", "installer_bytes=" + str(GLOBAL_BOOTSTRAP_BYTES)),
            ("Supports macOS arm64 and Linux x86_64; installs the historical R2 diagnostic preview.",
             "Supports macOS arm64 and Linux x86_64; installs the current GitHub diagnostic preview."),
            ("Downloading the pinned R2 diagnostic installer (historical October 6/8 binaries).",
             "Downloading the pinned GitHub diagnostic-preview bootstrap."),
        ]
        for old, new in replacements:
            self.assertEqual(expected.count(old), 1)
            expected = expected.replace(old, new)
        actual = GLOBAL_LAUNCHER.read_text()
        self.assertEqual(actual, expected)
        body = GLOBAL_BOOTSTRAP.read_bytes()
        self.assertEqual(len(body), GLOBAL_BOOTSTRAP_BYTES)
        self.assertEqual(hashlib.sha256(body).hexdigest(), GLOBAL_BOOTSTRAP_SHA256)
        channel = json.loads((ROOT / "preview-channel.json").read_text())
        self.assertEqual(channel["bootstrap"], {"url": GLOBAL_BOOTSTRAP_URL,
                                                "sha256": GLOBAL_BOOTSTRAP_SHA256,
                                                "bytes": GLOBAL_BOOTSTRAP_BYTES})

    def test_github_bootstrap_shell_truncation_and_curl_errors_never_execute(self):
        self.environment.update(CURL_BODY=str(GLOBAL_BOOTSTRAP), CURL_STATUS="0", CURL_PARTIAL="1")
        prefix = self.root / "partial download prefix"
        result = self.run_global_launcher("--prefix", str(prefix))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("size/SHA-256 mismatch", result.stderr)
        self.assertFalse(prefix.exists())

        self.environment.update(CURL_STATUS="22", CURL_PARTIAL="1")
        failed = self.run_global_launcher("--prefix", str(prefix))
        self.assertEqual(failed.returncode, 22, failed.stderr)
        self.assertIn("HTTPS download failed", failed.stderr)
        self.assertFalse(prefix.exists())

    def test_github_bootstrap_shell_forwards_quoted_prefix_to_saved_bootstrap(self):
        channel, _installer = self.prepare_global_success_transport()
        prefix = self.root / "prefix with spaces ' and quote"
        result = self.run_global_launcher("--prefix", str(prefix), "--allow-downgrade")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.root / "child args.json").read_text()),
                         ["--prefix", str(prefix), "--allow-downgrade"])
        self.assertIn("Diagnostic preview; whole-project acceptance is not established.", result.stdout)
        self.assertIn(channel["platforms"]["linux-x64"]["known_failures"][0], result.stdout)
        requests = (self.root / "requests.log").read_text().splitlines()
        self.assertEqual(requests, [
            "https://github.com/nudoxorg/Backend/releases/download/nudox-preview-installer/preview-channel.json",
            channel["platforms"]["linux-x64"]["installer"]["url"],
        ])
        curl_args = json.loads((self.root / "curl.json").read_text())
        self.assertIn(GLOBAL_BOOTSTRAP_URL, curl_args)
        self.assertEqual(curl_args[curl_args.index("--max-filesize") + 1], str(GLOBAL_BOOTSTRAP_BYTES))

    def test_github_bootstrap_shell_rejects_rustpython_before_curl(self):
        self.environment["NUDOX_INSTALL_PYTHON"] = sys.executable
        self.environment["PYTHONPATH"] = str(self.root)
        (self.root / "sitecustomize.py").write_text(
            'import platform\nplatform.python_implementation=lambda: "RustPython"\n')
        self.environment["CURL_BODY"] = str(GLOBAL_BOOTSTRAP)
        result = self.run_global_launcher()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("NUDOX_INSTALL_PYTHON", result.stderr)
        self.assertFalse((self.root / "curl.json").exists())

    def fixture_transport(self):
        entry, archive = mirror_tests.mac_fixture(self.root)
        value = mirror_tests.channel()
        value["platforms"]["macos-arm64"] = entry
        metadata = self.root / "channel.json"
        metadata.write_text(json.dumps(value))
        mapping = {mirror_tests.boot.CHANNEL: str(metadata), value["bootstrap"]["url"]: str(BOOTSTRAP),
                   entry["installer"]["url"]: str(mirror_tests.published_mac_library(self.root)),
                   entry["archive"]["url"]: str(archive)}
        (self.root / "mapping.json").write_text(json.dumps(mapping))
        (self.root / "sitecustomize.py").write_text('''import io,json,os,pathlib,platform,urllib.request
platform.system=lambda: 'Darwin'
platform.machine=lambda: 'arm64'
mapping=json.loads(pathlib.Path(os.environ['FIXTURE_MAPPING']).read_text())
class Response(io.BytesIO):
 def __init__(self,url): super().__init__(pathlib.Path(mapping[url]).read_bytes()); self.url=url
 def geturl(self): return self.url
class Opener:
 def open(self,request,timeout): return Response(request.full_url)
urllib.request.build_opener=lambda *handlers: Opener()
''')
        self.environment.update(PYTHONPATH=str(self.root), FIXTURE_MAPPING=str(self.root / "mapping.json"))
        return entry, archive

    def test_launcher_literal_pin_is_exact_existing_published_r2_source(self):
        source = LAUNCHER.read_text()
        body = BOOTSTRAP.read_bytes()
        self.assertIn("installer_sha256='" + hashlib.sha256(body).hexdigest() + "'", source)
        self.assertIn("installer_bytes=" + str(len(body)), source)
        self.assertIn(mirror_tests.boot.release_url(mirror_tests.boot.BOOTSTRAP_TAG, "install.py"), source)

    def test_http_and_partial_download_failure_preserve_status_and_never_execute(self):
        for status in (22, 23, 28, 35):
            with self.subTest(status=status):
                self.environment.update(CURL_STATUS=str(status), CURL_PARTIAL="1")
                prefix = self.root / "must not install"
                result = self.run_launcher("--prefix", str(prefix), stdin=True)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertIn("HTTPS download failed", result.stderr)
                self.assertFalse(prefix.exists())

    def test_successful_wrong_or_empty_response_is_never_executed(self):
        for body in (b"", b"<html>unavailable</html>", b"raise RuntimeError('UNVERIFIED EXECUTION')\n", BOOTSTRAP.read_bytes() + b"x"):
            with self.subTest(body=body):
                bad = self.root / "wrong.py"
                bad.write_bytes(body)
                self.environment["CURL_BODY"] = str(bad)
                result = self.run_launcher("--prefix", str(self.root / "prefix"))
                self.assertEqual(result.returncode, 1, result.stderr)
                expected_error = "exceeded its pinned byte count" if len(body) > len(BOOTSTRAP.read_bytes()) else "size/SHA-256 mismatch"
                self.assertIn(expected_error, result.stderr)
                self.assertNotIn("UNVERIFIED EXECUTION", result.stderr)
                self.assertFalse((self.root / "prefix").exists())

    def test_file_and_shell_stdin_fresh_install_reinstall_and_aliases_use_saved_python(self):
        self.fixture_transport()
        prefix = self.root / "prefix with spaces ' quote"
        for stdin in (False, True):
            result = self.run_launcher("--prefix", str(prefix), stdin=stdin)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(str(prefix / "bin/backend-mcp"), result.stdout)
            self.assertIn('export PATH=' + shlex.quote(str(prefix / "bin")) + ':"$PATH"', result.stdout)
            current = prefix / "lib/nudox/current"
            self.assertTrue(current.is_symlink())
            for alias in ("nudox", "nudox-mcp", "nudox-locald", "backend-cli", "backend-mcp", "backend-locald"):
                self.assertTrue((prefix / "bin" / alias).is_symlink())
                self.assertTrue((prefix / "bin" / alias).resolve().is_file())
        args = json.loads((self.root / "curl.json").read_text())
        self.assertEqual(args[0], "-q")
        for flag in ("--fail", "--proto", "--proto-redir", "--tlsv1.2", "--max-filesize",
                     "--connect-timeout", "--max-time"):
            self.assertIn(flag, args)
        self.assertEqual(args[args.index("--proto")+1], "=https")
        self.assertEqual(args[args.index("--proto-redir")+1], "=https")
        self.assertEqual(args[args.index("--max-filesize")+1], str(BOOTSTRAP.stat().st_size))
        self.assertEqual(args[args.index("--output")+1], "-")
        self.assertEqual(args[args.index("--connect-timeout")+1], "15")
        self.assertEqual(args[args.index("--max-time")+1], "180")

    def test_one_line_launcher_reaches_candidate_bootstrap_and_skips_repeat_archive(self):
        entry, archive = mirror_tests.mac_fixture(self.root)
        entry, value, candidate_environment = mirror_tests.candidate_mac_transport(self.root, entry, archive)
        self.environment.update(candidate_environment)
        prefix = self.root / "candidate preview"

        first = self.run_launcher("--prefix", str(prefix))
        self.assertEqual(first.returncode, 0, first.stderr)
        requests = Path(self.environment["FIXTURE_REQUESTS"])
        first_requests = requests.read_text().splitlines()
        self.assertIn(value["bootstrap"]["url"], first_requests)
        self.assertIn(entry["installer"]["url"], first_requests)
        self.assertIn(entry["archive"]["url"], first_requests)
        requests.unlink()

        repeated = self.run_launcher("--prefix", str(prefix))
        self.assertEqual(repeated.returncode, 0, repeated.stderr)
        self.assertIn("skipped its archive download", repeated.stderr)
        repeat_requests = requests.read_text().splitlines()
        self.assertIn(value["bootstrap"]["url"], repeat_requests)
        self.assertIn(entry["installer"]["url"], repeat_requests)
        self.assertNotIn(entry["archive"]["url"], repeat_requests)

    def test_update_refusal_preserves_pointer_and_explicit_flag_is_forwarded(self):
        entry, archive = self.fixture_transport()
        prefix = self.root / "existing"
        newer = {**entry, "tag": entry["tag"].replace("20261006", "20261010")}
        mirror_tests.run_published_install(newer, "macos-arm64", mirror_tests.published_mac_library(self.root), archive, None, prefix, False)
        current = prefix / "lib/nudox/current"
        prior = current.resolve()
        refused = self.run_launcher("--prefix", str(prefix))
        self.assertEqual(refused.returncode, 1, refused.stderr)
        self.assertIn("older", refused.stderr)
        self.assertEqual(current.resolve(), prior)
        accepted = self.run_launcher("--prefix", str(prefix), "--allow-downgrade", stdin=True)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertNotEqual(current.resolve(), prior)
        self.assertTrue(prior.is_dir())

    def test_unsupported_host_and_missing_curl_fail_before_download(self):
        self.script("uname", "#!/bin/sh\necho unsupported\n")
        result = self.run_launcher()
        self.assertEqual(result.returncode, 1)
        self.assertIn("unsupported host", result.stderr)
        self.assertFalse((self.root / "curl.json").exists())
        self.script("uname", "#!/bin/sh\ncase \"$1\" in -s) echo Linux;; -m) echo x86_64;; esac\n")
        (self.bin / "curl").unlink()
        self.environment["PATH"] = str(self.bin)
        result = self.run_launcher()
        self.assertEqual(result.returncode, 127)
        self.assertIn("curl is required", result.stderr)

    def test_invalid_explicit_interpreter_does_not_silently_fallback(self):
        self.environment["NUDOX_INSTALL_PYTHON"] = str(self.root / "missing python")
        result = self.run_launcher()
        self.assertEqual(result.returncode, 1)
        self.assertIn("NUDOX_INSTALL_PYTHON", result.stderr)
        self.assertFalse((self.root / "curl.json").exists())

    def test_optimized_python_cannot_bypass_implementation_version_or_api_probe(self):
        for replacement in ('platform.python_implementation=lambda: "RustPython"',
                            'sys.version_info=(3, 7)', 'del os.O_NOFOLLOW'):
            with self.subTest(replacement=replacement):
                (self.root / "sitecustomize.py").write_text('import os,platform,sys\n' + replacement + '\n')
                self.environment.update(PYTHONPATH=str(self.root), PYTHONOPTIMIZE="1")
                result = self.run_launcher()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("NUDOX_INSTALL_PYTHON", result.stderr)
                self.assertFalse((self.root / "curl.json").exists())

    def test_python_argument_failure_preserves_nonzero_status_and_cleanup(self):
        result = self.run_launcher("--unknown-option")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("unrecognized arguments", result.stderr)

    def test_auto_selection_prefers_host_cpython_over_broken_path_shim(self):
        self.fixture_transport()
        self.environment.pop("NUDOX_INSTALL_PYTHON")
        marker = self.root / "shim executed"
        self.script("python3", "#!/bin/sh\ntouch " + shlex.quote(str(marker)) + "\nexit 101\n")
        result = self.run_launcher("--prefix", str(self.root / "prefix"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()
