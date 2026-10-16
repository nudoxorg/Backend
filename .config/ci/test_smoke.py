"""Failure contracts for the surface smoke driver; no product build needed."""
import contextlib
import io
import json
import os
import platform
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

    def test_gui_shutdown_diagnostics_sample_owner_threads_with_bounds(self):
        proc_root = self.root / "proc"

        def process(pid, threads, children="", thread_children=None):
            base = proc_root / str(pid)
            (base / "task" / str(pid)).mkdir(parents=True)
            (base / "task" / str(pid) / "children").write_text(children)
            (base / "cwd").symlink_to(self.home)
            (base / "environ").write_text("PRIVATE_ENV_SENTINEL")
            (base / "cmdline").write_text("PRIVATE_ARG_SENTINEL")
            thread_children = thread_children or {}
            for tid, comm in threads:
                thread = base / "task" / str(tid)
                thread.mkdir(parents=True, exist_ok=True)
                (thread / "comm").write_text(comm)
                (thread / "children").write_text(thread_children.get(tid, ""))
                (thread / "wchan").write_text("futex_wait_queue")
                (thread / "stack").write_text("[<0>] schedule+0x1/0x2")
                fsync_number = 82 if platform.machine().casefold() in ("aarch64", "arm64") else 74
                (thread / "syscall").write_text(f"{fsync_number} 7 0 0 0 0 0 0 0\n")
            (base / "fd").mkdir()
            (base / "fd" / "7").symlink_to(self.home / "workspace")

        ordinary = [(400 + index, f"ui-frame-{index}") for index in range(34)]
        process(400, [*ordinary, (9000, "backend-owner")], thread_children={433: "450"})
        process(450, [(450, "locald-worker")])

        snapshot = smoke._gui_process_diagnostics(400, proc_root)

        self.assertLessEqual(snapshot.count(" tid="), 32)
        self.assertIn("tid=9000 comm='backend-owner'", snapshot)
        self.assertIn("name=fsync fd=7", snapshot)
        self.assertIn(str(self.home / "workspace"), snapshot)
        self.assertIn("tid=450 comm='locald-worker'", snapshot)
        self.assertIn(f"pid=400 cwd='{self.home}'", snapshot)
        self.assertNotIn("PRIVATE_ENV_SENTINEL", snapshot)
        self.assertNotIn("PRIVATE_ARG_SENTINEL", snapshot)

    def test_syscall_diagnostic_exposes_only_number_and_sync_fd_path(self):
        proc_root = self.root / "proc"
        thread = proc_root / "81" / "task" / "82"
        thread.mkdir(parents=True)
        (thread / "syscall").write_text("75 9 0 0 0 0 0 0 0\n")
        (proc_root / "81" / "fd").mkdir()
        (proc_root / "81" / "fd" / "9").symlink_to(self.home / "object.cas")

        sync = smoke._thread_syscall_diagnostic(proc_root, 81, 82, "x86_64")
        ordinary = thread / "syscall"
        ordinary.write_text("42 987654 0 0 0 0 0 0 0\n")
        other = smoke._thread_syscall_diagnostic(proc_root, 81, 82, "x86_64")

        self.assertEqual(
            sync,
            f"syscall=75 name=fdatasync fd=9 path={str(self.home / 'object.cas')!r}",
        )
        self.assertEqual(other, "syscall=42 (not fsync/fdatasync)")
        self.assertNotIn("987654", other)

    def test_shutdown_trace_keeps_lifecycle_order_and_filters_frame_flood(self):
        trace = self.root / "trace.jsonl"
        records = [
            {"name": "boot.owner_host_start.begin", "t_ms": 2, "detail": "private data"},
            *(
                {"name": "frame_collection", "t_ms": index, "timings": []}
                for index in range(300)
            ),
            *(
                {"name": f"boot.phase_{index}", "t_ms": index + 300}
                for index in range(80)
            ),
            {"name": "close.finish", "t_ms": 400, "detail": "private data"},
        ]
        trace.write_text("\n".join(json.dumps(record) for record in records))

        lifecycle = smoke._lifecycle_trace(trace)

        self.assertLess(lifecycle.index("boot.owner_host_start.begin"), lifecycle.index("close.finish"))
        self.assertNotIn("frame_collection", lifecycle)
        self.assertNotIn("private data", lifecycle)
        self.assertLessEqual(len(lifecycle.splitlines()), 64)
        self.assertLess(len(lifecycle.encode()), 16 * 1024)

    def test_shutdown_startup_log_keeps_only_workspace_phases(self):
        log = self.root / "desktop.log"
        log.write_text(
            "unrelated diagnostic with secret\n"
            "backend-local-service: owner startup phase WorkspaceOpen\n"
            "backend-engine: workspace open genesis_seed.begin\n"
            "backend-local-service: workspace genesis admitted\n"
        )

        phases = smoke._startup_log(log)

        self.assertIn("owner startup phase WorkspaceOpen", phases)
        self.assertIn("workspace open genesis_seed.begin", phases)
        self.assertIn("workspace genesis admitted", phases)
        self.assertNotIn("unrelated", phases)
        self.assertNotIn("secret", phases)

    def test_shutdown_startup_log_summary_is_bounded(self):
        log = self.root / "many-phases.log"
        log.write_text("".join(
            f"backend-engine: workspace open phase_{index} " + ("x" * 400) + "\n"
            for index in range(100)
        ))

        phases = smoke._startup_log(log)

        self.assertLessEqual(len(phases.splitlines()), 64)
        self.assertLess(len(phases.encode()), 16 * 1024)
        self.assertIn("startup phase records omitted", phases)

    def gui(self, visible=True, quit_code=0, ignore_quit=False, injection_error=False):
        def executable(name, code):
            path = self.bin_dir / name
            path.write_text(f"#!{os.sys.executable}\n" + code)
            path.chmod(0o700)

        executable("Xvfb", "import os, sys, time\n"
                   "assert 'GENERIC_CREDENTIAL_SENTINEL' not in os.environ\n"
                   "assert os.environ.get('LANG') == 'C.UTF-8'\n"
                   "assert os.environ.get('LC_ALL') == 'en_US.UTF-8'\n"
                   "assert os.environ.get('LD_LIBRARY_PATH') == '/nix/runtime/lib'\n"
                   "assert os.environ.get('__EGL_VENDOR_LIBRARY_DIRS') == '/nix/mesa/egl'\n"
                   "assert os.environ.get('LIBGL_DRIVERS_PATH') == '/nix/mesa/dri'\n"
                   "assert os.environ.get('VK_DRIVER_FILES') == '/nix/mesa/lvp.json'\n"
                   "assert os.environ.get('VK_ICD_FILENAMES') == '/nix/mesa/lvp.json'\n"
                   "assert os.environ.get('FONTCONFIG_FILE') == '/nix/fonts/fonts.conf'\n"
                   "print('fixture X server', file=sys.stderr, flush=True)\n"
                   "os.write(int(sys.argv[2]), b'91\\n')\ntime.sleep(30)\n")
        executable("xdotool", "import os, sys\nfrom pathlib import Path\n"
                   f"visible = {visible!r}\n"
                   "if sys.argv[1] == 'search':\n"
                   "    if visible: print('123')\n"
                   "    sys.exit(0 if visible else 1)\n"
                   "if sys.argv[1] == 'windowfocus':\n"
                   "    assert sys.argv[2:] == ['--sync', '123'], 'focus and key must be separate'\n"
                   "    sys.exit(0)\n"
                   "assert sys.argv[1:] == ['key', '--clearmodifiers', 'super+q']\n"
                   f"if {injection_error!r}:\n"
                   "    print('fixture XTest delivery refused', file=sys.stderr)\n"
                   "    sys.exit(4)\n"
                   "Path(os.environ['HOME'], 'quit').touch()\n")
        executable("backend-desktop", "import os, sys, time\nfrom pathlib import Path\n"
                   "assert 'GENERIC_CREDENTIAL_SENTINEL' not in os.environ\n"
                   "assert os.environ.get('LANG') == 'C.UTF-8'\n"
                   "assert os.environ.get('LC_ALL') == 'en_US.UTF-8'\n"
                   "assert os.environ.get('LD_LIBRARY_PATH') == '/nix/runtime/lib'\n"
                   "assert os.environ.get('__EGL_VENDOR_LIBRARY_DIRS') == '/nix/mesa/egl'\n"
                   "assert os.environ.get('LIBGL_DRIVERS_PATH') == '/nix/mesa/dri'\n"
                   "assert os.environ.get('VK_DRIVER_FILES') == '/nix/mesa/lvp.json'\n"
                   "assert os.environ.get('VK_ICD_FILENAMES') == '/nix/mesa/lvp.json'\n"
                   "assert os.environ.get('FONTCONFIG_FILE') == '/nix/fonts/fonts.conf'\n"
                   "Path(os.environ['NUDOX_TRACE']).write_text('{\"name\":\"frame\"}\\n')\n"
                   "deadline = time.monotonic() + 3\n"
                   "while not Path(os.environ['HOME'], 'quit').exists():\n"
                   "    if time.monotonic() > deadline: sys.exit(9)\n"
                   "    time.sleep(0.01)\n"
                   "print('fixture received Quit', flush=True)\n"
                   f"if {ignore_quit!r}: time.sleep(30)\n"
                   f"sys.exit({quit_code})\n")
        stderr = io.StringIO()
        with patch.dict(os.environ, {
            "PATH": str(self.bin_dir),
            "GENERIC_CREDENTIAL_SENTINEL": "must-not-cross-boundary",
            "LANG": "C.UTF-8",
            "LC_ALL": "en_US.UTF-8",
            "LD_LIBRARY_PATH": "/nix/runtime/lib",
            "__EGL_VENDOR_LIBRARY_DIRS": "/nix/mesa/egl",
            "LIBGL_DRIVERS_PATH": "/nix/mesa/dri",
            "VK_DRIVER_FILES": "/nix/mesa/lvp.json",
            "VK_ICD_FILENAMES": "/nix/mesa/lvp.json",
            "FONTCONFIG_FILE": "/nix/fonts/fonts.conf",
        }), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(stderr):
            result = smoke.smoke_gui(self.bin_dir, self.home, 5)
        self.gui_error = stderr.getvalue()
        return result

    def test_gui_requires_visible_window_and_successful_public_quit(self):
        self.assertEqual(self.gui(), 0)

    def test_gui_quit_injection_failure_is_not_accepted_or_hidden(self):
        self.assertEqual(self.gui(injection_error=True), 1)
        self.assertIn("Quit key delivery failed with exit 4", self.gui_error)
        self.assertIn("fixture XTest delivery refused", self.gui_error)

    def test_gui_paint_without_visible_window_fails(self):
        self.assertEqual(self.gui(visible=False), 1)

    def test_gui_crash_on_quit_fails(self):
        self.assertEqual(self.gui(quit_code=7), 1)

    def test_gui_hanging_after_public_quit_fails_with_diagnostics(self):
        self.assertEqual(self.gui(ignore_quit=True), 1)
        self.assertIn("did not shut down within 10s after Quit", self.gui_error)
        self.assertIn("fixture received Quit", self.gui_error)
        self.assertIn("desktop.log:", self.gui_error)
        self.assertIn("xvfb.log:", self.gui_error)
        self.assertIn("trace.jsonl:", self.gui_error)

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

    def test_environment_drops_generic_credentials_and_keeps_runtime_locale(self):
        source = {
            "PATH": "/nix/toolchain/bin",
            "LD_LIBRARY_PATH": "/nix/gui/lib",
            "__EGL_VENDOR_LIBRARY_DIRS": "/nix/mesa/egl",
            "LIBGL_DRIVERS_PATH": "/nix/mesa/dri",
            "VK_DRIVER_FILES": "/nix/mesa/lvp.json",
            "VK_ICD_FILENAMES": "/nix/mesa/lvp.json",
            "FONTCONFIG_FILE": "/nix/fonts/fonts.conf",
            "LANG": "C.UTF-8",
            "LC_ALL": "en_US.UTF-8",
            "GENERIC_CREDENTIAL_SENTINEL": "must-not-cross-boundary",
            "OPENAI_API_KEY": "also-secret",
            "AWS_SECRET_ACCESS_KEY": "also-secret",
            "GITHUB_TOKEN": "also-secret",
            "HOME": "/real/home",
            "CARGO_HOME": "/real/cargo-home",
        }
        env = smoke.isolated_env(self.home, source)

        self.assertEqual(env["PATH"], "/nix/toolchain/bin")
        self.assertEqual(env["LD_LIBRARY_PATH"], "/nix/gui/lib")
        self.assertEqual(env["__EGL_VENDOR_LIBRARY_DIRS"], "/nix/mesa/egl")
        self.assertEqual(env["LIBGL_DRIVERS_PATH"], "/nix/mesa/dri")
        self.assertEqual(env["VK_DRIVER_FILES"], "/nix/mesa/lvp.json")
        self.assertEqual(env["VK_ICD_FILENAMES"], "/nix/mesa/lvp.json")
        self.assertEqual(env["FONTCONFIG_FILE"], "/nix/fonts/fonts.conf")
        self.assertEqual(env["LANG"], "C.UTF-8")
        self.assertEqual(env["LC_ALL"], "en_US.UTF-8")
        self.assertEqual(env["HOME"], str(self.home))
        self.assertEqual(env["TMPDIR"], str(self.home / "tmp"))
        for name in (
            "GENERIC_CREDENTIAL_SENTINEL",
            "OPENAI_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "CARGO_HOME",
        ):
            self.assertNotIn(name, env)


if __name__ == "__main__":
    unittest.main()
