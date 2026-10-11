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
PUBLISHED_MAC_LIBRARY_FIXTURE = ROOT / "fixtures/install-macos-arm64-r2-pinned.py"
PUBLISHED_BOOTSTRAP_FIXTURE = ROOT / "fixtures/install-forgejo-bootstrap-r2-pinned.py"
spec = importlib.util.spec_from_file_location("mirror", PUBLISHED_BOOTSTRAP_FIXTURE)
boot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boot)
candidate_spec = importlib.util.spec_from_file_location("candidate_mirror", ROOT / "install_forgejo_preview.py")
candidate_boot = importlib.util.module_from_spec(candidate_spec)
candidate_spec.loader.exec_module(candidate_boot)


def channel():
    return json.loads((ROOT / "forgejo-preview-channel.json").read_bytes())


def published_mac_library_bytes():
    """Return the exact R2 library bytes pinned by the unchanged channel."""
    body = PUBLISHED_MAC_LIBRARY_FIXTURE.read_bytes()
    expected = boot.LIBRARIES["macos-arm64"][:2]
    if (hashlib.sha256(body).hexdigest(), len(body)) != expected:
        raise AssertionError("historical Mac source does not match the immutable R2 pin")
    return body


def published_bootstrap_bytes():
    """Return the exact R2 bootstrap bytes pinned by the unchanged channel."""
    body = PUBLISHED_BOOTSTRAP_FIXTURE.read_bytes()
    expected = channel()["bootstrap"]
    if (hashlib.sha256(body).hexdigest(), len(body)) != (expected["sha256"], expected["bytes"]):
        raise AssertionError("historical bootstrap source does not match the immutable R2 pin")
    return body


def published_mac_library(root):
    """Materialize the exact R2 library bytes pinned by the unchanged channel."""
    path = Path(root) / "published-r2-install-macos-preview.py"
    path.write_bytes(published_mac_library_bytes())
    return path


def candidate_mac_transport(root, entry, archive):
    """Make a next-bootstrap channel that pins the current candidate sources."""
    bootstrap_body = (ROOT / "install_forgejo_preview.py").read_bytes()
    bootstrap_sha = hashlib.sha256(bootstrap_body).hexdigest()
    installer_body = (ROOT / "install_macos_preview.py").read_bytes()
    installer_sha = hashlib.sha256(installer_body).hexdigest()
    if (installer_sha, len(installer_body)) != candidate_boot.LIBRARIES["macos-arm64"][:2]:
        raise AssertionError("candidate Mac library differs from the candidate bootstrap pin")
    entry = copy.deepcopy(entry)
    entry["installer"] = {
        "url": candidate_boot.release_url(entry["tag"], "install-macos-arm64-" + installer_sha[:16] + ".py"),
        "sha256": installer_sha,
        "bytes": len(installer_body),
    }
    value = channel()
    value["bootstrap"] = {
        "url": candidate_boot.release_url(candidate_boot.BOOTSTRAP_TAG,
                                            "install-forgejo-bootstrap-" + bootstrap_sha[:16] + ".py"),
        "sha256": bootstrap_sha,
        "bytes": len(bootstrap_body),
    }
    value["platforms"]["macos-arm64"] = entry
    candidate_boot.validate_channel(value)
    candidate_boot.admit_libraries(value)
    metadata = Path(root) / "candidate-channel.json"
    metadata.write_text(json.dumps(value))
    bootstrap_path = Path(root) / "candidate-bootstrap.py"
    bootstrap_path.write_bytes(bootstrap_body)
    installer_path = Path(root) / "candidate-mac-library.py"
    installer_path.write_bytes(installer_body)
    mapping = {
        candidate_boot.CHANNEL: str(metadata),
        value["bootstrap"]["url"]: str(bootstrap_path),
        entry["installer"]["url"]: str(installer_path),
        entry["archive"]["url"]: str(archive),
    }
    mapping_path = Path(root) / "candidate-mapping.json"
    mapping_path.write_text(json.dumps(mapping))
    (Path(root) / "sitecustomize.py").write_text('''import io,json,os,pathlib,platform,urllib.request
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
                   "FIXTURE_MAPPING": str(mapping_path), "FIXTURE_REQUESTS": str(Path(root) / "candidate-requests")}
    return entry, value, environment


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
        boot.admit_libraries(channel())
        mac = published_mac_library_bytes()
        candidate_mac = (ROOT / "install_macos_preview.py").read_bytes()
        linux = (ROOT / "install_linux.py").read_text().replace('PINNED_RELEASE_TAG = ""',
                'PINNED_RELEASE_TAG = "checkpoint-20261008-c0016d4f4f-linux-x64"').replace('PINNED_MANIFEST_SHA256 = ""',
                'PINNED_MANIFEST_SHA256 = "384ab3802efd256752f39fd8e3d79ace7af9e3ccf72797c71577327cb2535b24"').encode()
        self.assertEqual((hashlib.sha256(mac).hexdigest(), len(mac)), boot.LIBRARIES["macos-arm64"][:2])
        self.assertNotEqual((hashlib.sha256(candidate_mac).hexdigest(), len(candidate_mac)),
                            boot.LIBRARIES["macos-arm64"][:2])
        self.assertEqual((hashlib.sha256(linux).hexdigest(), len(linux)), boot.LIBRARIES["linux-x64"][:2])
        script = published_bootstrap_bytes()
        candidate_script = (ROOT / "install_forgejo_preview.py").read_bytes()
        self.assertEqual(channel()["bootstrap"]["sha256"], hashlib.sha256(script).hexdigest())
        self.assertEqual(channel()["bootstrap"]["bytes"], len(script))
        self.assertNotEqual((hashlib.sha256(candidate_script).hexdigest(), len(candidate_script)),
                            (channel()["bootstrap"]["sha256"], channel()["bootstrap"]["bytes"]))

    def test_candidate_bootstrap_repeat_reuses_verified_mac_install_before_archive_fetch(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(); entry, archive = mac_fixture(root)
            entry, value, environment = candidate_mac_transport(root, entry, archive)
            prefix = root / "prefix"
            command = [sys.executable, str(PUBLISHED_BOOTSTRAP_FIXTURE), "--prefix", str(prefix)]
            first = subprocess.run(command, text=True, capture_output=True, env=environment, timeout=25)
            self.assertEqual(first.returncode, 0, first.stderr)
            requests = Path(environment["FIXTURE_REQUESTS"])
            first_requests = requests.read_text().splitlines()
            self.assertIn(value["bootstrap"]["url"], first_requests)
            self.assertIn(entry["installer"]["url"], first_requests)
            self.assertIn(entry["archive"]["url"], first_requests)
            requests.unlink()

            repeated = subprocess.run(command, text=True, capture_output=True, env=environment, timeout=25)
            self.assertEqual(repeated.returncode, 0, repeated.stderr)
            self.assertIn("skipped its archive download", repeated.stderr)
            repeat_requests = requests.read_text().splitlines()
            self.assertIn(value["bootstrap"]["url"], repeat_requests)
            self.assertIn(entry["installer"]["url"], repeat_requests)
            self.assertNotIn(entry["archive"]["url"], repeat_requests)
            self.assertTrue((prefix / "lib/nudox/current").is_symlink())
            self.assertEqual(value["bootstrap"]["sha256"], hashlib.sha256(
                (ROOT / "install_forgejo_preview.py").read_bytes()).hexdigest())
            self.assertIn("Downloading", repeated.stderr)

    def test_candidate_bootstrap_new_release_downloads_archive_and_corruption_fails_before_fetch(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(); entry, archive = mac_fixture(root)
            entry, value, environment = candidate_mac_transport(root, entry, archive)
            prefix = root / "prefix"
            command = [sys.executable, str(PUBLISHED_BOOTSTRAP_FIXTURE), "--prefix", str(prefix)]
            first = subprocess.run(command, text=True, capture_output=True, env=environment, timeout=25)
            self.assertEqual(first.returncode, 0, first.stderr)
            requests = Path(environment["FIXTURE_REQUESTS"])
            requests.unlink()

            newer = copy.deepcopy(entry)
            newer["tag"] = newer["tag"].replace("20261006", "20261010")
            newer["installer"]["url"] = candidate_boot.release_url(
                newer["tag"], Path(newer["installer"]["url"]).name)
            newer["archive"]["url"] = candidate_boot.release_url(
                newer["tag"], Path(newer["archive"]["url"]).name)
            value["platforms"]["macos-arm64"] = newer
            candidate_boot.validate_channel(value)
            (root / "candidate-channel.json").write_text(json.dumps(value))
            mapping = json.loads((root / "candidate-mapping.json").read_text())
            mapping[newer["installer"]["url"]] = mapping[entry["installer"]["url"]]
            mapping[newer["archive"]["url"]] = mapping[entry["archive"]["url"]]
            (root / "candidate-mapping.json").write_text(json.dumps(mapping))

            updated = subprocess.run(command, text=True, capture_output=True, env=environment, timeout=25)
            self.assertEqual(updated.returncode, 0, updated.stderr)
            self.assertIn(newer["archive"]["url"], requests.read_text().splitlines())
            self.assertEqual((prefix / "lib/nudox/current").resolve().name, newer["tag"])
            requests.unlink()

            installed = (prefix / "lib/nudox/current").resolve()
            (installed / "bin/backend-cli").write_bytes(b"corrupted after a successful install")
            refused = subprocess.run(command, text=True, capture_output=True, env=environment, timeout=25)
            self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
            self.assertIn("integrity/startup recheck", refused.stderr)
            self.assertNotIn(newer["archive"]["url"], requests.read_text().splitlines())
            requests.unlink()

            installed = (prefix / "lib/nudox/current").resolve()
            (installed / "bin/backend-cli").write_bytes(b"#!/bin/sh\nexit 0\n")
            value["platforms"]["macos-arm64"] = entry
            candidate_boot.validate_channel(value)
            (root / "candidate-channel.json").write_text(json.dumps(value))
            downgrade = subprocess.run(command, text=True, capture_output=True, env=environment, timeout=25)
            self.assertEqual(downgrade.returncode, 1, downgrade.stdout + downgrade.stderr)
            self.assertIn("older", downgrade.stderr)
            self.assertNotIn(entry["archive"]["url"], requests.read_text().splitlines())

    def test_candidate_bootstrap_transport_reports_progress_with_bounded_timeout(self):
        class StreamingResponse(io.BytesIO):
            def geturl(self):
                return candidate_boot.CHANNEL

        class StreamingOpener:
            def __init__(self, body):
                self.response = StreamingResponse(body)
                self.timeouts = []

            def open(self, request, timeout):
                self.timeouts.append(timeout)
                return self.response

        body = b"x" * (candidate_boot.DOWNLOAD_PROGRESS_BYTES + 7)
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "archive.tar.gz"
            opener = StreamingOpener(body)
            with patch.object(candidate_boot.urllib.request, "build_opener", return_value=opener), \
                    patch.object(candidate_boot.sys, "stderr", new_callable=io.StringIO) as stderr:
                candidate_boot.download(candidate_boot.CHANNEL, destination, len(body),
                                         expected_size=len(body), expected_sha256=hashlib.sha256(body).hexdigest())
            output = stderr.getvalue()
            self.assertIn("0%", output)
            self.assertIn(f"{candidate_boot.DOWNLOAD_PROGRESS_BYTES:,}/", output)
            self.assertIn("100%", output)
            self.assertIn("checking declared size and digest", output)
            self.assertEqual(opener.timeouts, [candidate_boot.DOWNLOAD_TIMEOUT_SECONDS])
            self.assertEqual(destination.read_bytes(), body)

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
                     lambda c: c["platforms"]["linux-x64"]["installer"].update(
                         url=boot.release_url("unapproved-tag", "install-linux-x64-" + c["platforms"]["linux-x64"]["installer"]["sha256"][:16] + ".py")),
                     lambda c: c["platforms"]["linux-x64"]["archive"].update(bytes=True),
                     lambda c: c["platforms"]["linux-x64"]["manifest"].update(url=boot.CHANNEL)]
        for mutation in mutations:
            value = channel(); mutation(value)
            with self.assertRaises(boot.InstallError): boot.validate_channel(value)

    def test_r2_pins_repaired_linux_library_without_changing_prior_release_assets(self):
        value = channel()
        boot.validate_channel(value)
        boot.admit_libraries(value)
        self.assertEqual(boot.BOOTSTRAP_TAG, "nudox-diagnostic-installer-20261009-r2")
        linux = value["platforms"]["linux-x64"]
        mac = value["platforms"]["macos-arm64"]
        self.assertEqual(linux["installer"]["url"], boot.release_url(
            boot.BOOTSTRAP_TAG, "install-linux-x64-" + boot.LIBRARIES["linux-x64"][0][:16] + ".py"))
        self.assertEqual((linux["installer"]["sha256"], linux["installer"]["bytes"]), boot.LIBRARIES["linux-x64"][:2])
        self.assertEqual(boot.LIBRARIES["macos-arm64"],
                         ("82f71f98a05a2b13e05afd7185e35c5f8d252b9973ab4ce6d32ab22688794f25", 22094, "install-macos-arm64"))
        self.assertEqual(mac["installer"], {
            "url": boot.release_url(mac["tag"], "install-macos-arm64-82f71f98a05a2b13.py"),
            "sha256": "82f71f98a05a2b13e05afd7185e35c5f8d252b9973ab4ce6d32ab22688794f25",
            "bytes": 22094})
        self.assertEqual(mac["archive"], {
            "url": boot.release_url(mac["tag"], "nudox-macos-arm64-a659d5d181.tar.gz"),
            "sha256": "f88b841030699ae277afe697b6c92e98dfbb09a815f40388e3d2fbee0661e110",
            "bytes": 70309994})
        self.assertEqual(linux["archive"], {
            "url": boot.release_url(linux["tag"], "nudox-linux-x86_64-c0016d4f4f.tar.gz"),
            "sha256": "b394e10203d02898b20140f0f9f9869aa8412e6e288fb8d8e25ac01469df7a05",
            "bytes": 133556631})
        self.assertEqual(linux["manifest"], {
            "url": boot.release_url(linux["tag"], "release-manifest-linux-x64.json"),
            "sha256": "384ab3802efd256752f39fd8e3d79ace7af9e3ccf72797c71577327cb2535b24",
            "bytes": 698})

    def test_supported_and_refused_platforms(self):
        for system, machine, key in [("Linux", "x86_64", "linux-x64"), ("Linux", "AMD64", "linux-x64"),
                                     ("Darwin", "aarch64", "macos-arm64")]:
            self.assertEqual(boot.platform_key(system, machine), key)
        for system, machine in [("Linux", "arm64"), ("Darwin", "x86_64"), ("Windows", "amd64")]:
            with self.assertRaises(boot.InstallError): boot.platform_key(system, machine)

    def test_new_library_envelope_requires_new_bootstrap_before_library_admission(self):
        value = channel(); entry = value["platforms"]["macos-arm64"]
        entry["installer"].update(sha256="b" * 64, bytes=23000,
                url=boot.release_url(entry["tag"], "install-macos-arm64-" + "b" * 16 + ".py"))
        boot.validate_channel(value)
        with self.assertRaisesRegex(boot.InstallError, "reviewed bytes"):
            boot.admit_libraries(value)

    def test_unreviewed_r2_linux_library_digest_is_rejected_by_compiled_pin(self):
        value = channel()
        installer = value["platforms"]["linux-x64"]["installer"]
        installer.update(sha256="b" * 64, bytes=49955,
                         url=boot.release_url(boot.BOOTSTRAP_TAG, "install-linux-x64-" + "b" * 16 + ".py"))
        boot.validate_channel(value)
        with self.assertRaisesRegex(boot.InstallError, "reviewed bytes"):
            boot.admit_libraries(value)

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
            library = published_mac_library(root)
            boot.install_verified(entry, "macos-arm64", library, archive, None, prefix, False)
            current = prefix / "lib/nudox/current"; active = current.resolve()
            marker = json.loads((active / ".installed-release.json").read_bytes())
            self.assertEqual(marker["sha256"], entry["archive"]["sha256"])
            self.assertEqual(marker["asset"], Path(entry["archive"]["url"]).name)
            boot.install_verified(entry, "macos-arm64", library, archive, None, prefix, False)
            self.assertEqual(current.resolve(), active)
            (active / "bin/backend-cli").write_bytes(b"tampered")
            with self.assertRaises(boot.InstallError):
                boot.install_verified(entry, "macos-arm64", library, archive, None, prefix, False)
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
            mapping = {boot.CHANNEL: str(metadata), value["bootstrap"]["url"]: str(PUBLISHED_BOOTSTRAP_FIXTURE),
                       entry["installer"]["url"]: str(published_mac_library(root)), entry["archive"]["url"]: str(archive)}
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
            source = published_bootstrap_bytes().decode()
            prefix = root / "prefix"
            newer = copy.deepcopy(entry)
            newer["tag"] = newer["tag"].replace("20261006", "20261010")
            boot.install_verified(newer, "macos-arm64", published_mac_library(root), archive, None, prefix, False)
            original = (prefix / "lib/nudox/current").resolve()
            refusal = subprocess.run([sys.executable, str(PUBLISHED_BOOTSTRAP_FIXTURE), "--prefix", str(prefix)],
                                     text=True, capture_output=True, env=environment, timeout=20)
            self.assertEqual(refusal.returncode, 1, refusal.stderr)
            self.assertIn("older", refusal.stderr)
            self.assertEqual((prefix / "lib/nudox/current").resolve(), original)
            for mode in ["stdin", "file"]:
                command = [sys.executable, "-" if mode == "stdin" else str(PUBLISHED_BOOTSTRAP_FIXTURE),
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

    def test_actual_old_bootstrap_updates_before_distinct_new_library_stdin_and_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); entry, archive = mac_fixture(root)
            # Owned future-version fixture: only a library comment and the new
            # bootstrap's literal reviewed SHA/size constants differ. Its full
            # install() and every ownership/inventory/lease check remain normal.
            old_source = published_bootstrap_bytes()
            library_body = (ROOT / "install_macos_preview.py").read_bytes() + b"\n# Reviewed future-version Python fixture only.\n"
            library = root / "new-library.py"; library.write_bytes(library_body)
            library_sha = hashlib.sha256(library_body).hexdigest()
            old_sha, old_size, _ = boot.LIBRARIES["macos-arm64"]
            new_source = old_source.replace(old_sha.encode(), library_sha.encode()).replace(str(old_size).encode(), str(len(library_body)).encode())
            self.assertNotEqual(new_source, old_source)
            new_bootstrap = root / "new-bootstrap.py"; new_bootstrap.write_bytes(new_source)
            new_sha = hashlib.sha256(new_source).hexdigest()
            old_file = root / "old-bootstrap.py"; old_file.write_bytes(old_source)
            value = channel(); value["platforms"]["macos-arm64"] = entry
            entry["installer"] = {"url": boot.release_url(entry["tag"], "install-macos-arm64-" + library_sha[:16] + ".py"),
                                  "sha256": library_sha, "bytes": len(library_body)}
            value["bootstrap"] = {"url": boot.release_url(boot.BOOTSTRAP_TAG, "install-forgejo-bootstrap-" + new_sha[:16] + ".py"),
                                  "sha256": new_sha, "bytes": len(new_source)}
            boot.validate_channel(value)
            with self.assertRaises(boot.InstallError): boot.admit_libraries(value)
            metadata = root / "channel.json"; metadata.write_text(json.dumps(value))
            mapping = {boot.CHANNEL: str(metadata), value["bootstrap"]["url"]: str(new_bootstrap),
                       entry["installer"]["url"]: str(library), entry["archive"]["url"]: str(archive)}
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
            for mode in ["stdin", "file"]:
                prefix = root / mode
                command = [sys.executable, "-" if mode == "stdin" else str(old_file), "--prefix", str(prefix), "--allow-downgrade"]
                result = subprocess.run(command, input=old_source.decode() if mode == "stdin" else None,
                                        text=True, capture_output=True, env=environment, timeout=20)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue((prefix / "lib/nudox/current").is_symlink())
                requests = (root / "requests").read_text().splitlines()
                self.assertEqual(requests[0:3], [boot.CHANNEL, value["bootstrap"]["url"], boot.CHANNEL])
                self.assertIn(entry["installer"]["url"], requests[3:])
                (root / "requests").unlink()

    def test_r2_bootstrap_updates_old_linux_pin_before_library_admission(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); entry, archive = mac_fixture(root)
            new_source = published_bootstrap_bytes()
            new_sha = hashlib.sha256(new_source).hexdigest()
            old_pin = ("e11fcc271fc3a8feb676b9f8c383bbf5bdc215578ffc399774819184908ed071", 49954)
            current_pin = boot.LIBRARIES["linux-x64"][:2]
            old_row = f'"linux-x64": ("{current_pin[0]}", {current_pin[1]}, "install-linux-x64")'.encode()
            prior_row = f'"linux-x64": ("8baa4a60be98ee4e41e26c3b1cf0d4800e8ec4b8c80758128f38c19a06c799c9", 46973, "install-linux-x64")'.encode()
            self.assertEqual(current_pin, old_pin)
            self.assertEqual(new_source.count(old_row), 1)
            old_source = new_source.replace(old_row, prior_row, 1)
            self.assertNotEqual(old_source, new_source)
            old_file = root / "old-r2-bootstrap.py"; old_file.write_bytes(old_source)
            new_bootstrap = root / "current-r2-bootstrap.py"; new_bootstrap.write_bytes(new_source)
            value = channel(); value["platforms"]["macos-arm64"] = entry
            self.assertEqual(value["bootstrap"]["sha256"], new_sha)
            self.assertNotEqual(("8baa4a60be98ee4e41e26c3b1cf0d4800e8ec4b8c80758128f38c19a06c799c9", 46973),
                                (value["platforms"]["linux-x64"]["installer"]["sha256"], value["platforms"]["linux-x64"]["installer"]["bytes"]))
            metadata = root / "channel.json"; metadata.write_text(json.dumps(value))
            mapping = {boot.CHANNEL: str(metadata), value["bootstrap"]["url"]: str(new_bootstrap),
                       entry["installer"]["url"]: str(published_mac_library(root)),
                       entry["archive"]["url"]: str(archive)}
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
            prefix = root / "installed"
            result = subprocess.run([sys.executable, str(old_file), "--prefix", str(prefix), "--allow-downgrade"],
                                    text=True, capture_output=True, env=environment, timeout=20)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((prefix / "lib/nudox/current").is_symlink())
            requests = (root / "requests").read_text().splitlines()
            self.assertEqual(requests[0:3], [boot.CHANNEL, value["bootstrap"]["url"], boot.CHANNEL])
            self.assertIn(entry["installer"]["url"], requests[3:])
            self.assertNotIn(value["platforms"]["linux-x64"]["installer"]["url"], requests)


if __name__ == "__main__":
    unittest.main()
