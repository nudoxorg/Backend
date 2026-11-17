"""External CLI controls for artifact admission; native qualification uses a real T1 build."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("t1_artifacts.py").resolve()
SHA = "a" * 40
TARGET = "x86_64-unknown-linux-gnu"
STORE = "/nix/store/" + "b" * 32 + "-runtime"
PRODUCTS = ("backend-cli", "backend-mcp", "backend-desktop", "backend-locald", "backend-metrics")
WORKERS = ("backend-stylo-worker", "backend-sqlite-worker")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class ArtifactProcessControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "artifact"
        for directory in ("bin", "lib", "observers"):
            (self.root / directory).mkdir(parents=True)
        names = ([f"bin/{name}" for name in (*PRODUCTS, *WORKERS)] +
                 ["lib/libgcc_s.so.1", "observers/stylo_identity_observer",
                  "observers/installed_basis_observer", "smoke.py", "runtime.nar"])
        for name in names:
            (self.root / name).write_bytes(b"synthetic verifier fixture")
        observation = {
            "schema": "nudox.keel-development-native-observation.v1", "source_revision": SHA,
            "source_status": "", "host": TARGET,
            "stylo_worker_sha256": digest(self.root / "bin/backend-stylo-worker"),
            "sqlite_worker_sha256": digest(self.root / "bin/backend-sqlite-worker"),
            "libgcc_sha256": digest(self.root / "lib/libgcc_s.so.1"),
            "stylo_observer_sha256": digest(self.root / "observers/stylo_identity_observer"),
            "sqlite_observer_sha256": digest(self.root / "observers/installed_basis_observer"),
            "stylo": {"state": "observed-helper", "identity": "1" * 64},
            "sqlite": {"state": "observed-build-helper", "artifact_identity": "2" * 64},
            "product_outputs": [{"name": name, "sha256": digest(self.root / "bin" / name)} for name in PRODUCTS],
        }
        (self.root / "development-observation.json").write_text(json.dumps(observation))
        names.append("development-observation.json")
        self.manifest = {"schema": 2, "profile": "debug", "source_sha": SHA, "target": TARGET,
                         "toolchain": "synthetic fixture", "runtime_roots": [STORE],
                         "python": STORE + "/bin/python3", "runtime_env": {"PATH": STORE + "/bin"},
                         "files": [{"path": name, "bytes": (self.root / name).stat().st_size,
                                    "sha256": digest(self.root / name)} for name in names]}
        self.save()

    def save(self):
        (self.root / "manifest.json").write_text(json.dumps(self.manifest))

    def run_cli(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *map(str, args)], capture_output=True, text=True)

    def verify(self, *extra):
        return self.run_cli("verify", self.root, "--source-sha", SHA, *extra)

    def test_static_contract_and_inventory(self):
        # Synthetic bytes exercise admission only; they are not a native qualification.
        self.assertEqual(self.verify().returncode, 0)
        result = self.run_cli("inventory", self.root, "--source-sha", SHA)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["artifact_manifest_sha256"], digest(self.root / "manifest.json"))

    def test_wrong_source_and_target(self):
        self.assertNotEqual(self.run_cli("verify", self.root, "--source-sha", "c" * 40).returncode, 0)
        self.assertNotEqual(self.verify("--target", "aarch64-unknown-linux-gnu").returncode, 0)

    def test_manifest_controls(self):
        for field, value in (("schema", 1), ("profile", "release"), ("toolchain", ""),
                             ("runtime_roots", ["/tmp/runtime"]),
                             ("python", "/nix/store/" + "c" * 32 + "-other/bin/python3"),
                             ("runtime_env", {"PATH": "/usr/bin"}),
                             ("runtime_env", {"FORGEJO_TOKEN": "private"})):
            with self.subTest(field=field, value=value):
                old = self.manifest[field]
                self.manifest[field] = value
                self.save()
                self.assertNotEqual(self.verify().returncode, 0)
                self.manifest[field] = old
        self.save()
        for path in ("../outside", self.manifest["files"][1]["path"]):
            original = self.manifest["files"][0]["path"]
            self.manifest["files"][0]["path"] = path
            self.save()
            self.assertNotEqual(self.verify().returncode, 0)
            self.manifest["files"][0]["path"] = original

    def test_damaged_missing_and_symlinked_payloads(self):
        for name in ("bin/backend-cli", "bin/backend-stylo-worker", "lib/libgcc_s.so.1", "runtime.nar"):
            path = self.root / name
            original = path.read_bytes()
            path.write_bytes(b"damaged")
            self.assertNotEqual(self.verify().returncode, 0)
            path.unlink()
            self.assertNotEqual(self.verify().returncode, 0)
            path.symlink_to(self.root / "smoke.py")
            self.assertNotEqual(self.verify().returncode, 0)
            path.unlink()
            path.write_bytes(original)

    def test_inventory_and_import_reject_damage_before_execution(self):
        (self.root / "runtime.nar").write_bytes(b"damaged")
        for action in (("inventory", self.root, "--source-sha", SHA),
                       ("verify", self.root, "--source-sha", SHA, "--import-runtime"),
                       ("verify", self.root, "--source-sha", SHA, "--observe-native")):
            result = self.run_cli(*action)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("artifact checksum mismatch", result.stderr)

    def test_symlinked_inventory_rejected(self):
        path = self.root / "manifest.json"
        data = path.read_text()
        path.unlink()
        (self.root / "elsewhere.json").write_text(data)
        path.symlink_to(self.root / "elsewhere.json")
        self.assertNotEqual(self.verify().returncode, 0)

    def test_observation_and_owner_mismatch_rejected(self):
        path = self.root / "development-observation.json"
        original = json.loads(path.read_text())
        for field, value in (("source_revision", "c" * 40), ("source_status", " M source"),
                             ("stylo", {"state": "observed-helper", "identity": "0" * 64}),
                             ("stylo_worker_sha256", "0" * 64), ("product_outputs", [])):
            changed = dict(original)
            changed[field] = value
            path.write_text(json.dumps(changed))
            item = next(item for item in self.manifest["files"] if item["path"] == path.name)
            item.update(bytes=path.stat().st_size, sha256=digest(path))
            self.save()
            self.assertNotEqual(self.verify().returncode, 0)

    def test_dirty_checkout_is_rejected_by_real_producer(self):
        checkout = Path(self.temp.name) / "checkout"
        checkout.mkdir()
        subprocess.run(["git", "init", "-q", str(checkout)], check=True)
        (checkout / "tracked").write_text("before")
        subprocess.run(["git", "add", "tracked"], cwd=checkout, check=True)
        subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                        "commit", "-qm", "fixture"], cwd=checkout, check=True)
        (checkout / "tracked").write_text("after")
        output = Path(self.temp.name) / "out"
        result = subprocess.run([sys.executable, str(SCRIPT), "publish", "--stage", str(self.root),
                                 "--output", str(output)], cwd=checkout, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("clean tracked checkout", result.stderr)
        self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
