"""Python transport/package fixtures, never current native runtime evidence."""
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import urllib.request

ROOT = Path(__file__).parent
spec = importlib.util.spec_from_file_location("mirror", ROOT / "install_forgejo_preview.py")
boot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boot)


def channel():
    return json.loads((ROOT / "forgejo-preview-channel.json").read_bytes())


class Response(io.BytesIO):
    def __init__(self, body, url):
        super().__init__(body)
        self.url = url
        self.requests = []

    def geturl(self):
        return self.url

    def read(self, size=-1):
        self.requests.append(size)
        return super().read(size)


class Opener:
    def __init__(self, response):
        self.response = response

    def open(self, request, timeout):
        return self.response


def mac_fixture(root):
    entry = copy.deepcopy(channel()["platforms"]["macos-arm64"])
    source = entry["source"]
    native = json.dumps({"source": {"commit": source}, "kind": "Python fixture only"}).encode()
    files = {"bin/" + name: b"#!/bin/sh\nexit 0\n" for name in ("backend-cli", "backend-mcp", "backend-locald")}
    files.update({"native-build-manifest.json": native, "README.txt": b"Python fixture, not native evidence"})
    manifest = {"schema": "nudox.mac-cli-diagnostic-preview.v1", "source": source,
                "production_ready": False, "runtime_manifest_sha256": hashlib.sha256(native).hexdigest(),
                "files": [{"packaged": name, "packaged_sha256": hashlib.sha256(body).hexdigest()}
                          for name, body in files.items() if name.startswith("bin/")]}
    files["manifest.json"] = json.dumps(manifest).encode()
    archive = root / "fixture.tar.gz"
    with tarfile.open(archive, "w:gz") as bundle:
        for name, body in files.items():
            member = tarfile.TarInfo("nudox-macos-arm64/" + name)
            member.size = len(body)
            bundle.addfile(member, io.BytesIO(body))
    entry["archive"].update(sha256=hashlib.sha256(archive.read_bytes()).hexdigest(), bytes=archive.stat().st_size)
    entry["known_failures"] = ["Python transport/package fixture; no native runtime claim."]
    return entry, archive


class Tests(unittest.TestCase):
    def test_default_channel_and_unchanged_platform_library_pins(self):
        boot.validate_channel(channel())
        mac = (ROOT / "install_macos_preview.py").read_bytes()
        linux = (ROOT / "install_linux.py").read_text().replace('PINNED_RELEASE_TAG = ""',
                'PINNED_RELEASE_TAG = "checkpoint-20261008-c0016d4f4f-linux-x64"').replace('PINNED_MANIFEST_SHA256 = ""',
                'PINNED_MANIFEST_SHA256 = "384ab3802efd256752f39fd8e3d79ace7af9e3ccf72797c71577327cb2535b24"').encode()
        self.assertEqual((hashlib.sha256(mac).hexdigest(), len(mac)), boot.LIBRARIES["macos-arm64"][:2])
        self.assertEqual((hashlib.sha256(linux).hexdigest(), len(linux)), boot.LIBRARIES["linux-x64"][:2])
        script = (ROOT / "install_forgejo_preview.py").read_bytes()
        self.assertEqual(channel()["bootstrap"]["sha256"], hashlib.sha256(script).hexdigest())
        self.assertEqual(channel()["bootstrap"]["bytes"], len(script))

    def test_closed_initial_roles_reject_credentials_encoding_and_other_repositories(self):
        initial = boot.release_url(boot.BOOTSTRAP_TAG, "preview-channel.json")
        bad = [initial.replace("https:", "http:"), initial.replace("dev.nudox.org", "dev.nudox.org.evil.test"),
               initial.replace("dev.nudox.org", "user@dev.nudox.org"), initial.replace("dev.nudox.org", "dev.nudox.org:443"),
               initial.replace("philocalyst/Backend", "Nudox/Backend"), initial.replace("preview-channel", "other"),
               initial + "?x=1", initial + "#x", initial.replace("/releases/", "/%72eleases/"),
               boot.ORIGIN + "/git/attachments/00000000-0000-0000-0000-000000000000"]
        for url in bad:
            with self.subTest(url=url), self.assertRaises(boot.InstallError):
                boot.check_url(url, initial)

    def test_redirects_allow_only_same_origin_uuid_attachment_or_original_role(self):
        initial = boot.CHANNEL
        attachment = boot.ORIGIN + "/git/attachments/00000000-0000-0000-0000-000000000000"
        boot.check_url(attachment, initial, redirect=True)
        boot.check_url(initial, initial, redirect=True)
        request = urllib.request.Request(initial)
        handler = boot.HTTPSRedirect(initial)
        self.assertEqual(handler.redirect_request(request, None, 302, "Found", {}, attachment).full_url, attachment)
        for target in [attachment.replace("https:", "http:"), attachment + "?x=1", attachment + "/extra",
                       attachment.replace("dev.nudox.org", "example.org"), initial.replace("preview-channel", "unrelated")]:
            with self.subTest(target=target), self.assertRaises(boot.InstallError):
                handler.redirect_request(request, None, 302, "Found", {}, target)

    def test_channel_refuses_role_pin_source_date_and_status_mismatches(self):
        mutations = [lambda c: c.update(production_ready=True), lambda c: c.update(extra=True),
                     lambda c: c["platforms"]["macos-arm64"].update(known_failures=[]),
                     lambda c: c["platforms"]["macos-arm64"].update(source="b" * 40),
                     lambda c: c["platforms"]["macos-arm64"].update(tag="checkpoint-20261340-a659d5d181-macos-arm64"),
                     lambda c: c["platforms"]["linux-x64"]["installer"].update(sha256="b" * 64),
                     lambda c: c["platforms"]["linux-x64"]["archive"].update(bytes=True),
                     lambda c: c["platforms"]["linux-x64"]["manifest"].update(url=boot.CHANNEL)]
        for mutation in mutations:
            value = channel(); mutation(value)
            with self.assertRaises(boot.InstallError): boot.validate_channel(value)

    def test_supported_and_refused_platforms(self):
        for system, machine, key in [("Linux", "x86_64", "linux-x64"), ("Linux", "AMD64", "linux-x64"),
                                     ("Darwin", "aarch64", "macos-arm64")]:
            self.assertEqual(boot.platform_key(system, machine), key)
        for system, machine in [("Linux", "arm64"), ("Darwin", "x86_64"), ("Windows", "amd64")]:
            with self.assertRaises(boot.InstallError): boot.platform_key(system, machine)

    def test_streamed_private_file_matches_digest_and_bounded_read(self):
        body = b"x" * 150000
        response = Response(body, boot.CHANNEL)
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "verified"
            with patch.object(boot.urllib.request, "build_opener", return_value=Opener(response)):
                boot.download(boot.CHANNEL, destination, len(body), expected_size=len(body), expected_sha256=hashlib.sha256(body).hexdigest())
            self.assertEqual(destination.read_bytes(), body)
            self.assertEqual(destination.stat().st_mode & 0o777, 0o600)
            self.assertTrue(all(size == 65536 for size in response.requests))

    def test_size_digest_overflow_and_final_route_fail_before_import(self):
        for body, size, digest, final in [(b"bad", 3, "a" * 64, boot.CHANNEL),
                                          (b"short", 9, "a" * 64, boot.CHANNEL),
                                          (b"long", 2, "a" * 64, boot.CHANNEL),
                                          (b"bad", 3, "a" * 64, "https://example.org/file")]:
            with tempfile.TemporaryDirectory() as temporary:
                destination = Path(temporary) / "bad"
                with patch.object(boot.urllib.request, "build_opener", return_value=Opener(Response(body, final))):
                    with self.assertRaises(boot.InstallError):
                        boot.download(boot.CHANNEL, destination, 100, expected_size=size, expected_sha256=digest)
                self.assertFalse(destination.exists())

    def test_download_does_not_overwrite_existing_or_follow_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); owned = root / "owned"; owned.write_bytes(b"keep")
            link = root / "link"; link.symlink_to(owned)
            for path in [owned, link]:
                with self.assertRaises(FileExistsError): boot.download(boot.CHANNEL, path, 10)
            self.assertEqual(owned.read_bytes(), b"keep")

    def test_normal_mac_install_api_keeps_inventory_and_lease_guard(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); entry, archive = mac_fixture(root); prefix = root / "prefix"
            boot.install_verified(entry, "macos-arm64", ROOT / "install_macos_preview.py", archive, None, prefix, False)
            current = prefix / "lib/nudox/current"; active = current.resolve()
            marker = json.loads((active / ".installed-release.json").read_bytes())
            self.assertEqual(marker["sha256"], entry["archive"]["sha256"])
            self.assertEqual(marker["asset"], Path(entry["archive"]["url"]).name)
            boot.install_verified(entry, "macos-arm64", ROOT / "install_macos_preview.py", archive, None, prefix, False)
            self.assertEqual(current.resolve(), active)
            (active / "bin/backend-cli").write_bytes(b"tampered")
            with self.assertRaises(boot.InstallError):
                boot.install_verified(entry, "macos-arm64", ROOT / "install_macos_preview.py", archive, None, prefix, False)
            self.assertEqual(current.resolve(), active)

    def test_linux_original_pin_refuses_changed_mirror_manifest_before_install(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            body = (ROOT / "install_linux.py").read_text().replace('PINNED_RELEASE_TAG = ""',
                'PINNED_RELEASE_TAG = "checkpoint-20261008-c0016d4f4f-linux-x64"').replace('PINNED_MANIFEST_SHA256 = ""',
                'PINNED_MANIFEST_SHA256 = "384ab3802efd256752f39fd8e3d79ace7af9e3ccf72797c71577327cb2535b24"')
            library = root / "platform.py"; library.write_text(body)
            manifest = root / "manifest.json"; manifest.write_bytes(b"changed")
            entry = channel()["platforms"]["linux-x64"]
            with self.assertRaisesRegex(boot.InstallError, "SHA-256 mismatch"):
                boot.install_verified(entry, "linux-x64", library, root / "absent", manifest, root / "prefix", False)
            self.assertFalse((root / "prefix").exists())

    def test_original_linux_manifest_parses_without_rewriting_provenance(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            body = (ROOT / "install_linux.py").read_text().replace('PINNED_RELEASE_TAG = ""',
                'PINNED_RELEASE_TAG = "checkpoint-20261008-c0016d4f4f-linux-x64"').replace('PINNED_MANIFEST_SHA256 = ""',
                'PINNED_MANIFEST_SHA256 = "384ab3802efd256752f39fd8e3d79ace7af9e3ccf72797c71577327cb2535b24"')
            library = root / "platform.py"; library.write_text(body)
            module = boot.load_library(library, "linux-x64")
            raw = (ROOT / "fixtures/diagnostic-c001-release-manifest.json").read_bytes()
            value = channel()["platforms"]["linux-x64"]
            product, manifest = module.parse_pinned_manifest(value["tag"], raw)
            self.assertEqual(hashlib.sha256(raw).hexdigest(), value["manifest"]["sha256"])
            self.assertEqual(len(raw), value["manifest"]["bytes"])
            self.assertEqual(product["source_sha"], value["source"])
            self.assertEqual(manifest["sha256"], value["archive"]["sha256"])
            self.assertEqual(manifest["size_bytes"], value["archive"]["bytes"])
            self.assertTrue(all("dev.nudox.org" not in str(v) for v in manifest.values()))

    def test_library_changed_after_download_is_refused_before_import(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); library = root / "platform.py"
            library.write_bytes(b"raise RuntimeError('unverified import executed')\n")
            with self.assertRaisesRegex(boot.InstallError, "changed before import"):
                boot.load_library(library, "macos-arm64")

    def test_actual_stdin_file_dispatch_and_downgrade_flag_with_unchanged_library(self):
        # Only HTTPS I/O and platform detection are fixture substitutes. The child
        # runs unchanged bootstrap bytes and the unchanged normal Mac install().
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); entry, archive = mac_fixture(root)
            value = channel(); value["platforms"]["macos-arm64"] = entry
            metadata = root / "channel.json"; metadata.write_text(json.dumps(value))
            mapping = {boot.CHANNEL: str(metadata), value["bootstrap"]["url"]: str(ROOT / "install_forgejo_preview.py"),
                       entry["installer"]["url"]: str(ROOT / "install_macos_preview.py"), entry["archive"]["url"]: str(archive)}
            (root / "mapping.json").write_text(json.dumps(mapping))
            (root / "sitecustomize.py").write_text('''import io,json,os,pathlib,platform,urllib.request
platform.system=lambda: 'Darwin'
platform.machine=lambda: 'arm64'
mapping=json.loads(pathlib.Path(os.environ['FIXTURE_MAPPING']).read_text())
class Response(io.BytesIO):
 def __init__(self,url): super().__init__(pathlib.Path(mapping[url]).read_bytes()); self.url=url
 def geturl(self): return self.url
class Opener:
 def open(self,request,timeout):
  with open(os.environ['FIXTURE_REQUESTS'],'a') as log: log.write(request.full_url+'\\n')
  return Response(request.full_url)
urllib.request.build_opener=lambda *handlers: Opener()
''')
            environment = {**os.environ, "PYTHONPATH": str(root), "PYTHONDONTWRITEBYTECODE": "1",
                           "FIXTURE_MAPPING": str(root / "mapping.json"), "FIXTURE_REQUESTS": str(root / "requests")}
            private = root / "temporary"; private.mkdir(mode=0o700)
            environment["TMPDIR"] = str(private)
            source = (ROOT / "install_forgejo_preview.py").read_text()
            prefix = root / "prefix"
            newer = copy.deepcopy(entry)
            newer["tag"] = newer["tag"].replace("20261006", "20261010")
            boot.install_verified(newer, "macos-arm64", ROOT / "install_macos_preview.py", archive, None, prefix, False)
            original = (prefix / "lib/nudox/current").resolve()
            refusal = subprocess.run([sys.executable, str(ROOT / "install_forgejo_preview.py"), "--prefix", str(prefix)],
                                     text=True, capture_output=True, env=environment, timeout=20)
            self.assertEqual(refusal.returncode, 1, refusal.stderr)
            self.assertIn("older", refusal.stderr)
            self.assertEqual((prefix / "lib/nudox/current").resolve(), original)
            for mode in ["stdin", "file"]:
                command = [sys.executable, "-" if mode == "stdin" else str(ROOT / "install_forgejo_preview.py"),
                           "--prefix", str(prefix), "--allow-downgrade"]
                result = subprocess.run(command, input=source if mode == "stdin" else None,
                                        text=True, capture_output=True, env=environment, timeout=20)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("Diagnostic mirror", result.stdout)
                self.assertIn(str(prefix / "bin/backend-mcp"), result.stdout)
                self.assertTrue((prefix / "lib/nudox/current").is_symlink())
                self.assertEqual(list(private.iterdir()), [])
            requests = (root / "requests").read_text().splitlines()
            self.assertIn(value["bootstrap"]["url"], requests)
            self.assertIn(entry["installer"]["url"], requests)
            self.assertIn(entry["archive"]["url"], requests)
            self.assertEqual(json.loads((prefix / "lib/nudox/current/.installed-release.json").read_text())["source_sha"], entry["source"])


if __name__ == "__main__":
    unittest.main()
