#!/usr/bin/env python3
"""Execute generated launchers against a child probe, without opening a GUI.

These tests cover process environment and argument propagation. They do not
establish TypeScript host admission or native app-bundle acceptance.
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


PACKAGE_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(PACKAGE_DIR))
spec = importlib.util.spec_from_file_location(
    "macos_launcher_bundle", PACKAGE_DIR / "macos-investor-bundle.py"
)
if spec is None or spec.loader is None:
    raise RuntimeError("cannot load bundle generator")
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)

COMPILER_VARIABLES = (
    "NUDOX_TYPESCRIPT_MODULE_ROOT",
    "NUDOX_TYPESCRIPT_REPORT_PROGRAM",
    "NUDOX_TSC",
)


class LauncherEnvironmentTests(unittest.TestCase):
    def _executable(self, path: Path, text: str) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        path.chmod(0o755)
        return path

    def _bundle(self, root: Path) -> tuple[Path, Path]:
        contents = root.resolve() / "bundle with spaces/Nudox.app/Contents"
        macos = contents / "MacOS"
        macos.mkdir(parents=True)
        node = self._executable(
            contents / "Resources/Helpers/typescript/node/bin/node",
            "#!/bin/sh\nprintf '%s\\n' bundled-node-probe\n",
        )
        # Observe the actual child environment. When the caller selects Node,
        # execute that path to check argument quoting and propagation too.
        self._executable(
            macos / "backend-desktop",
            f"#!{sys.executable}\n"
            "import json, os, subprocess, sys\n"
            "node = os.environ.get('NUDOX_TYPESCRIPT_NODE')\n"
            "probe = subprocess.run([node, '--version'], check=True, capture_output=True, text=True).stdout.strip() if node else None\n"
            "print(json.dumps({'node': node, 'node_probe': probe,\n"
            "  'compiler': {key: os.environ.get(key) for key in\n"
            "    ['NUDOX_TYPESCRIPT_MODULE_ROOT', 'NUDOX_TYPESCRIPT_REPORT_PROGRAM', 'NUDOX_TSC']},\n"
            "  'args': sys.argv[1:]}))\n",
        )
        bundle.write_application_launcher(macos)
        return macos / "Nudox", node

    def _run(self, launcher: Path, overrides: dict[str, str]) -> dict:
        env = {"PATH": "/usr/bin:/bin", **overrides}
        result = subprocess.run(
            [str(launcher), "--project", "project with spaces", "literal $(not executed)"],
            env=env,
            cwd=launcher.parent.parent.parent.parent,
            capture_output=True,
            text=True,
            timeout=10,
            check=True,
        )
        self.assertEqual(result.stderr, "")
        return json.loads(result.stdout)

    def test_bundle_does_not_inject_typescript_runtime_or_compiler_overrides(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            launcher, _ = self._bundle(Path(temporary))
            observed = self._run(launcher, {})
            self.assertIsNone(observed["node"])
            self.assertIsNone(observed["node_probe"])
            self.assertEqual(observed["compiler"], dict.fromkeys(COMPILER_VARIABLES))
            self.assertEqual(
                observed["args"],
                ["--project", "project with spaces", "literal $(not executed)"],
            )

    def test_explicit_user_compiler_and_node_paths_propagate_verbatim(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            launcher, _ = self._bundle(root)
            user_node = self._executable(
                root / "user runtime/node",
                "#!/bin/sh\nprintf '%s\\n' explicit-node-probe\n",
            )
            values = {
                "NUDOX_TYPESCRIPT_MODULE_ROOT": str(root / "project/node_modules"),
                "NUDOX_TYPESCRIPT_REPORT_PROGRAM": str(root / "user report $(literal)"),
                "NUDOX_TSC": str(root / "user compiler with spaces"),
            }
            observed = self._run(
                launcher, {**values, "NUDOX_TYPESCRIPT_NODE": str(user_node)}
            )
            self.assertEqual(observed["node"], str(user_node))
            self.assertEqual(observed["node_probe"], "explicit-node-probe")
            self.assertEqual(observed["compiler"], values)

    def test_explicit_empty_typescript_values_are_not_replaced_with_defaults(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            launcher, _ = self._bundle(Path(temporary))
            values = dict.fromkeys(COMPILER_VARIABLES, "")
            observed = self._run(launcher, {**values, "NUDOX_TYPESCRIPT_NODE": ""})
            self.assertEqual(observed["compiler"], values)
            self.assertEqual(observed["node"], "")
            self.assertIsNone(observed["node_probe"])


if __name__ == "__main__":
    unittest.main()
