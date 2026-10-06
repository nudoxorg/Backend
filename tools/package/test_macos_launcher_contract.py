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
        # The child executes the selected Node path, so a missing, unexported,
        # or incorrectly quoted bundled runtime cannot pass this contract.
        self._executable(
            macos / "backend-desktop",
            f"#!{sys.executable}\n"
            "import json, os, subprocess, sys\n"
            "node = os.environ['NUDOX_TYPESCRIPT_NODE']\n"
            "probe = subprocess.run([node, '--version'], check=True, capture_output=True, text=True)\n"
            "print(json.dumps({'node': node, 'node_probe': probe.stdout.strip(),\n"
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

    def test_bundled_node_runs_without_forcing_a_project_compiler(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            launcher, node = self._bundle(Path(temporary))
            observed = self._run(launcher, {})
            self.assertEqual(Path(observed["node"]), node)
            self.assertEqual(observed["node_probe"], "bundled-node-probe")
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

    def test_explicit_empty_compiler_values_are_not_replaced_with_defaults(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            launcher, _ = self._bundle(Path(temporary))
            values = dict.fromkeys(COMPILER_VARIABLES, "")
            self.assertEqual(self._run(launcher, values)["compiler"], values)

    def test_package_manager_symlinks_preserve_helpers_and_arguments(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            gui, node = self._bundle(root)
            for name, binary in {"nudox-cli": "backend-cli", "nudox-mcp": "backend-mcp", "nudox-locald": "backend-locald"}.items():
                probe = gui.parent / binary
                self._executable(probe, (gui.parent / "backend-desktop").read_text())
                link = root / "bin" / name
                link.parent.mkdir(exist_ok=True)
                link.symlink_to(gui.parent / name)
                observed = self._run(link, {})
                self.assertEqual(Path(observed["node"]), node)
                self.assertEqual(observed["compiler"], dict.fromkeys(COMPILER_VARIABLES))
                self.assertEqual(observed["args"], ["--project", "project with spaces", "literal $(not executed)"])


if __name__ == "__main__":
    unittest.main()
