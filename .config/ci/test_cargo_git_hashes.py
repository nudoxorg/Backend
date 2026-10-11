import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "git_hashes", Path(__file__).with_name("check-cargo-git-hashes.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class GitHashCoverageTests(unittest.TestCase):
    def test_new_git_dependency_fails_before_vendoring(self):
        lock = {"package": [
            {"name": "lsp-types", "version": "0.95.2", "source": "git+https://example.org/lsp#abc"},
            {"name": "serde", "version": "1.0", "source": "registry+https://example.org"},
        ]}
        self.assertEqual(module.validate(lock, {}), [
            "lsp-types-0.95.2: missing or invalid Nix Git source hash"
        ])
        self.assertEqual(module.validate(lock, {
            "lsp-types-0.95.2": "sha256-" + "A" * 43 + "="
        }), [])

    def test_shared_repository_requires_every_package_pin(self):
        lock = {"package": [
            {"name": name, "version": "1", "source": "git+https://example.org/shared#abc"}
            for name in ["library", "derive"]
        ]}
        self.assertEqual(module.validate(lock, {
            "library-1": "sha256-" + "A" * 43 + "="
        }), ["derive-1: missing or invalid Nix Git source hash"])


if __name__ == "__main__":
    unittest.main()
