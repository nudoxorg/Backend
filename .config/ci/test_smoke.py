"""Failure contracts for the surface smoke driver; no product build needed."""
import contextlib
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import smoke


class SmokeContract(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.home = self.root / "home"
        self.home.mkdir()

    def mcp(self, replies, exit_code=0):
        binary = self.bin_dir / "backend-mcp"
        binary.write_text(
            f"#!{os.sys.executable}\n"
            "import sys\n"
            "sys.stdin.read()\n"
            f"print({replies!r})\n"
            f"sys.exit({exit_code})\n"
        )
        binary.chmod(0o700)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return smoke.smoke_mcp(self.bin_dir, self.home)

    def valid_replies(self):
        return "\n".join(json.dumps(reply) for reply in [
            {"jsonrpc": "2.0", "id": 1, "result": {
                "serverInfo": {"name": "fixture"}, "capabilities": {"tools": {}},
            }},
            {"jsonrpc": "2.0", "id": 2, "result": {"tools": [{"name": "fixture"}]}},
        ])

    def test_mcp_metadata_handshake(self):
        self.assertEqual(self.mcp(self.valid_replies()), 0)

    def test_mcp_crash_after_valid_replies_fails(self):
        self.assertEqual(self.mcp(self.valid_replies(), 7), 1)

    def test_mcp_protocol_error_fails(self):
        self.assertEqual(self.mcp('{"jsonrpc":"2.0","id":1,"error":{"code":-1}}'), 1)

    def test_mcp_non_json_output_fails(self):
        self.assertEqual(self.mcp("server started"), 1)

    def test_mcp_duplicate_reply_fails(self):
        self.assertEqual(self.mcp(self.valid_replies() + "\n" + self.valid_replies()), 1)

    def test_mcp_empty_tools_fails(self):
        self.assertEqual(self.mcp(self.valid_replies().replace('[{"name": "fixture"}]', '[]')), 1)

    def test_empty_frame_collection_and_ticks_are_not_a_paint(self):
        trace = self.root / "trace.jsonl"
        trace.write_text('\n'.join(json.dumps(event) for event in [
            {"name": "frame_lifecycle", "kind": "frame_tick"},
            {"name": "frame_collection", "timings": 0, "lifecycle": 2},
        ]))
        self.assertFalse(smoke.drew_a_frame(trace))
        with trace.open("a") as out:
            out.write('\n{"name":"frame","draw_ms":0.8}\n')
        self.assertTrue(smoke.drew_a_frame(trace))

    def gui(self, visible=True, quit_code=0):
        def executable(name, code):
            path = self.bin_dir / name
            path.write_text(f"#!{os.sys.executable}\n" + code)
            path.chmod(0o700)

        executable("Xvfb", "import os, sys, time\nos.write(int(sys.argv[2]), b'91\\n')\ntime.sleep(30)\n")
        executable("xdotool", "import os, sys\nfrom pathlib import Path\n"
                   f"visible = {visible!r}\n"
                   "if sys.argv[1] == 'search':\n"
                   "    if visible: print('123')\n"
                   "    sys.exit(0 if visible else 1)\n"
                   "Path(os.environ['HOME'], 'quit').touch()\n")
        executable("backend-desktop", "import os, sys, time\nfrom pathlib import Path\n"
                   "Path(os.environ['NUDOX_TRACE']).write_text('{\"name\":\"frame\"}\\n')\n"
                   "deadline = time.monotonic() + 3\n"
                   "while not Path(os.environ['HOME'], 'quit').exists():\n"
                   "    if time.monotonic() > deadline: sys.exit(9)\n"
                   "    time.sleep(0.01)\n"
                   f"sys.exit({quit_code})\n")
        with patch.dict(os.environ, {"PATH": str(self.bin_dir)}), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return smoke.smoke_gui(self.bin_dir, self.home, 5)

    def test_gui_requires_visible_window_and_successful_public_quit(self):
        self.assertEqual(self.gui(), 0)

    def test_gui_paint_without_visible_window_fails(self):
        self.assertEqual(self.gui(visible=False), 1)

    def test_gui_crash_on_quit_fails(self):
        self.assertEqual(self.gui(quit_code=7), 1)

    def test_environment_cannot_attach_to_real_workspace(self):
        with patch.dict(os.environ, {
            "BACKEND_PROJECT": "/real/project", "BACKEND_LOCALD_ENDPOINT": "/real/socket",
            "NUDOX_TRACE": "/real/trace", "WAYLAND_DISPLAY": "wayland-real", "DISPLAY": ":0",
        }):
            env = smoke.isolated_env(self.home)
        self.assertFalse(any(name.startswith(("BACKEND_", "NUDOX_")) for name in env))
        self.assertNotIn("DISPLAY", env)
        self.assertNotIn("WAYLAND_DISPLAY", env)
        self.assertEqual(env["HOME"], str(self.home))


if __name__ == "__main__":
    unittest.main()
