"""The snapshot admission contract must reject failed product setup."""
import importlib.util
from concurrent.futures import Future
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("surface", Path(__file__).with_name("surface.py"))
surface = importlib.util.module_from_spec(spec)
spec.loader.exec_module(surface)


class FailedIndexDiagnosticTests(unittest.TestCase):
    def test_diagnostic_reads_public_outcome_without_changing_setup_verdict(self):
        import contextlib, io, json
        from unittest.mock import patch
        owner = object()
        project = Path("/tmp/nudox-surface/example/project")
        reply = {"exit": 0, "json": {"answer": "compiler-outcome", "failures": ["retained refusal"]}}
        with patch.object(surface, "run_cli", return_value=(reply, 0.1)) as run, contextlib.redirect_stdout(io.StringIO()) as output:
            surface.emit_index_failure_diagnostic(owner, project)
        run.assert_called_once_with(owner, project, [
            "package-profile", str(project),
            "--compiler-outcome", '{"kind":"current"}',
        ], 10)
        packet = json.loads(output.getvalue().split(" ", 1)[1])
        self.assertEqual(packet, {"project": str(project), "reply": reply})

    def test_diagnostic_failure_cannot_replace_the_original_exception(self):
        from unittest.mock import patch
        with patch.object(surface, "run_cli", side_effect=RuntimeError("owner gone")):
            # The caller's bare raise must still rethrow its setup exception.
            with self.assertRaisesRegex(ValueError, "original setup refusal"):
                try:
                    raise ValueError("original setup refusal")
                except Exception:
                    surface.emit_index_failure_diagnostic(object(), Path("/tmp/project"))
                    raise


class RuntimeImportDrainTests(unittest.TestCase):
    def test_linux_flushes_only_task_filesystem_with_bounded_setup(self):
        import subprocess
        from unittest.mock import patch
        with patch.object(surface.sys, "platform", "linux"), \
                patch.object(surface.process_custody, "run", return_value=subprocess.CompletedProcess([], 0, "", "")) as run:
            surface.drain_task_volume()
        run.assert_called_once_with(
            ["sync", "--file-system", str(surface.ROOT)],
            capture_output=True, text=True, timeout=60,
        )

    def test_drain_failure_cannot_become_a_product_pass(self):
        import subprocess
        from unittest.mock import patch
        with patch.object(surface.sys, "platform", "linux"), \
                patch.object(surface.process_custody, "run", return_value=subprocess.CompletedProcess([], 1, "", "I/O error")):
            with self.assertRaisesRegex(RuntimeError, "infrastructure.*I/O error"):
                surface.drain_task_volume()

    def test_timeout_is_an_infrastructure_failure(self):
        import subprocess
        from unittest.mock import patch
        with patch.object(surface.sys, "platform", "linux"), \
                patch.object(surface.process_custody, "run", side_effect=subprocess.TimeoutExpired("sync", 60)):
            with self.assertRaisesRegex(RuntimeError, "infrastructure.*exceeded 60 seconds"):
                surface.drain_task_volume()

    def test_other_platforms_do_not_invoke_linux_syncfs(self):
        from unittest.mock import patch
        with patch.object(surface.sys, "platform", "darwin"), \
                patch.object(surface.process_custody, "run") as run:
            surface.drain_task_volume()
        run.assert_not_called()


class LocaldStartupDiagnosticTests(unittest.TestCase):
    def test_process_context_binds_child_pid_namespace_boot_and_cgroup(self):
        import json, tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = root / "case"
            workspace = root / "workspace"
            proc_root = root / "proc"
            child = proc_root / "42"
            observer = proc_root / "self"
            case.mkdir()
            workspace.mkdir()
            (child / "ns").mkdir(parents=True)
            (observer / "ns").mkdir(parents=True)
            (child / "fd").mkdir()
            (proc_root / "sys/kernel/random").mkdir(parents=True)
            (proc_root / "ns").mkdir()
            for name in ("pid-observer", "pid-child", "mnt-observer", "mnt-child"):
                (proc_root / "ns" / name).write_text(name)
            (observer / "ns/pid").symlink_to(proc_root / "ns/pid-observer")
            (child / "ns/pid").symlink_to(proc_root / "ns/pid-child")
            (observer / "ns/mnt").symlink_to(proc_root / "ns/mnt-observer")
            (child / "ns/mnt").symlink_to(proc_root / "ns/mnt-child")
            (observer / "status").write_text("Name:\tpytest\nNSpid:\t101 7\n")
            (child / "status").write_text("Name:\tbackend-locald\nNSpid:\t202 42\n")
            (observer / "cgroup").write_text("0::/fixture/task\n")
            (child / "cgroup").write_text("0::/fixture/task/owner\n")
            fields = ["S"] + ["1"] * 18 + ["987654321"]
            (child / "stat").write_text("42 (backend-locald) " + " ".join(fields) + "\n")
            (proc_root / "sys/kernel/random/boot_id").write_text("fixture-boot-id\n")

            observed = json.loads(surface._locald_process_diagnostic(
                42, case, workspace, None, proc_root=proc_root))
            context = observed["process_context"]
            self.assertEqual(context["child_pid"], 42)
            self.assertEqual(context["observer_nspid"], [101, 7])
            self.assertEqual(context["child_nspid"], [202, 42])
            self.assertEqual(context["child_starttime_ticks"], 987654321)
            self.assertEqual(context["boot_id"], "fixture-boot-id")
            self.assertEqual(context["child_cgroup"], "0::/fixture/task/owner")
            self.assertEqual(context["observer_cgroup"], "0::/fixture/task")
            self.assertTrue(context["child_pid_namespace"]["target"].endswith("/ns/pid-child"))
            self.assertTrue(context["observer_pid_namespace"]["target"].endswith("/ns/pid-observer"))
            self.assertEqual(context["child_pid_namespace"]["inode"],
                             (proc_root / "ns/pid-child").stat().st_ino)
            self.assertNotIn("Name:", json.dumps(context))

    def test_process_context_reports_missing_and_permission_limited_identity(self):
        import json, tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = root / "case"
            workspace = root / "workspace"
            proc_root = root / "proc"
            case.mkdir()
            workspace.mkdir()

            missing = json.loads(surface._locald_process_diagnostic(
                42, case, workspace, None, proc_root=proc_root))["process_context"]
            self.assertEqual(missing["child_pid_namespace"], "unsupported:FileNotFoundError")
            self.assertEqual(missing["child_nspid"], "unsupported:FileNotFoundError")
            self.assertEqual(missing["child_starttime_ticks"], "unsupported:FileNotFoundError")
            self.assertEqual(missing["boot_id"], "unsupported:FileNotFoundError")

            child = proc_root / "42"
            (child / "ns").mkdir(parents=True)
            (child / "status").write_text("NSpid:\t42\n")
            (child / "cgroup").write_text("0::/fixture/task/owner\n")
            fields = ["S"] + ["1"] * 18 + ["123"]
            (child / "stat").write_text("42 (backend-locald) " + " ".join(fields) + "\n")
            original_readlink = surface.os.readlink
            original_open = open

            def readlink(path, *args, **kwargs):
                if str(path) == str(child / "ns/pid"):
                    raise PermissionError("fixture denied")
                return original_readlink(path, *args, **kwargs)

            def read(path, *args, **kwargs):
                if str(path) == str(child / "status"):
                    raise PermissionError("fixture denied")
                return original_open(path, *args, **kwargs)

            with patch.object(surface.os, "readlink", side_effect=readlink), \
                    patch("builtins.open", side_effect=read):
                denied = json.loads(surface._locald_process_diagnostic(
                    42, case, workspace, None, proc_root=proc_root))["process_context"]
            self.assertEqual(denied["child_pid_namespace"], "unsupported:PermissionError")
            self.assertEqual(denied["child_nspid"], "unsupported:PermissionError")
            self.assertEqual(denied["child_starttime_ticks"], 123)
            self.assertEqual(denied["child_cgroup"], "0::/fixture/task/owner")

    def test_process_observation_is_bounded_and_filters_non_case_descriptors(self):
        import json, tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = root / "case"
            workspace = root / "state" / "workspace"
            proc = root / "proc" / "42"
            (case / "project").mkdir(parents=True)
            workspace.mkdir(parents=True)
            (proc / "fd").mkdir(parents=True)
            executable = case / "backend-locald"
            executable.write_bytes(b"owned executable")
            project_file = case / "project" / "input"
            project_file.write_text("case data")
            workspace_secret = workspace / "authority.secret"
            workspace_secret.write_text("secret contents must not be captured")
            outside = root / "outside-secret"
            outside.write_text("must not leak")
            for name, value in (("stat", "42 (locald) S 1"), ("wchan", "futex_wait"),
                                ("syscall", "202 0x1"), ("io", "read_bytes: 3\n")):
                (proc / name).write_text(value)
            (proc / "exe").symlink_to(executable)
            (proc / "fd" / "3").symlink_to(project_file)
            (proc / "fd" / "4").symlink_to(outside)
            (proc / "fd" / "5").symlink_to(executable)
            (proc / "fd" / "6").symlink_to(workspace_secret)
            observed = json.loads(surface._locald_process_diagnostic(42, case, workspace, executable, proc_root=root / "proc"))
            self.assertEqual(observed["stat"], "42 (locald) S 1")
            self.assertEqual(observed["running_executable_identity"]["inode"], executable.stat().st_ino)
            self.assertEqual(observed["fd_paths"], [{"fd": "3", "target": "case:project/input"},
                                                     {"fd": "4", "target": "filtered"},
                                                     {"fd": "5", "target": "executable"},
                                                     {"fd": "6", "target": "workspace:authority.secret"}])
            self.assertNotIn("outside-secret", str(observed))
            self.assertNotIn("secret contents", str(observed))

    def test_fd_enumeration_stops_after_sixteen_entries_and_marks_truncation(self):
        import json, tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = root / "case"
            workspace = root / "workspace"
            proc_fd = root / "proc" / "9" / "fd"
            case.mkdir()
            workspace.mkdir()
            proc_fd.mkdir(parents=True)
            outside = root / "outside"
            outside.write_text("not retained")
            for fd in range(20):
                (proc_fd / str(fd)).symlink_to(outside)
            observed = json.loads(surface._locald_process_diagnostic(9, case, workspace, None, proc_root=root / "proc"))
            self.assertEqual(len(observed["fd_paths"]), 16)
            self.assertTrue(observed["fd_paths_truncated"])
            self.assertTrue(all(row["target"] == "filtered" for row in observed["fd_paths"]))
            self.assertNotIn("not retained", str(observed))

    def test_endpoint_wait_probes_once_at_25s_and_keeps_30s_timeout(self):
        from pathlib import Path
        from types import SimpleNamespace
        from unittest.mock import patch
        now = [0.0]
        process = SimpleNamespace(pid=42, poll=lambda: None)
        probes = []
        def sleep(seconds):
            now[0] += seconds
        def probe(pid, case_root, workspace_root, executable):
            probes.append((pid, case_root, workspace_root, executable, now[0]))
            return '{"captured":true}'
        endpoint = Path("/tmp/surface-diagnostic-test-missing.sock")
        with patch.object(surface, "_locald_process_diagnostic", side_effect=probe):
            with self.assertRaisesRegex(RuntimeError, "did not open its endpoint") as failure:
                surface._wait_for_endpoint(endpoint, process, endpoint.parent / "workspace", lambda: "", monotonic=lambda: now[0], sleep=sleep)
        self.assertEqual(len(probes), 1)
        self.assertGreaterEqual(probes[0][4], 25.0)
        self.assertLess(probes[0][4], 25.1)
        self.assertIn('process_diagnostic={"captured":true}', str(failure.exception))
        self.assertIn("waited=30.", str(failure.exception))
        self.assertEqual(surface.LOCALD_ENDPOINT_START_TIMEOUT_SECONDS, 30)

    def test_missing_proc_files_are_explicit_and_do_not_raise(self):
        import json, tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = root / "case"
            case.mkdir()
            observed = json.loads(surface._locald_process_diagnostic(7, case, case / "workspace", case / "missing", proc_root=root / "proc"))
            self.assertEqual(observed["stat"], "unsupported:FileNotFoundError")
            self.assertEqual(observed["fd_paths"], "unsupported:FileNotFoundError")


class ProjectCopyLifecycleTests(unittest.TestCase):
    def test_readonly_pinned_source_stays_unchanged_and_copied_case_can_be_removed(self):
        import tempfile, shutil, stat
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "pin"
            source.mkdir()
            (source / "src").mkdir()
            (source / "src/lib.py").write_text("class Example: pass\n")
            (source / "src").chmod(0o555)
            source.chmod(0o555)
            copied = root / "case"
            try:
                shutil.copytree(source, copied)
                surface.make_case_directories_writable(copied)
                self.assertTrue(copied.stat().st_mode & stat.S_IWUSR)
                self.assertTrue((copied / "src").stat().st_mode & stat.S_IWUSR)
                self.assertEqual((copied / "src/lib.py").read_text(), "class Example: pass\n")
                shutil.rmtree(copied)
                self.assertEqual(stat.S_IMODE(source.stat().st_mode), 0o555)
                self.assertEqual(stat.S_IMODE((source / "src").stat().st_mode), 0o555)
            finally:
                source.chmod(0o700)
                (source / "src").chmod(0o700)

    def test_directory_links_cannot_change_permissions_outside_owned_case(self):
        import tempfile, stat
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            external = root / "external"
            external.mkdir(mode=0o500)
            case = root / "case"
            case.mkdir()
            (case / "link").symlink_to(external, target_is_directory=True)
            surface.make_case_directories_writable(case)
            self.assertEqual(stat.S_IMODE(external.stat().st_mode), 0o500)
            with self.assertRaisesRegex(RuntimeError, "owned directories"):
                surface.make_case_directories_writable(case / "link")
            external.chmod(0o700)


class OwnerLifecycleTests(unittest.TestCase):
    def exercise(self, startup_failure=False):
        import tempfile
        from unittest.mock import Mock, patch
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = root / "fixed-project-case"
            case.mkdir()
            process = Mock()
            process.poll.return_value = 0 if startup_failure else None
            process.returncode = 7 if startup_failure else 0
            observed = {}
            def launch(command, **kwargs):
                workspace = Path(command[command.index("--workspace") + 1])
                observed.update(workspace=workspace, log=kwargs["stdout"])
                self.assertEqual(workspace.parent.parent, root / ".local")
                self.assertEqual(workspace.parent.stat().st_mode & 0o777, 0o700)
                self.assertEqual(command[command.index("--endpoint") + 1], str(case / "locald.sock"))
                self.assertEqual(command[command.index("--timeout-ms") + 1], "180000")
                self.assertEqual(command[command.index("--idle-timeout-ms") + 1], "5000")
                self.assertNotIn(str(workspace), str(case / "project"))
                if startup_failure:
                    kwargs["stdout"].write("authority startup refused\n")
                else:
                    (case / "locald.sock").touch()
                return process
            with patch.object(surface, "ROOT", root), patch.object(surface.process_custody, "popen", side_effect=launch):
                if startup_failure:
                    with self.assertRaisesRegex(RuntimeError, "did not open its endpoint") as failure:
                        surface.Owner(root / "bin", case)
                    self.assertIn("authority startup refused", str(failure.exception))
                    self.assertIn("exited 7", str(failure.exception))
                else:
                    owner = surface.Owner(root / "bin", case)
                    self.assertTrue(owner.workspace.is_dir())
                    process.poll.return_value = 0
                    owner.stop()
            self.assertFalse(observed["workspace"].parent.exists())
            self.assertTrue(observed["log"].closed)

    def test_owner_state_uses_private_task_volume_without_changing_public_project_paths(self):
        self.exercise()

    def test_failed_startup_releases_log_and_private_state(self):
        self.exercise(startup_failure=True)

    def test_owner_retirement_failure_keeps_original_case_failure(self):
        import tempfile
        from unittest.mock import Mock, patch
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "source").mkdir()
            owner = Mock()
            owner.stop.side_effect = RuntimeError("owner idle retirement failed")
            original = RuntimeError("index refused authored source")
            case = {"name": "failed-index", "project": "source", "languages": ["c"], "index_mode": "wait"}
            with patch.object(surface, "ROOT", root), patch.object(surface, "SCRATCH", root / "scratch"), \
                 patch.object(surface, "Owner", return_value=owner), \
                 patch.object(surface, "run_cli", side_effect=original), \
                 patch.object(surface, "_record_owner_threads"):
                with self.assertRaises(RuntimeError) as failure:
                    surface.run_case(case, root / "bin")
            self.assertIn("index refused authored source", str(failure.exception))
            self.assertIn("owner idle retirement failed", str(failure.exception))
            self.assertIs(failure.exception.__cause__, original)
            owner.stop.assert_called_once()


class AdmissionTests(unittest.TestCase):
    def test_failed_index_is_never_a_golden_answer(self):
        for answer in (
            {"exit": 1, "json": {"answer": "fault", "cause": "unreachable"}},
            {"exit": 0, "json": {"answer": "fault"}},
            {"exit": 0, "json": None},
        ):
            with self.subTest(answer=answer), self.assertRaises(RuntimeError):
                surface.require_cli_success("cli add", answer)

    def test_failed_pending_or_partial_index_receipts_are_refused(self):
        import copy
        p = {"state": {"state": "published", "detail": {"coverage": {"coverage": "complete"}}}}
        answer = {"exit": 0, "json": {"index_operation": {"state": "known", "detail": {
            "state": {"state": "published", "detail": {"view_root": "a" * 64}},
            "source_capture": {"profiles": [p]}}}}}
        self.assertEqual(surface.require_completed_index(answer, 1), "a" * 64)
        for state in ("failed", "active", "partially_published"):
            bad = copy.deepcopy(answer)
            bad["json"]["index_operation"]["detail"]["state"]["state"] = state
            with self.subTest(state=state), self.assertRaises(RuntimeError):
                surface.require_completed_index(bad, 1)
        bad = copy.deepcopy(answer)
        bad["json"]["index_operation"]["detail"]["source_capture"]["profiles"][0]["state"]["state"] = "pending"
        with self.assertRaises(RuntimeError):
            surface.require_completed_index(bad, 1)
        with self.assertRaises(RuntimeError):
            surface.require_completed_index(answer, 7)

    def test_normalizer_preserves_semantic_and_budget_fields(self):
        self.assertEqual(surface.stable({"timeout_ms": 100, "diagnostic": "bad", "duration_ms": 3}),
                         {"timeout_ms": 100, "diagnostic": "bad"})

    def test_successful_structured_answer_is_admitted(self):
        surface.require_cli_success("cli add", {"exit": 0, "json": {"answer": "indexed"}})


class IndexResumptionAdmissionTests(unittest.TestCase):
    def fixture(self):
        return {"exit": 0, "json": {"index_operation": {"state": "known", "detail": {
            "operation_key": "a" * 64, "request_digest": "b" * 64,
            "package": {"kind": "local", "value": "/project"}, "execution_intent": "background",
            "state": {"state": "active", "detail": {}},
        }}}}

    def test_wrong_request_unknown_operation_and_false_wait_success_are_rejected(self):
        import copy
        initial = self.fixture()
        initial["exit"] = 75
        expected = surface.require_index_wait_stop(initial, Path("/project"))
        for exit_code in (0, 1, 64, 70):
            with self.subTest(exit=exit_code), self.assertRaises(RuntimeError):
                surface.require_index_wait_stop(dict(initial, exit=exit_code), Path("/project"))
        for field, value in (
            ("operation_key", "c" * 64), ("operation_key", "0" * 64),
            ("request_digest", "c" * 64), ("request_digest", None),
            ("package", {"kind": "local", "value": "/other"}),
            ("execution_intent", "interactive"),
            ("state", {"state": "failed"}), ("state", {"state": "unresolved"}),
        ):
            changed = copy.deepcopy(initial)
            changed["json"]["index_operation"]["detail"][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(RuntimeError):
                surface.require_index_identity(changed, Path("/project"), expected)
        changed = copy.deepcopy(initial)
        changed["json"]["index_operation"]["state"] = "unknown"
        with self.assertRaises(RuntimeError):
            surface.require_index_identity(changed, Path("/project"), expected)

    def test_replay_must_preserve_complete_terminal_receipt(self):
        import copy
        answer = self.fixture()
        terminal = answer["json"]["index_operation"]["detail"]
        terminal["state"] = {"state": "published", "detail": {"view_root": "c" * 64}}
        terminal["source_capture"] = {"profiles": [
            {"state": {"state": "published", "detail": {"coverage": {"coverage": "complete"}}}}
        ]}
        surface.require_same_index_receipt(answer, Path("/project"), terminal, 1)
        mcp = {"isError": False, "structuredContent": {"surface": {
            "result": "index-operation-status", "data": answer["json"]["index_operation"]}}}
        surface.require_mcp_index_receipt(mcp, Path("/project"), terminal, 1)
        for wrong in (
            dict(mcp, isError=True),
            {"structuredContent": answer["json"]},
            {"structuredContent": {"surface": {"result": "index-operation-started", "data": answer["json"]["index_operation"]}}},
            {"structuredContent": {"surface": {"result": "index-operation-status", "data": {"state": "unknown"}}}},
            {"structuredContent": dict(mcp["structuredContent"], fault={"slug": "refused"})},
        ):
            with self.subTest(mcp=wrong), self.assertRaises(RuntimeError):
                surface.require_mcp_index_receipt(wrong, Path("/project"), terminal, 1)
        for field, value in (
            ("state", {"state": "published", "detail": {"view_root": "d" * 64}}),
            ("source_capture", {"profiles": []}),
            ("state", {"state": "active"}),
        ):
            changed = copy.deepcopy(answer)
            changed["json"]["index_operation"]["detail"][field] = value
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                surface.require_same_index_receipt(changed, Path("/project"), terminal, 1)
        with self.assertRaises(RuntimeError):
            surface.require_index_wait_stop(dict(answer, exit=75), Path("/project"))


class LiveContractAdmissionTests(unittest.TestCase):
    def test_wrong_revision_or_source_cannot_pass_live_health(self):
        import copy
        root, source = "a" * 64, "b" * 64
        summary = {"answer": "status", "detail": "summary", "availability": "available", "readiness": "indexing", "publication": "populated", "rows": 25, "coverage": [], "capabilities": {}, "revision": root, "revision_tag": root[:8]}
        full = dict(summary, detail="full", source=source, source_tag=source[:8])
        page = dict(full)
        surface.require_live_health(summary, full, page)
        with self.assertRaises(RuntimeError):
            surface.require_live_health(summary, full, page, root)
        ready = dict(summary, readiness="ready")
        ready_full = dict(full, readiness="ready")
        surface.require_live_health(ready, ready_full, ready_full, root)
        with self.assertRaises(RuntimeError):
            surface.require_live_health(ready, ready_full, ready_full, "c" * 64)
        for field in ("revision", "source"):
            changed = copy.deepcopy(full)
            changed[field] = "c" * 64
            changed[field + "_tag"] = "c" * 8
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                surface.require_live_health(summary, changed, page)

    def test_ignored_bound_lost_rank_or_false_completeness_is_rejected(self):
        reference = {"answer": "records", "query": "language", "records": [{"id": 1}, {"id": 2}], "more": False}
        good = dict(reference, records=[{"id": 1}], more=True)
        surface.require_bounded_search(reference, good, 1)
        for changed in (reference, dict(good, records=[{"id": 2}]), dict(good, more=False)):
            with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                surface.require_bounded_search(reference, changed, 1)


class ContinuationAdmissionTests(unittest.TestCase):
    def test_slow_page_cannot_pass_after_the_traversal_deadline(self):
        from unittest.mock import Mock, patch
        reference = {"records": [{"id": 1}, {"id": 2}], "more": False}
        fetch = Mock(return_value={"answer": "records"})
        with patch.object(surface.time, "monotonic", side_effect=[0, 0, 31]):
            with self.assertRaisesRegex(RuntimeError, "deadline"):
                surface.walk_search_pages(reference, fetch)
        fetch.assert_called_once_with(None)

    def test_traversal_rejects_loss_repetition_drift_and_false_exhaustion(self):
        import copy
        reference = {"answer": "records", "query": "language", "records": [{"id": 1}, {"id": 2}, {"id": 3}], "more": False, "coverage": [], "readiness": "ready"}
        pages = {
            None: dict(reference, records=[{"id": 1}], more=True, nextCursor="one"),
            "one": dict(reference, records=[{"id": 2}], more=True, nextCursor="two"),
            "two": dict(reference, records=[{"id": 3}]),
        }
        calls = []
        def fetch(cursor):
            calls.append(cursor)
            return pages[cursor]
        self.assertEqual(surface.walk_search_pages(reference, fetch), (3, "one"))
        self.assertEqual(calls, [None, "one", "two"])
        for cursor, field, value in (
            (None, "records", []), (None, "records", reference["records"]),
            (None, "records", [{"id": 2}]), ("one", "records", [{"id": 1}]),
            (None, "more", False), (None, "more", 1),
            (None, "nextCursor", None), (None, "nextCursor", ""),
            ("one", "nextCursor", "one"), ("two", "nextCursor", "extra"),
            ("two", "more", True), ("one", "query", "other"),
            ("one", "coverage", ["lost"]), ("one", "readiness", "indexing"),
        ):
            changed = copy.deepcopy(pages)
            changed[cursor][field] = value
            with self.subTest(cursor=cursor, field=field, value=value), self.assertRaises(RuntimeError):
                surface.walk_search_pages(reference, lambda token: changed[token])


class CursorRefusalAdmissionTests(unittest.TestCase):
    def test_success_transport_failure_and_unrelated_errors_cannot_pass(self):
        import copy
        cli = {"exit": 70, "json": {"answer": "fault", "slug": "protocol", "cause": "unproven", "operand": "cursor", "detail": "malformed continuation token"}}
        mcp = {"isError": True, "structuredContent": {"answer": "fault", "slug": "usage", "cause": "malformed", "operand": "arguments", "detail": "cursor is unknown or malformed"}}
        surface.require_malformed_cursor_refusal(cli, mcp)
        wrong = copy.deepcopy(cli)
        wrong["exit"] = 0
        with self.assertRaises(RuntimeError):
            surface.require_malformed_cursor_refusal(wrong, mcp)
        for field, value in (("slug", "endpoint"), ("operand", "query"), ("detail", "connection failed")):
            wrong = copy.deepcopy(cli)
            wrong["json"][field] = value
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                surface.require_malformed_cursor_refusal(wrong, mcp)
        for wrong in ({"code": -32603, "message": "cursor unavailable"}, {"answer": "records"}, dict(mcp, isError=False), {"isError": True, "structuredContent": dict(mcp["structuredContent"], detail="unrelated error")}, {"isError": True, "structuredContent": dict(mcp["structuredContent"], slug="endpoint")}):
            with self.subTest(reply=wrong), self.assertRaises(RuntimeError):
                surface.require_malformed_cursor_refusal(cli, wrong)


class AuthoredCorpusAdmissionTests(unittest.TestCase):
    def test_plausible_results_from_the_wrong_source_are_not_a_reference(self):
        expected = {"Sum64String": "xxhash_unsafe.go"}
        row = {"identity": {"name": "Sum64String", "path": "xxhash_unsafe.go"}, "language": "go", "state": "ready"}
        surface.require_authored_symbols({"records": [row]}, expected, "go")
        for wrong in ({"records": []}, {"records": [dict(row, file="xxhash_unsafe.go", identity={"name": "Sum64String", "path": "benchmark_test.go"})]},
                      {"records": [dict(row, state="stale")]}, {"records": [dict(row, language="rust")]}, {"records": [dict(row, identity={"name": "unrelated"})]}):
            with self.subTest(answer=wrong), self.assertRaises(RuntimeError):
                surface.require_authored_symbols(wrong, expected, "go")


        expected = {"Nonnull": {"name": "javax.annotation.Nonnull", "path": "javax/annotation/Nonnull.java"}}
        row = {"identity": {"name": "javax.annotation.Nonnull", "path": "javax/annotation/Nonnull.java"}, "language": "java", "state": "ready"}
        surface.require_authored_symbols({"records": [row]}, expected, "java")
        for wrong in (
            dict(row, identity=dict(row["identity"], name="Nonnull")),
            dict(row, identity=dict(row["identity"], path="elsewhere/Nonnull.java")),
            dict(row, language="unknown"), dict(row, state="unverified"),
        ):
            with self.subTest(qualified_identity=wrong), self.assertRaises(RuntimeError):
                surface.require_authored_symbols({"records": [wrong]}, expected, "java")


class PartialSearchReadinessTests(unittest.TestCase):
    def frames(self, *, transport="cli", readiness="preparing", revision=None):
        row = {"identity": {"name": "Token", "path": "src/token.py"},
               "language": "python", "state": "ready"}
        content = {"answer": "records", "query": "Token", "readiness": "partial",
                   "search_scope": "published_names_only", "full_search": readiness,
                   "records": [row], "more": False}
        if revision is not None:
            content["revision"] = revision
        if transport == "cli":
            return {"exit": 0, "json": content}
        return {"isError": False, "structuredContent": content, "content": []}

    def ready(self, *, transport="cli", revision=None):
        row = {"identity": {"name": "Token", "path": "src/token.py"},
               "language": "python", "state": "ready"}
        content = {"answer": "records", "query": "Token", "readiness": "ready",
                   "records": [row], "more": False}
        if revision is not None:
            content["revision"] = revision
        if transport == "cli":
            return {"exit": 0, "json": content}
        return {"isError": False, "structuredContent": content, "content": []}

    def test_partial_authored_result_retries_same_query_until_ready(self):
        from unittest.mock import patch
        now = [0.0]
        timeouts = []
        replies = [self.frames(), self.ready()]
        def retry(timeout):
            timeouts.append(timeout)
            return replies.pop(0)
        def sleep(delay):
            now[0] += delay
        request = ["search", "Token"]
        with patch.object(surface, "_emit_search_readiness_frame") as emit:
            final, frames = surface.resolve_partial_authored_search(
                self.frames(), retry, 1.0, transport="cli", query="Token",
                expected={"Token": "src/token.py"}, language="python", request=request,
                now=lambda: now[0], sleep=sleep)
        self.assertEqual(final, self.ready())
        self.assertEqual(len(frames), 3)
        self.assertEqual(len(timeouts), 2)
        self.assertTrue(all(value > 0 for value in timeouts))
        self.assertEqual(emit.call_count, 3)
        self.assertEqual(emit.call_args_list[0].args[2], 0)
        self.assertEqual(emit.call_args_list[-1].args[2], 2)

    def test_permanent_partial_expires_original_deadline(self):
        now = [0.0]
        attempts = []
        def retry(timeout):
            attempts.append(timeout)
            return self.frames()
        def sleep(delay):
            now[0] += delay
        with self.assertRaisesRegex(RuntimeError, "deadline"):
            surface.resolve_partial_authored_search(
                self.frames(), retry, 0.12, transport="cli", query="Token",
                expected={"Token": "src/token.py"}, language="python", request=["search", "Token"],
                now=lambda: now[0], sleep=sleep)
        self.assertEqual(len(attempts), 1)
        self.assertGreater(attempts[0], 0)

        late_now = [0.0]
        def late_ready(_timeout):
            late_now[0] = 1.0
            return self.ready()
        with self.assertRaisesRegex(RuntimeError, "deadline"):
            surface.resolve_partial_authored_search(
                self.frames(), late_ready, 1.0, transport="cli", query="Token",
                expected={"Token": "src/token.py"}, language="python", request=["search", "Token"],
                now=lambda: late_now[0], sleep=lambda _delay: None)

    def test_refusal_during_retry_is_not_hidden_as_partial_success(self):
        refusal = {"exit": 70, "json": {"answer": "fault", "slug": "endpoint"}}
        attempts = []
        with self.assertRaisesRegex(RuntimeError, "failed"):
            surface.resolve_partial_authored_search(
                self.frames(), lambda timeout: attempts.append(timeout) or refusal,
                surface.time.monotonic() + 10.0,
                transport="cli", query="Token", expected={"Token": "src/token.py"},
                language="python", request=["search", "Token"], sleep=lambda _delay: None)
        self.assertEqual(len(attempts), 1)

    def test_changed_returned_basis_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "changed revision"):
            surface.resolve_partial_authored_search(
                self.frames(revision="a" * 64), lambda _timeout: self.ready(revision="b" * 64),
                surface.time.monotonic() + 10.0,
                transport="cli", query="Token", expected={"Token": "src/token.py"},
                language="python", request=["search", "Token"], sleep=lambda _delay: None)

    def test_mcp_partial_reply_uses_the_same_useful_to_ready_contract(self):
        from unittest.mock import patch
        now = [0.0]
        final, frames = surface.resolve_partial_authored_search(
            self.frames(transport="mcp"), lambda _timeout: self.ready(transport="mcp"),
            surface.time.monotonic() + 1.0,
            transport="mcp", query="Token", expected={"Token": "src/token.py"},
            language="python", request={"tool": "backend.search", "arguments": {"query": "Token"}},
            now=lambda: now[0], sleep=lambda delay: now.__setitem__(0, now[0] + delay))
        self.assertEqual(final, self.ready(transport="mcp"))
        self.assertEqual(len(frames), 2)


class SnapshotContinuationTests(unittest.TestCase):
    def page(self, token="pc4-one", coordinate="first"):
        size = 100 + len(token)
        return {"answer": "records", "query": "Digest", "records": [{"identity": {"coordinate": coordinate}}],
                "more": True, "nextCursor": token,
                "budget": {"bytes": size, "bytesPerToken": 4, "estimatedTokens": (size + 3) // 4}}

    def test_only_consumed_tokens_are_normalized_and_payload_budget_survives(self):
        first, other = self.page(), self.page("pc4-a-longer-proof")
        with self.assertRaises(RuntimeError):
            surface.normalize_continuations(first, set())
        actual = surface.normalize_continuations(first, {first["nextCursor"]})
        self.assertEqual(actual, surface.normalize_continuations(other, {other["nextCursor"]}))
        self.assertEqual(actual["budget"], {"bytesExcludingCursor": 100, "bytesPerToken": 4, "estimatedTokensExcludingCursor": 25})
        changed = self.page(coordinate="different-declaration")
        self.assertNotEqual(actual, surface.normalize_continuations(changed, {changed["nextCursor"]}))
        invalid = self.page()
        invalid["budget"]["estimatedTokens"] += 1
        with self.assertRaises(RuntimeError):
            surface.normalize_continuations(invalid, {invalid["nextCursor"]})
        unchanged = {"budget": {"bytes": 99, "estimatedTokens": 25}, "state": "stale"}
        self.assertEqual(surface.normalize_continuations(unchanged, set()), unchanged)

    def test_executable_only_cursor_is_consumed_before_normalization(self):
        import copy
        first = self.page("mcp2-long-portable-proof")
        token = first.pop("nextCursor")
        first["continuation_call"] = {"name": "backend.search", "arguments": {
            "query": "Digest", "limit": 3, "cursor": token}}
        call = {"tool": "backend.search", "arguments": {"query": "Digest", "limit": 9}}
        self.assertEqual(surface.mcp_continuation_call(call, first), {
            "tool": "backend.search", "arguments": first["continuation_call"]["arguments"]})
        with self.assertRaisesRegex(RuntimeError, "unconsumed"):
            surface.normalize_continuations(first, set())
        actual = surface.normalize_continuations(first, {token})
        self.assertNotIn("nextCursor", actual)
        self.assertEqual(actual["continuation_call"]["arguments"]["cursor"], "mcp2-<live-validated>")
        self.assertEqual(actual["budget"]["bytesExcludingCursor"], 100)
        other = copy.deepcopy(first)
        other["continuation_call"]["arguments"]["cursor"] = "mcp2-other-proof-length"
        size = 100 + len(other["continuation_call"]["arguments"]["cursor"])
        other["budget"].update(bytes=size, estimatedTokens=(size + 3) // 4)
        self.assertEqual(actual, surface.normalize_continuations(other, {other["continuation_call"]["arguments"]["cursor"]}))
        wrong = copy.deepcopy(first)
        wrong["continuation_call"]["name"] = "backend.outline"
        with self.assertRaisesRegex(RuntimeError, "changed tool"):
            surface.mcp_continuation_call(call, wrong)
        wrong = copy.deepcopy(first)
        wrong["nextCursor"] = "mcp2-different-proof"
        with self.assertRaisesRegex(RuntimeError, "contradictory"):
            surface.normalize_continuations(wrong, {token, wrong["nextCursor"]})

    def test_wire_accounting_is_measured_before_normalizing_proof_bytes(self):
        import copy
        import json

        def fixture(token):
            reply = {"structuredContent": self.page(token), "_meta": {"backend/wireBudget": {
                "bytes": 0, "estimatedTokens": 0, "bytesPerToken": 4,
                "limitBytes": 49152, "limitTokens": 12000, "scope": "complete_jsonrpc_line"}}}
            budget = reply["_meta"]["backend/wireBudget"]
            for _ in range(10):
                size = len(json.dumps({"jsonrpc": "2.0", "id": 2, "result": reply}, separators=(",", ":"), ensure_ascii=False).encode()) + 1
                if budget["bytes"] == size:
                    break
                budget.update(bytes=size, estimatedTokens=(size + 3) // 4)
            surface.require_mcp_wire_budget(reply, size)
            return {"mcp": {"calls": [{"call": {"tool": "backend.search", "arguments": {}}, "reply": reply}]}}, size

        first, size = fixture("mcp2-first-proof")
        other, _ = fixture("mcp2-a-different-longer-proof")
        one = surface.normalize_surface_answers(first, {"mcp2-first-proof"})
        two = surface.normalize_surface_answers(other, {"mcp2-a-different-longer-proof"})
        self.assertEqual(one, two)
        bad = copy.deepcopy(first["mcp"]["calls"][0]["reply"])
        bad["_meta"]["backend/wireBudget"]["bytes"] += 1
        with self.assertRaises(RuntimeError):
            surface.require_mcp_wire_budget(bad, size)
        self.assertEqual(one["mcp"]["calls"][0]["reply"]["_meta"]["backend/wireBudget"]["limitBytes"], 49152)

    def test_search_continuation_refuses_fault_repetition_and_scope_drift(self):
        first = self.page()
        following = {"answer": "records", "query": "Digest", "records": [{"identity": {"coordinate": "second"}}], "more": False}
        surface.require_continuation_reply(first, following, "root")
        for wrong in ({"answer": "fault"}, dict(following, records=[]), dict(following, records=first["records"]),
                      dict(following, query="other"), dict(following, nextCursor=first["nextCursor"]),
                      dict(following, more=True)):
            with self.subTest(answer=wrong), self.assertRaises(RuntimeError):
                surface.require_continuation_reply(first, wrong, "root")

    def test_outline_resume_uses_the_returned_page_limit(self):
        call = {"tool": "backend.outline", "arguments": {}}
        first = {"nextCursor": "mcp2-proof", "page": {"limit": 50},
                 "package": {"project": "/project"}, "detail": "summary"}
        self.assertEqual(surface.mcp_continuation_call(call, first),
                         {"tool": "backend.outline", "arguments": {"path": "/project", "limit": 50, "detail": "summary", "cursor": "mcp2-proof"}})
        with self.assertRaises(RuntimeError):
            surface.mcp_continuation_call(call, dict(first, page={}))

    def test_outline_continuation_keeps_the_published_basis(self):
        import copy
        first = {"answer": "outline", "roots": [{"key": "first"}], "declarations": 1, "nextCursor": "mcp2-one",
                 "page": {"source_root": "root", "branch": "branch", "log": "log", "schema": 3, "sequence": 2, "limit": 100, "more": True}}
        following = {"answer": "outline", "roots": [{"key": "second"}], "declarations": 1,
                     "page": dict(first["page"], more=False)}
        surface.require_continuation_reply(first, following, "root")
        for field in ("source_root", "branch", "log", "schema", "sequence", "limit"):
            wrong = copy.deepcopy(following)
            wrong["page"][field] = "changed"
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                surface.require_continuation_reply(first, wrong, "root")
        with self.assertRaises(RuntimeError):
            surface.require_continuation_reply(first, dict(following, roots=first["roots"]), "root")


class InteractiveMcpTests(unittest.TestCase):
    def test_partial_search_is_retried_inside_its_issuing_mcp_session(self):
        import os
        import sys
        import tempfile
        from types import SimpleNamespace

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable = root / "backend-mcp"
            executable.write_text("#!" + sys.executable + "\n" + '''
import json, sys
searches = 0
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        result = {}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'backend.search'}]}
    else:
        searches += 1
        row = {'identity': {'name': 'Token', 'path': 'src/token.py'}, 'language': 'python', 'state': 'ready'}
        content = {'answer': 'records', 'query': 'Token', 'records': [row], 'more': False}
        if searches == 1:
            content.update(readiness='partial', search_scope='published_names_only', full_search='preparing')
        else:
            content.update(readiness='ready')
        result = {'isError': False, 'structuredContent': content, 'content': []}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
''')
            executable.chmod(0o700)
            owner = SimpleNamespace(bin_dir=root, endpoint=root / "unused.sock", env=dict(os.environ))
            expected = {"Token": "src/token.py"}

            def resolve(call, reply, _continuation, readiness_exchange):
                return surface.resolve_partial_authored_search(
                    reply,
                    lambda remaining: readiness_exchange(call, remaining),
                    readiness_exchange.deadline,
                    transport="mcp", query="Token", expected=expected,
                    language="python", request=call,
                )[0]

            answer, _seconds = surface.run_mcp_interactive(
                owner, root, [{"tool": "backend.search", "arguments": {"query": "Token"}}], 3, resolve)
            self.assertEqual(answer["exit"], 0)
            self.assertEqual(len(answer["calls"]), 1)
            self.assertEqual(answer["calls"][0]["reply"]["structuredContent"]["readiness"], "ready")

    def test_dynamic_continuation_uses_the_issuing_process_and_returned_operands(self):
        import os
        import sys
        import tempfile
        from types import SimpleNamespace

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable = root / "backend-mcp"
            executable.write_text("#!" + sys.executable + "\n" + '''
import json, sys
issued = False
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        result = {}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'backend.outline'}]}
    else:
        args = request['params']['arguments']
        if 'cursor' not in args:
            issued = True
            result = {'structuredContent': {'nextCursor': 'mcp2-issued', 'page': {'limit': 50}, 'package': {'project': '/project'}, 'detail': 'summary'}}
        else:
            assert issued and args == {'cursor': 'mcp2-issued', 'limit': 50, 'path': '/project', 'detail': 'summary'}
            result = {'resumed': True}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
''')
            executable.chmod(0o700)
            owner = SimpleNamespace(bin_dir=root, endpoint=root / "unused.sock", env=dict(os.environ))
            observed = []

            def consume(call, reply, exchange, _readiness_exchange):
                observed.append(exchange(surface.mcp_continuation_call(call, reply["structuredContent"])))

            answer, _ = surface.run_mcp_interactive(owner, root,
                [{"tool": "backend.outline", "arguments": {}}], 3, consume)
            self.assertEqual(observed, [{"resumed": True}])
            self.assertEqual(answer["exit"], 0)
            self.assertEqual(len(answer["calls"]), 1)
            self.assertEqual(answer["calls"][0]["reply"]["structuredContent"]["nextCursor"], "mcp2-issued")


class ParallelCaseTests(unittest.TestCase):
    def invoke(self, failure=None, duplicate=False, wait_for_fast_verdict=False):
        import contextlib
        import io
        import json
        import sys
        import tempfile
        import threading
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            snapshots = root / "snapshots"
            snapshots.mkdir()
            names = ["alpha", "alpha" if duplicate else "beta"]
            (root / "cases.json").write_text(json.dumps({"cases": [{"name": name} for name in names]}))
            for name in names:
                (snapshots / (name + ".json")).write_text(json.dumps({"answers": {"case": name}, "performance": {"seconds": {}}}))
            report = root / "report.json"
            rendezvous = threading.Barrier(2)
            fast_verdict = threading.Event()
            reached = []

            def run(case, binaries):
                reached.append(case["name"])
                self.assertEqual(surface.CURRENT_CASE.get(), case["name"])
                rendezvous.wait(timeout=3)  # A serial executor cannot satisfy this.
                if wait_for_fast_verdict and case["name"] == "alpha":
                    if not fast_verdict.wait(timeout=3):
                        raise RuntimeError("completed peer verdict was blocked by this unfinished case")
                if case["name"] == failure:
                    raise RuntimeError("intentional setup failure")
                return {"answers": {"case": case["name"]}, "performance": {"seconds": {"query": 0.01}}}

            stdout, stderr = io.StringIO(), io.StringIO()
            original_log = surface.log
            def log(message):
                if message.startswith("case beta matches"):
                    fast_verdict.set()
                original_log(message)
            argv = ["surface.py", "--bin-dir", str(root), "--report", str(report)]
            with patch.object(surface, "SURFACE", root), patch.object(surface, "SCRATCH", root / "scratch"), patch.object(surface, "run_case", side_effect=run), patch.object(surface, "log", side_effect=log), patch.object(sys, "argv", argv), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                try:
                    result = surface.main()
                except SystemExit as error:
                    result = error.code
            return result, reached, json.loads(report.read_text()) if report.exists() else {}, stdout.getvalue(), stderr.getvalue()

    def test_default_workers_overlap_distinct_cases_and_preserve_both_results(self):
        import json
        result, reached, report, output, error = self.invoke()
        self.assertEqual(result, 0, error + output)
        self.assertEqual(set(reached), {"alpha", "beta"})
        self.assertEqual(set(report), {"alpha", "beta"})
        metrics = [json.loads(line.split(" ", 1)[1]) for line in output.splitlines() if line.startswith("SURFACE-PERFORMANCE ")]
        self.assertEqual({row["case"] for row in metrics}, {"alpha", "beta"})

    def test_completed_peer_verdict_is_reported_before_slow_first_case_finishes(self):
        result, reached, report, output, error = self.invoke(wait_for_fast_verdict=True)
        self.assertEqual(result, 0, error + output)
        self.assertEqual(set(reached), {"alpha", "beta"})
        self.assertEqual(set(report), {"alpha", "beta"})

    def test_failed_case_cannot_be_hidden_by_successful_peer(self):
        result, reached, report, output, error = self.invoke(failure="beta")
        self.assertEqual(result, 1, error + output)
        self.assertEqual(set(reached), {"alpha", "beta"})
        self.assertEqual(set(report), {"alpha"})
        self.assertIn("intentional setup failure", output)
        self.assertIn("failed: beta", output)

    def test_duplicate_scratch_names_fail_before_launch(self):
        result, reached, report, output, error = self.invoke(duplicate=True)
        self.assertEqual(result, 2, error + output)
        self.assertEqual(reached, [])
        self.assertIn("unique scratch", error)


class DiagnosticIsolationTests(unittest.TestCase):
    CASE = {"name": "diagnostic-fixture"}
    PROVENANCE = {"source_sha": "a" * 40, "artifact_manifest_sha256": "b" * 64}

    def saved_context(self):
        return (
            surface.CURRENT_CASE.set("outer-case"),
            surface.RUN_PROVENANCE.set({"source_sha": "outer-provenance"}),
            surface.CASE_DIAGNOSTICS.set({"case": "outer-diagnostics"}),
        )

    def assert_context_restored(self):
        self.assertEqual(surface.CURRENT_CASE.get(), "outer-case")
        self.assertEqual(surface.RUN_PROVENANCE.get(), {"source_sha": "outer-provenance"})
        self.assertEqual(surface.CASE_DIAGNOSTICS.get(), {"case": "outer-diagnostics"})

    def reset_context(self, tokens):
        surface.CASE_DIAGNOSTICS.reset(tokens[2])
        surface.RUN_PROVENANCE.reset(tokens[1])
        surface.CURRENT_CASE.reset(tokens[0])

    def test_diagnostic_setup_failure_is_unavailable_and_product_still_runs(self):
        import contextlib
        import io
        import json
        from unittest.mock import patch
        import perf_context

        tokens = self.saved_context()
        stdout = io.StringIO()
        observed = []

        def product(case, bin_dir):
            observed.append((surface.CURRENT_CASE.get(), dict(surface.CASE_DIAGNOSTICS.get())))
            return {"performance": {"seconds": {}}}

        try:
            with patch.object(perf_context, "system", side_effect=[RuntimeError("secret setup text"), {"sample": "end"}]), \
                    patch.object(surface, "run_case", side_effect=product), \
                    contextlib.redirect_stdout(stdout):
                result = surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertEqual(result[0], {"performance": {"seconds": {}}})
            self.assertIsNone(result[2])
            self.assertEqual(observed[0][0], self.CASE["name"])
            self.assertEqual(observed[0][1]["start"], {"available": False, "error_class": "RuntimeError"})
            self.assertNotIn("end", observed[0][1])
            record = json.loads(next(line.split(" ", 1)[1] for line in stdout.getvalue().splitlines()
                                     if line.startswith("CI-PERF-CONTEXT ")))
            self.assertEqual(record["start"], {"available": False, "error_class": "RuntimeError"})
            self.assertEqual(record["end"], {"sample": "end"})
            self.assertEqual(record["source_sha"], self.PROVENANCE["source_sha"])
            self.assertEqual(record["artifact_manifest_sha256"], self.PROVENANCE["artifact_manifest_sha256"])
            self.assertNotIn("secret setup text", stdout.getvalue())
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)

    def test_end_collection_failure_preserves_product_failure(self):
        import contextlib
        import io
        import json
        from unittest.mock import patch
        import perf_context

        tokens = self.saved_context()
        stdout = io.StringIO()
        product_error = "product setup failed"
        try:
            with patch.object(perf_context, "system", side_effect=[{"sample": "start"}, RuntimeError("secret end text")]), \
                    patch.object(surface, "run_case", side_effect=RuntimeError(product_error)), \
                    contextlib.redirect_stdout(stdout):
                result = surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertEqual(result[0], None)
            self.assertEqual(result[2], product_error)
            record = json.loads(next(line.split(" ", 1)[1] for line in stdout.getvalue().splitlines()
                                     if line.startswith("CI-PERF-CONTEXT ")))
            self.assertEqual(record["end"], {"available": False, "error_class": "RuntimeError"})
            self.assertNotIn("secret end text", stdout.getvalue())
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)

    def test_product_cancellation_survives_end_collection_failure_and_resets_context(self):
        import asyncio
        import contextlib
        import io
        from unittest.mock import patch
        import perf_context

        tokens = self.saved_context()
        stdout = io.StringIO()
        product_cancel = asyncio.CancelledError("product cancelled")
        diagnostic_cancel = asyncio.CancelledError("diagnostic cancelled")

        def cancel_product(case, bin_dir):
            self.assertEqual(surface.CURRENT_CASE.get(), self.CASE["name"])
            self.assertEqual(surface.RUN_PROVENANCE.get(), self.PROVENANCE)
            self.assertEqual(surface.CASE_DIAGNOSTICS.get()["case"], self.CASE["name"])
            raise product_cancel

        try:
            with patch.object(perf_context, "system", side_effect=[{"sample": "start"}, diagnostic_cancel]) as system, \
                    patch.object(perf_context, "encode") as encode, \
                    patch.object(surface, "run_case", side_effect=cancel_product), \
                    contextlib.redirect_stdout(stdout):
                with self.assertRaisesRegex(asyncio.CancelledError, "product cancelled") as raised:
                    surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertIs(raised.exception, product_cancel)
            system.assert_called_once()
            encode.assert_not_called()
            self.assertFalse(any(line.startswith("CI-PERF-CONTEXT ") for line in stdout.getvalue().splitlines()))
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)

    def test_diagnostic_setup_cancellation_propagates_and_resets_context(self):
        import asyncio
        import contextlib
        import io
        from unittest.mock import patch
        import perf_context

        tokens = self.saved_context()
        stdout = io.StringIO()
        cancelled = asyncio.CancelledError("setup cancelled")
        try:
            with patch.object(perf_context, "system", side_effect=cancelled) as system, \
                    patch.object(surface, "run_case") as run_case, \
                    contextlib.redirect_stdout(stdout):
                with self.assertRaisesRegex(asyncio.CancelledError, "setup cancelled") as raised:
                    surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertIs(raised.exception, cancelled)
            system.assert_called_once()
            run_case.assert_not_called()
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)

    def test_diagnostic_end_cancellation_propagates_and_resets_context(self):
        import asyncio
        import contextlib
        import io
        from unittest.mock import patch
        import perf_context

        tokens = self.saved_context()
        stdout = io.StringIO()
        cancelled = asyncio.CancelledError("end cancelled")
        try:
            with patch.object(perf_context, "system", side_effect=[{"sample": "start"}, cancelled]) as system, \
                    patch.object(surface, "run_case", return_value={"performance": {"seconds": {}}}), \
                    patch.object(perf_context, "encode") as encode, \
                    contextlib.redirect_stdout(stdout):
                with self.assertRaisesRegex(asyncio.CancelledError, "end cancelled") as raised:
                    surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertIs(raised.exception, cancelled)
            self.assertEqual(system.call_count, 2)  # setup and end sampling both ran
            encode.assert_not_called()
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)

    def test_diagnostic_encoding_cancellation_propagates_and_resets_context(self):
        import asyncio
        import contextlib
        import io
        from unittest.mock import patch
        import perf_context

        tokens = self.saved_context()
        stdout = io.StringIO()
        cancelled = asyncio.CancelledError("encoding cancelled")
        try:
            with patch.object(perf_context, "system", return_value={"sample": "ok"}) as system, \
                    patch.object(perf_context, "encode", side_effect=cancelled), \
                    patch.object(surface, "run_case", return_value={"performance": {"seconds": {}}}), \
                    contextlib.redirect_stdout(stdout):
                with self.assertRaisesRegex(asyncio.CancelledError, "encoding cancelled") as raised:
                    surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertIs(raised.exception, cancelled)
            self.assertEqual(system.call_count, 2)
            self.assertFalse(any(line.startswith("CI-PERF-CONTEXT ") for line in stdout.getvalue().splitlines()))
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)

    def test_diagnostic_output_cancellation_propagates_and_resets_context(self):
        import asyncio
        import contextlib
        import io
        from unittest.mock import patch
        import perf_context

        cancelled = asyncio.CancelledError("output cancelled")

        class DiagnosticOutputCancellation(io.StringIO):
            def write(self, value):
                if "CI-PERF-CONTEXT " in value:
                    raise cancelled
                return super().write(value)

        tokens = self.saved_context()
        stdout, stderr = DiagnosticOutputCancellation(), io.StringIO()
        try:
            with patch.object(perf_context, "system", return_value={"sample": "ok"}), \
                    patch.object(surface, "run_case", return_value={"performance": {"seconds": {}}}), \
                    contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                with self.assertRaisesRegex(asyncio.CancelledError, "output cancelled") as raised:
                    surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertIs(raised.exception, cancelled)
            self.assertEqual(stderr.getvalue(), "")
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)

    def test_encoder_failure_emits_bounded_error_class_without_changing_success(self):
        import contextlib
        import io
        import json
        from unittest.mock import patch
        import perf_context

        tokens = self.saved_context()
        stdout = io.StringIO()
        try:
            with patch.object(perf_context, "system", return_value={"sample": "ok"}), \
                    patch.object(perf_context, "encode", side_effect=ValueError("credential-like secret")), \
                    patch.object(surface, "run_case", return_value={"performance": {"seconds": {}}}), \
                    contextlib.redirect_stdout(stdout):
                result = surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertIsNone(result[2])
            record = json.loads(next(line.split(" ", 1)[1] for line in stdout.getvalue().splitlines()
                                     if line.startswith("CI-PERF-CONTEXT ")))
            self.assertEqual(record["encoding"], {"available": False, "error_class": "ValueError"})
            self.assertEqual(record["source_sha"], self.PROVENANCE["source_sha"])
            self.assertEqual(record["artifact_manifest_sha256"], self.PROVENANCE["artifact_manifest_sha256"])
            self.assertNotIn("credential-like secret", stdout.getvalue())
            self.assertLessEqual(len(json.dumps(record).encode("utf-8")), 65536)
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)

    def test_output_failure_falls_back_to_stderr_without_changing_success(self):
        import contextlib
        import io
        import json
        from unittest.mock import patch
        import perf_context

        class DiagnosticOutputFailure(io.StringIO):
            def write(self, value):
                if "CI-PERF-CONTEXT " in value:
                    raise OSError("secret output destination")
                return super().write(value)

        tokens = self.saved_context()
        stdout, stderr = DiagnosticOutputFailure(), io.StringIO()
        try:
            with patch.object(perf_context, "system", return_value={"sample": "ok"}), \
                    patch.object(surface, "run_case", return_value={"performance": {"seconds": {}}}), \
                    contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                result = surface._invoke_case(self.CASE, Path("/fixture/bin"), self.PROVENANCE)
            self.assertIsNone(result[2])
            record = json.loads(next(line.split(" ", 1)[1] for line in stderr.getvalue().splitlines()
                                     if line.startswith("CI-PERF-CONTEXT ")))
            self.assertEqual(record["output"], {"available": False, "error_class": "OSError"})
            self.assertEqual(record["source_sha"], self.PROVENANCE["source_sha"])
            self.assertEqual(record["artifact_manifest_sha256"], self.PROVENANCE["artifact_manifest_sha256"])
            self.assertNotIn("secret output destination", stderr.getvalue())
            self.assert_context_restored()
        finally:
            self.reset_context(tokens)


class CIEvidenceTests(unittest.TestCase):
    def exercise(self, damage=False, head=None, dirty=False, jobs=1):
        import contextlib
        import hashlib
        import io
        import json
        import sys
        import tempfile
        from unittest.mock import patch
        import t1_artifacts

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "artifact"
            (artifact / "bin").mkdir(parents=True)
            payloads = [*("bin/" + name for name in t1_artifacts.BINARIES), "smoke.py", "runtime.nar"]
            for name in payloads:
                (artifact / name).write_bytes(b"fixture")
            sha = "a" * 40
            store = "/nix/store/" + "b" * 32 + "-runtime"
            manifest = {"schema": 1, "source_sha": sha, "target": t1_artifacts.TARGET,
                        "toolchain": "fixture", "runtime_roots": [store],
                        "python": store + "/bin/python3", "runtime_env": {"PATH": store + "/bin"},
                        "files": [{"path": name, "bytes": 7, "sha256": hashlib.sha256(b"fixture").hexdigest()}
                                  for name in payloads]}
            (artifact / "manifest.json").write_text(json.dumps(manifest))
            if damage:
                (artifact / "bin/backend-cli").write_bytes(b"tampered")
            corpus = root / "surface"
            (corpus / "snapshots").mkdir(parents=True)
            (corpus / "cases.json").write_text(json.dumps({"cases": [{"name": name} for name in ["alpha", "beta", "gamma"]]}))
            for name in ["alpha", "beta", "gamma"]:
                (corpus / "snapshots" / (name + ".json")).write_text(json.dumps({
                    "answers": {"value": name}, "performance": {"seconds": {"cli add": 10}}}))
            evidence, legacy = root / "evidence.json", root / "legacy.json"
            ran = []

            def run(case, binaries):
                ran.append(case["name"])
                if case["name"] == "gamma":
                    raise RuntimeError("native setup refused")
                return {"answers": {"value": "changed" if case["name"] == "beta" else case["name"]},
                        "performance": {"seconds": {"cli add": 11}, "cpu_seconds": 1.2, "peak_rss_kib": 1024}}

            args = ["surface.py", "--bin-dir", str(artifact / "bin"), "--ci-report", str(evidence),
                    "--source-sha", sha, "--report", str(legacy), "--jobs", str(jobs)]
            stdout = io.StringIO()
            with patch.object(surface, "SURFACE", corpus), patch.object(surface, "SCRATCH", root / "scratch"), patch.object(surface, "run_case", side_effect=run), patch.object(surface.subprocess, "check_output", side_effect=[head or sha, " M dirty.py" if dirty else ""]), patch.object(sys, "argv", args), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(io.StringIO()):
                try:
                    code = surface.main()
                except SystemExit as error:
                    code = error.code
            self.last_stdout = stdout.getvalue()
            return code, ran, (json.loads(evidence.read_text()) if evidence.exists() else None), (json.loads(legacy.read_text()) if legacy.exists() else None)

    def test_source_bound_report_preserves_failed_and_successful_peers(self):
        code, ran, report, legacy = self.exercise()
        self.assertEqual(code, 1)
        self.assertEqual(ran, ["alpha", "beta", "gamma"])
        self.assertEqual(report["schema"], 1)
        self.assertEqual(report["source_sha"], "a" * 40)
        self.assertRegex(report["artifact_manifest_sha256"], r"^[a-f0-9]{64}$")
        self.assertEqual(report["cases"]["alpha"]["state"], "passed")
        self.assertEqual(report["cases"]["alpha"]["index_delta_percent"], 10)
        self.assertEqual(report["cases"]["beta"]["state"], "failed")
        self.assertIn("this change", report["cases"]["beta"]["snapshot_diff"])
        self.assertIn("native setup refused", report["cases"]["gamma"]["detail"])
        self.assertEqual(set(legacy), {"alpha", "beta"})
        self.assertEqual(legacy["alpha"]["seconds"], {"cli add": 11})

    def test_parallel_context_is_bound_and_survives_a_failed_case(self):
        import json
        import os
        from unittest.mock import patch
        with patch.dict(os.environ, {"FORGEJO_TOKEN": "private-diagnostic-token"}):
            code, ran, report, legacy = self.exercise(jobs=2)
        self.assertEqual(code, 1)
        records = [json.loads(line[len("CI-PERF-CONTEXT "):]) for line in self.last_stdout.splitlines()
                   if line.startswith("CI-PERF-CONTEXT ")]
        self.assertEqual({r["case"] for r in records}, {"alpha", "beta", "gamma"})
        for record in records:
            self.assertEqual(record["source_sha"], report["source_sha"])
            self.assertEqual(record["artifact_manifest_sha256"], report["artifact_manifest_sha256"])
            self.assertIn("start", record)
            self.assertIn("end", record)
            self.assertLess(len(json.dumps(record)), 65536)
        self.assertNotIn("private-diagnostic-token", self.last_stdout)
        self.assertIsNone(surface.CASE_DIAGNOSTICS.get())
        self.assertIsNone(surface.RUN_PROVENANCE.get())
        self.assertEqual(report["cases"]["beta"]["state"], "failed")
        self.assertIn("native setup refused", report["cases"]["gamma"]["detail"])

    def test_changed_artifact_or_checkout_cannot_emit_source_bound_evidence(self):
        for options in ({"damage": True}, {"head": "c" * 40}, {"dirty": True}):
            with self.subTest(options=options):
                code, ran, report, legacy = self.exercise(**options)
                self.assertEqual(code, 2)
                self.assertEqual(ran, [])
                self.assertIsNone(report)
                self.assertIsNone(legacy)

    def test_large_unicode_diff_is_bounded_by_bytes_and_stays_readable(self):
        value = surface.bounded_text("α" * 10000, 8192)
        self.assertLessEqual(len(value.encode("utf-8")), 8192)
        self.assertIn("full detail retained", value)


class SourceIdentityTimeoutTests(unittest.TestCase):
    def test_actual_checkout_checks_all_tracked_changes_and_ignores_untracked(self):
        import subprocess
        import tempfile
        import source_identity

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def git(*args):
                return subprocess.run(["git", "-C", str(root), *args], check=True,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      text=True).stdout

            git("init", "-q")
            git("config", "user.name", "Source identity fixture")
            git("config", "user.email", "source-identity-fixture@localhost")
            tracked = root / "tracked.txt"
            tracked.write_text("clean\n")
            git("add", "tracked.txt")
            git("commit", "-qm", "fixture")
            source_sha = git("rev-parse", "HEAD").strip()

            (root / "untracked.txt").write_text("not part of the tracked gate\n")
            source_identity.verify_checkout(root, source_sha)

            tracked.write_text("unstaged tracked change\n")
            with self.assertRaisesRegex(ValueError, "clean tracked"):
                source_identity.verify_checkout(root, source_sha)

            git("checkout", "--", "tracked.txt")
            tracked.write_text("staged tracked change\n")
            git("add", "tracked.txt")
            with self.assertRaisesRegex(ValueError, "clean tracked"):
                source_identity.verify_checkout(root, source_sha)

    def test_checkout_uses_full_sha_and_tracked_only_cleanliness_check(self):
        from unittest.mock import call, patch
        import source_identity

        root = Path("/checkout")
        source_sha = "a" * 40
        with patch.object(source_identity.subprocess, "check_output",
                          side_effect=[source_sha + "\n", ""]) as check_output:
            source_identity.verify_checkout(root, source_sha)

        self.assertEqual(check_output.call_args_list, [
            call(["git", "-C", str(root), "rev-parse", "HEAD"], text=True, timeout=10),
            call(["git", "-C", str(root), "status", "--porcelain",
                  "--untracked-files=no"], text=True,
                 timeout=source_identity.TRACKED_STATUS_TIMEOUT_SECONDS),
        ])

    def test_tracked_status_timeout_propagates_without_retry(self):
        from unittest.mock import patch
        import source_identity

        source_sha = "b" * 40
        timeout = source_identity.subprocess.TimeoutExpired(
            ["git", "status"], source_identity.TRACKED_STATUS_TIMEOUT_SECONDS)
        with patch.object(source_identity.subprocess, "check_output",
                          side_effect=[source_sha, timeout]) as check_output:
            with self.assertRaises(source_identity.subprocess.TimeoutExpired):
                source_identity.verify_checkout(Path("/checkout"), source_sha)

        self.assertEqual(check_output.call_count, 2)
        self.assertEqual(check_output.call_args.kwargs["timeout"],
                         source_identity.TRACKED_STATUS_TIMEOUT_SECONDS)


class IncrementalCaseVerdictTests(unittest.TestCase):
    def test_completed_case_is_visible_before_later_case_aborts_without_final_packet(self):
        import argparse
        import contextlib
        import io
        import json
        import tempfile
        import time
        from unittest.mock import patch

        stdout = io.StringIO()
        class InterruptedExecutor:
            def __init__(self, **kwargs): pass
            def __enter__(self): return self
            def __exit__(self, *args): return False
            def shutdown(self, **kwargs): pass
            def submit(self, function, case, *args):
                future = Future()
                if case["name"] == "first":
                    future.set_result(({"performance": {"seconds": {}}}, 0.1, None))
                return future

        wait_calls = 0
        def interrupted_wait(futures, **kwargs):
            nonlocal wait_calls
            wait_calls += 1
            if wait_calls == 1:
                first = next(future for future in futures if future.done())
                return {first}, set(futures) - {first}
            records = [json.loads(line.removeprefix("SURFACE-CASE-EVIDENCE "))
                       for line in stdout.getvalue().splitlines()
                       if line.startswith("SURFACE-CASE-EVIDENCE ")]
            self_test.assertEqual(records[0]["case"], "first")
            self_test.assertEqual(records[0]["verdict"]["state"], "passed")
            raise RuntimeError("later case interrupted")

        self_test = self
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "cases.json").write_text(json.dumps({"cases": [
                {"name": "first", "kind": "cli-contract"},
                {"name": "later", "kind": "cli-contract"},
            ]}))
            packet = root / "evidence.json"
            args = argparse.Namespace(case=None, jobs=1, bin_dir=root, ci_report=packet,
                                      source_sha="a" * 40, artifact_digest="b" * 64,
                                      report=None, update=False, print_missing=False)
            with patch.object(surface, "SURFACE", root), \
                    patch.object(surface, "ThreadPoolExecutor", InterruptedExecutor), \
                    patch.object(surface, "wait", interrupted_wait), \
                    contextlib.redirect_stdout(stdout):
                with self.assertRaisesRegex(RuntimeError, "later case interrupted"):
                    surface.compare_cases(args, argparse.ArgumentParser())
            self.assertFalse(packet.exists(), "partial verdicts cannot stand in for full acceptance")


class SigtermEvidenceTests(unittest.TestCase):
    def test_started_case_error_after_signal_is_interrupted_not_product_failure(self):
        import argparse
        import contextlib
        import io
        import json
        import tempfile
        import time
        from unittest.mock import patch

        class StopAfterEvidence(BaseException):
            pass

        class ImmediateExecutor:
            def __init__(self, **kwargs): pass
            def submit(self, function, case, *args):
                future = Future()
                try:
                    future.set_result(function(case, *args))
                except BaseException as error:
                    future.set_exception(error)
                return future
            def shutdown(self, **kwargs): pass

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "cases.json").write_text(json.dumps({"cases": [{"name": "late"}]}))
            packet = root / "evidence.json"
            args = argparse.Namespace(case=None, jobs=1, bin_dir=root, ci_report=packet,
                                      source_sha="a" * 40, artifact_digest="b" * 64,
                                      report=None, update=False, print_missing=False)

            def late_error(*_args):
                surface.CI_SIGTERM_AT = time.monotonic()
                surface.CI_SIGTERM.set()
                return None, 0.0, "case raised after the signal"

            with patch.object(surface, "SURFACE", root), \
                    patch.object(surface, "ThreadPoolExecutor", ImmediateExecutor), \
                    patch.object(surface, "_invoke_case", side_effect=late_error), \
                    patch.object(surface.os, "_exit", side_effect=StopAfterEvidence), \
                    patch.object(surface.process_custody, "stop_before_exit", return_value={"roots_reaped": True}), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(StopAfterEvidence):
                    surface.compare_cases(args, argparse.ArgumentParser())
            verdict = json.loads(packet.read_text())["cases"]["late"]
            self.assertEqual(verdict["state"], "failed")
            self.assertEqual(verdict["failure_kind"], "sigterm-interrupted")
            self.assertIn("after the CI runner received SIGTERM", verdict["detail"])

    def test_sigterm_emits_completed_and_started_vs_queued_verdicts_before_nonzero_exit(self):
        import json
        import os
        import selectors
        import signal
        import subprocess
        import sys
        import tempfile
        import time

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = r'''\
import json, os, sys, time
from pathlib import Path
sys.path.insert(0, os.environ["CI_DIR"])
import surface
import cpu_placement
from contextlib import contextmanager

root = Path(os.environ["CASE_ROOT"])
corpus = root / "surface"
(corpus / "snapshots").mkdir(parents=True)
cases = ["completed", "started", "queued"]
(corpus / "cases.json").write_text(json.dumps({"cases": [{"name": name} for name in cases]}))
for name in cases:
    (corpus / "snapshots" / (name + ".json")).write_text(json.dumps({
        "answers": {"value": name}, "performance": {"seconds": {"cli add": 1}}
    }))
surface.SURFACE = corpus
surface.SCRATCH = root / "scratch"
surface.evidence_artifact = lambda _bin_dir, _sha: "b" * 64

def run_case(case, _bin_dir):
    name = case["name"]
    if name == "started":
        (root / "started").write_text("yes")
        while True:
            time.sleep(1)
    return {"answers": {"value": name},
            "performance": {"seconds": {"cli add": 1}, "cpu_seconds": 0.1}}

surface.run_case = run_case
@contextmanager
def placement():
    yield {"fixture": True}
cpu_placement.measurement_placement = placement
sys.argv = ["surface.py", "--bin-dir", str(root / "bin"), "--ci-report", str(root / "evidence.json"),
            "--source-sha", "a" * 40, "--report", str(root / "performance.json"), "--jobs", "1"]
sys.exit(surface.main())
'''
            env = dict(os.environ, CASE_ROOT=str(root), CI_DIR=str(Path(__file__).parent))
            child = subprocess.Popen(
                [sys.executable, "-c", script], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                env=env,
            )
            captured = bytearray()
            deadline = time.monotonic() + 5
            selector = selectors.DefaultSelector()
            selector.register(child.stdout, selectors.EVENT_READ)
            try:
                while time.monotonic() < deadline:
                    for _key, _mask in selector.select(timeout=0.02):
                        captured.extend(os.read(child.stdout.fileno(), 4096))
                    output = captured.decode("utf-8", errors="replace")
                    if (root / "started").exists() and any(
                        line.startswith("SURFACE-CASE-EVIDENCE ") and '"case":"completed"' in line
                        for line in output.splitlines()
                    ):
                        break
                    if child.poll() is not None:
                        stdout, stderr = child.communicate(timeout=1)
                        self.fail("evidence runner exited before the test could signal its owned child: " +
                                  (bytes(captured) + stdout).decode("utf-8", errors="replace") +
                                  stderr.decode("utf-8", errors="replace"))
                else:
                    self.fail("owned evidence runner did not start and complete the expected cases")
                os.kill(child.pid, signal.SIGTERM)
                stdout_tail, stderr = child.communicate(timeout=3)
                complete_output = (bytes(captured) + stdout_tail).decode("utf-8", errors="replace")
                self.assertNotEqual(child.returncode, 0, stderr.decode("utf-8", errors="replace") + complete_output)
                output_lines = complete_output.splitlines()
                self.assertTrue(any(line.startswith("SURFACE-CASE-EVIDENCE ") and '"case":"completed"' in line
                                    for line in output_lines))
                final_lines = [line.removeprefix("SURFACE-CI-EVIDENCE ") for line in output_lines
                               if line.startswith("SURFACE-CI-EVIDENCE ")]
                self.assertEqual(len(final_lines), 1)
                final_packet = json.loads(final_lines[0])
                self.assertEqual(final_packet["packet_kind"], "harness-interruption-summary")
                self.assertIn("not reread or adapter-reverified", final_packet["verification"])
                self.assertEqual(final_packet["source_sha"], "a" * 40)
                self.assertEqual(final_packet["artifact_manifest_sha256"], "b" * 64)
                packet = json.loads((root / "evidence.json").read_text())
                self.assertEqual(packet["source_sha"], "a" * 40)
                self.assertEqual(packet["artifact_manifest_sha256"], "b" * 64)
                self.assertEqual(packet["selection"], "All catalog cases; no product-path narrowing.")
                cases = packet["cases"]
                self.assertEqual(cases["completed"]["state"], "passed")
                self.assertEqual(cases["started"]["state"], "failed")
                self.assertEqual(cases["started"]["failure_kind"], "sigterm-interrupted")
                self.assertIn("received SIGTERM", cases["started"]["detail"])
                self.assertEqual(cases["queued"]["state"], "failed")
                self.assertEqual(cases["queued"]["failure_kind"], "infrastructure-not-run")
                self.assertIn("not run", cases["queued"]["detail"])
                self.assertIn("no product verdict", cases["queued"]["detail"])
                self.assertEqual(set(json.loads((root / "performance.json").read_text())), {"completed"})
            finally:
                selector.close()
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=3)
                if child.stdout:
                    child.stdout.close()
                if child.stderr:
                    child.stderr.close()




class CommunicateDeadlineTests(unittest.TestCase):
    def test_input_uses_remaining_deadline_and_followups_stay_clamped(self):
        c = surface.process_custody
        self.assertAlmostEqual(c._communicate_timeout(10.0, "payload", 9.95), 0.05)
        self.assertAlmostEqual(c._communicate_timeout(10.0, None, 9.95), 0.05)
        self.assertEqual(c._communicate_timeout(10.0, None, 9.0), 0.1)
        self.assertAlmostEqual(c._communicate_timeout(10.0, "", 9.95), 0.05)
        self.assertEqual(c._communicate_timeout(None, "payload", 0.0), 0.1)


class PlatformPopenCompatibilityTests(unittest.TestCase):
    def test_non_linux_popen_run_and_forced_owner_cleanup_preserve_original_paths(self):
        """Real ordinary children exercise the Darwin-selected API path."""
        import os, select, signal, subprocess, sys, tempfile, time
        from unittest.mock import patch
        c=surface.process_custody
        children=[]
        sentinel=None
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            with patch.multiple(c,_active=False,_fallback_enabled=False,_fallback_seen=False,
                                _fallback_children=set(),_stopping=False), \
                    patch.object(c.sys,'platform','darwin'), \
                    patch.object(c,'_prepare',side_effect=AssertionError('Linux custody used on Darwin')):
                c.activate()
                self.assertFalse(c._active)
                try:
                    sentinel=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'],start_new_session=True)
                    def spawn():
                        child=c.popen([sys.executable,'-u','-c',
                            'import time;print("ready",flush=True);time.sleep(60)'],
                            stdout=subprocess.PIPE,start_new_session=True)
                        children.append(child)
                        self.assertIs(type(child),subprocess.Popen)
                        self.assertFalse(hasattr(child,'retire'))
                        self.assertTrue(select.select([child.stdout],[],[],5)[0],'ordinary child not ready')
                        self.assertEqual(child.stdout.readline(),b'ready\n')
                        return child
                    child=spawn()
                    c.stop(child)
                    self.assertEqual(child.returncode,-signal.SIGTERM)
                    child.stdout.close()
                    result=c.run([sys.executable,'-c','print("ordinary-run")'],
                                 capture_output=True,text=True,timeout=5,check=True)
                    self.assertEqual(result.stdout,'ordinary-run\n')
                    self.assertEqual(result.returncode,0)

                    child=spawn()
                    original_wait=child.wait
                    first=True
                    def idle_timeout(timeout=None):
                        nonlocal first
                        if first:
                            first=False
                            self.assertEqual(timeout,20)
                            raise subprocess.TimeoutExpired(child.args,timeout)
                        return original_wait(timeout=timeout)
                    owner=surface.Owner.__new__(surface.Owner)
                    owner.process=child
                    owner.state_directory=tempfile.TemporaryDirectory(dir=root)
                    state=Path(owner.state_directory.name)
                    owner.log=(root/'owner.log').open('w')
                    with patch.object(child,'wait',side_effect=idle_timeout):
                        with self.assertRaisesRegex(RuntimeError,'failed to retire through its idle timeout'):
                            owner.stop()
                    self.assertEqual(child.returncode,-signal.SIGKILL)
                    self.assertTrue(owner.log.closed)
                    self.assertFalse(state.exists())
                    child.stdout.close()
                    # Root cleanup cannot masquerade as Linux descendant proof.
                    with self.assertRaisesRegex(RuntimeError,'descendant custody remains unproved'):
                        c.stop_before_exit()
                    self.assertIsNone(sentinel.poll())
                    with patch.object(c.subprocess,'Popen',side_effect=AssertionError('spawn after interruption')):
                        with self.assertRaisesRegex(RuntimeError,'new child admission is closed'):
                            c.popen([sys.executable,'-c','pass'])
                finally:
                    for child in children:
                        if child.poll() is None:child.kill()
                        child.wait(timeout=5)
                        if child.stdout and not child.stdout.closed:child.stdout.close()
                    if sentinel is not None:
                        if sentinel.poll() is None:sentinel.terminate()
                        sentinel.wait(timeout=5)


    def test_non_linux_admitted_constructor_is_registered_before_stop_snapshot(self):
        """Gate a real constructor while interruption competes for admission."""
        import signal, subprocess, sys, threading
        from unittest.mock import patch
        c=surface.process_custody
        original_popen=subprocess.Popen
        constructor_entered=threading.Event()
        release_constructor=threading.Event()
        stop_entered=threading.Event()
        stop_done=threading.Event()
        children=[]
        errors=[]
        refused=[]
        constructors=[]
        def gated_constructor(*args,**kwargs):
            constructors.append(True)
            constructor_entered.set()
            if not release_constructor.wait(5):
                raise RuntimeError('constructor regression gate expired')
            child=original_popen(*args,**kwargs)
            children.append(child)
            return child
        def spawn():
            try:
                c.popen([sys.executable,'-c','import time;time.sleep(60)'],
                        start_new_session=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            except BaseException as error:errors.append(error)
        def interrupt():
            stop_entered.set()
            try:c.stop_before_exit()
            except RuntimeError as error:refused.append(str(error))
            except BaseException as error:errors.append(error)
            finally:stop_done.set()
        with patch.multiple(c,_active=False,_fallback_enabled=False,_fallback_seen=False,
                            _fallback_children=set(),_stopping=False), \
                patch.object(c.sys,'platform','darwin'), \
                patch.object(c,'_prepare',side_effect=AssertionError('Linux custody used on Darwin')), \
                patch.object(c.subprocess,'Popen',side_effect=gated_constructor):
            c.activate()
            spawning=threading.Thread(target=spawn)
            stopping=threading.Thread(target=interrupt)
            try:
                spawning.start()
                self.assertTrue(constructor_entered.wait(5),'constructor was not reached')
                stopping.start()
                self.assertTrue(stop_entered.wait(5),'interruption was not attempted')
                self.assertFalse(stop_done.wait(.1),'stop snapshot escaped an admitted constructor')
                self.assertFalse(c._stopping)
                release_constructor.set()
                spawning.join(10)
                stopping.join(15)
                self.assertFalse(spawning.is_alive())
                self.assertFalse(stopping.is_alive())
                self.assertEqual(errors,[])
                self.assertEqual(len(children),1)
                self.assertIs(type(children[0]),original_popen)
                self.assertEqual(children[0].returncode,-signal.SIGTERM)
                self.assertEqual(children[0].wait(timeout=0),-signal.SIGTERM)
                self.assertEqual(len(refused),1)
                self.assertIn('descendant custody remains unproved',refused[0])
                self.assertIn(children[0],c._fallback_children)
                # The opposing order refuses before entering the constructor.
                with self.assertRaisesRegex(RuntimeError,'new child admission is closed'):
                    c.popen([sys.executable,'-c','pass'])
                self.assertEqual(len(constructors),1)
            finally:
                release_constructor.set()
                if spawning.ident is not None:spawning.join(10)
                if stopping.ident is not None:stopping.join(15)
                for child in children:
                    if child.poll() is None:child.kill()
                    child.wait(timeout=5)


    def test_non_linux_run_preserves_communication_and_stops_registered_worker(self):
        """Real ordinary run workers stay registered through interruption."""
        import signal, subprocess, sys, tempfile, threading, time
        from unittest.mock import patch
        c=surface.process_custody
        children=[]
        worker_errors=[]
        worker_results=[]
        stop_errors=[]
        stop_entered=threading.Event()
        stop_done=threading.Event()
        worker=None
        stopping=None
        sentinel=None
        with tempfile.TemporaryDirectory() as directory:
            ready=Path(directory)/'ready'
            halt=Path(directory)/'halt'
            with patch.multiple(c,_active=False,_fallback_enabled=False,_fallback_seen=False,
                                _fallback_children=set(),_stopping=False), \
                    patch.object(c.sys,'platform','darwin'), \
                    patch.object(c,'_prepare',side_effect=AssertionError('Linux custody used on Darwin')):
                try:
                    sentinel=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'],
                        stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)
                    c.activate()
                    payload='héllo'*32768
                    answer=c.run([sys.executable,'-c',
                        'import sys,time;time.sleep(.2);data=sys.stdin.read();sys.stdout.write(data);sys.stderr.write("err")'],
                        input=payload,capture_output=True,text=True,encoding='utf-8',timeout=5,check=True)
                    self.assertEqual((answer.returncode,answer.stdout,answer.stderr),(0,payload,'err'))
                    with self.assertRaises(subprocess.CalledProcessError) as failed:
                        c.run([sys.executable,'-c',
                            'import sys;print("out");print("err",file=sys.stderr);sys.exit(7)'],
                            capture_output=True,text=True,timeout=5,check=True)
                    self.assertEqual((failed.exception.returncode,failed.exception.output,failed.exception.stderr),
                                     (7,'out\n','err\n'))
                    with self.assertRaises(subprocess.TimeoutExpired) as timed:
                        c.run([sys.executable,'-u','-c',
                            'import sys,time;print("partial");print("error",file=sys.stderr);time.sleep(60)'],
                            capture_output=True,text=True,timeout=1)
                    self.assertEqual(timed.exception.timeout,1)
                    # Like CPython POSIX run, partial TimeoutExpired streams are bytes even in text mode.
                    self.assertEqual((timed.exception.output,timed.exception.stderr),(b'partial\n',b'error\n'))
                    before=len(c._fallback_children)
                    with self.assertRaises(ValueError):
                        c.run([sys.executable,'-c','pass'],input=b'x',stdin=subprocess.PIPE)
                    with self.assertRaises(ValueError):
                        c.run([sys.executable,'-c','pass'],capture_output=True,stdout=subprocess.PIPE)
                    self.assertEqual(len(c._fallback_children),before)
                    with self.assertRaises(TypeError):
                        c.run([sys.executable,'-c','import time;time.sleep(60)'],timeout='invalid')
                    self.assertTrue(all(child.returncode is not None for child in c._fallback_children))
                    code=('import pathlib,sys,time;data=sys.stdin.read();print(data,flush=True);'
                          'print("worker-error",file=sys.stderr,flush=True);'
                          'pathlib.Path(sys.argv[1]).write_text("ready");'
                          'end=time.monotonic()+60;'
                          'exec("while time.monotonic()<end and not pathlib.Path(sys.argv[2]).exists(): time.sleep(.01)")')
                    def run_worker():
                        try:worker_results.append(c.run([sys.executable,'-u','-c',code,str(ready),str(halt)],
                            input='once',capture_output=True,text=True))
                        except BaseException as error:worker_errors.append(error)
                    worker=threading.Thread(target=run_worker)
                    worker.start()
                    deadline=time.monotonic()+5
                    while not ready.exists() and time.monotonic()<deadline and worker.is_alive():time.sleep(.01)
                    self.assertTrue(ready.exists(),'real run worker never reached ready marker')
                    with c._lock:
                        children=list(c._fallback_children)
                    active=[child for child in children if child.poll() is None]
                    self.assertEqual(len(active),1)
                    child=active[0]
                    self.assertIs(type(child),subprocess.Popen)
                    def interrupt():
                        stop_entered.set()
                        try:c.stop_before_exit()
                        except BaseException as error:stop_errors.append(error)
                        finally:stop_done.set()
                    # Actual opposing CPython lock ownership must be retried,
                    # rather than reported as a successfully drained root.
                    with child._waitpid_lock:
                        stopping=threading.Thread(target=interrupt)
                        stopping.start()
                        self.assertTrue(stop_entered.wait(5))
                        self.assertFalse(stop_done.wait(.1),'stop ignored the occupied waitpid lock')
                    stopping.join(15)
                    worker.join(10)
                    self.assertFalse(stopping.is_alive())
                    self.assertFalse(worker.is_alive())
                    self.assertEqual(worker_errors,[])
                    self.assertEqual(len(worker_results),1)
                    self.assertEqual((worker_results[0].returncode,worker_results[0].stdout,worker_results[0].stderr),
                                     (-signal.SIGTERM,'once\n','worker-error\n'))
                    self.assertEqual(child.wait(timeout=0),-signal.SIGTERM)
                    self.assertEqual(len(stop_errors),1)
                    self.assertIsInstance(stop_errors[0],RuntimeError)
                    self.assertIn('descendant custody remains unproved',str(stop_errors[0]))
                    self.assertIsNone(sentinel.poll())
                    with self.assertRaisesRegex(RuntimeError,'new child admission is closed'):
                        c.run([sys.executable,'-c','pass'])
                finally:
                    # Cooperative private teardown also drains the old, unregistered
                    # implementation if this regression fails before finding its child.
                    halt.write_text('stop')
                    # No global Popen replacement: retain only exact admitted objects.
                    with c._lock:children=list(c._fallback_children)
                    for child in children:
                        if child.poll() is None:c.signal_session(child,signal.SIGKILL,time.monotonic()+5)
                        child.wait(timeout=5)
                    if stopping is not None:stopping.join(15)
                    if worker is not None:worker.join(10)
                    if sentinel is not None:
                        if sentinel.poll() is None:sentinel.kill()
                        sentinel.wait(timeout=5)


class InterruptedChildCustodyTests(unittest.TestCase):
    @unittest.skipUnless(hasattr(__import__('os'), 'waitid') and hasattr(__import__('os'), 'pidfd_open'), 'Linux waitid/pidfd session control')
    def test_exited_leader_retains_grandchild_custody_on_wait_timeout_sigterm_and_interrupt(self):
        """Actual root exits first; grandchild creates its own compiler-like PGID."""
        import json, os, signal, subprocess, sys, tempfile
        code = r'''import json, os, signal, subprocess, sys, threading, time
from pathlib import Path
import process_custody as c
mode, pidfile = sys.argv[1:]
def publish(role,pid):
    fields=Path('/proc/'+str(pid)+'/stat').read_text().rsplit(')',1)[1].split()
    row={'role':role,'pid':pid,'start':int(fields[19]),'sid':int(fields[3]),'pgrp':int(fields[2]),'parent':int(fields[1])}
    path=Path(pidfile).with_name(role+'.json')
    temporary=path.with_suffix('.tmp');temporary.write_text(json.dumps(row));temporary.replace(path)
    return row
# This unrelated session is present before custody starts and must survive.
sentinel = None
root_code = """import json,os,subprocess,sys
from pathlib import Path
def publish(role,pid):
    stat=Path('/proc/'+str(pid)+'/stat').read_text().rsplit(')',1)[1].split()
    row={'role':role,'pid':pid,'start':int(stat[19]),'sid':int(stat[3]),'pgrp':int(stat[2]),'parent':int(stat[1])}
    path=Path(sys.argv[1]).with_name(role+'.json')
    temporary=path.with_suffix('.tmp');temporary.write_text(json.dumps(row));temporary.replace(path)
    return row
publish('root',os.getpid())
p=subprocess.Popen([sys.executable,'-c','import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(60)'],process_group=0,stdout=None if sys.argv[2] in ('timeout','communicate-interrupt') else subprocess.DEVNULL)
try:
    identity=publish('grandchild',p.pid)
    Path(sys.argv[1]+'.tmp').write_text(json.dumps(identity));Path(sys.argv[1]+'.tmp').replace(sys.argv[1])
except BaseException:
    p.kill();p.wait();raise
sys.exit(0)
"""
cancelled = threading.Event()
def terminate(*_args):
    if mode=='sigterm':cancelled.set()
    else:raise SystemExit(143)
signal.signal(signal.SIGTERM,terminate)
errors=[]
def timed_run():
    try:
        c.run([sys.executable,'-c',root_code,pidfile,mode],capture_output=True,timeout=2)
    except subprocess.TimeoutExpired:
        return
    except BaseException as error:
        errors.append(repr(error))
    else:
        errors.append('timeout was not preserved')
try:
    sentinel = subprocess.Popen([sys.executable, '-c', 'import time;time.sleep(60)'], start_new_session=True)
    publish('sentinel',sentinel.pid)
    c.activate()
    if mode == 'timeout':
        worker=threading.Thread(target=timed_run);worker.start()
    else:
        child=c.popen([sys.executable,'-c',root_code,pidfile,mode],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    until=time.monotonic()+5
    while not Path(pidfile).exists():
        assert time.monotonic()<until
        time.sleep(.01)
    identity=json.loads(Path(pidfile).read_text())
    if not mode.endswith('-interrupt'):
        print(json.dumps(identity),flush=True)
    if mode != 'timeout':
        # For interrupts, observe without setting Popen.returncode: inherited
        # CPython _wait would otherwise short-circuit instead of exposing the
        # leader-first private-reap regression this control must detect.
        while os.waitid(os.P_PID,child.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT) is None:
            assert time.monotonic()<until
            time.sleep(.01)
        if not mode.endswith('-interrupt'):
            assert child.poll()==0
            assert os.waitid(os.P_PID,child.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT) is not None
        if mode.endswith('-interrupt'):
            assert child.returncode is None
            interrupted=False
            try:
                if mode == 'communicate-interrupt':
                    communicate=child._communicate
                    def announced_communicate(*args):
                        print(json.dumps(dict(identity,interrupt_ready=mode)),flush=True)
                        return communicate(*args)
                    child._communicate=announced_communicate
                    child.communicate(timeout=8)
                else:
                    # Exercise CPython's exhausted short Ctrl-C grace. It
                    # must preserve KeyboardInterrupt AND the unreaped anchor.
                    child._sigint_wait_secs=1e-9
                    with child:
                        print(json.dumps(dict(identity,interrupt_ready=mode)),flush=True)
                        signal.pause()
            except KeyboardInterrupt:
                interrupted=True
            assert interrupted, 'KeyboardInterrupt was replaced or swallowed'
            if mode == 'context-interrupt':
                assert not child.retired, 'expired grace claimed retirement'
            if not child.retired:
                assert os.waitid(os.P_PID,child.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT) is not None
                child.retire(time.monotonic()+5)
        elif mode == 'normal':
            assert child.wait(timeout=5)==0
        else:
            assert cancelled.wait(10)
            result=c.stop_before_exit()
            assert result['owned_sessions_retired'] and result['anchors_reaped_last']
    else:
        worker.join(8);assert not worker.is_alive() and not errors,errors
    assert all(child.retired for child in c._children)
    for pid in [identity['pid'],*[child.pid for child in c._children]]:
        try:
            os.waitpid(pid,os.WNOHANG)
        except ChildProcessError:
            pass
        else:
            raise AssertionError('root or adopted grandchild was not reaped')
    assert sentinel.poll() is None
    print(json.dumps({'sessions_retired':True,'adopted_grandchild_reaped':True,'sentinel_alive':True}),flush=True)
finally:
    # Failure cleanup retains the same exact recorded custody, never a host scan.
    try:
        c.stop_before_exit()
    finally:
        if sentinel is not None:
            sentinel.terminate();sentinel.wait()
'''
        for mode in ('normal', 'timeout', 'sigterm', 'communicate-interrupt', 'context-interrupt'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                identity_path=Path(tmp)/'owned-grandchild.json'
                env=dict(os.environ,PYTHONPATH=str(Path(surface.__file__).parent))
                runner=subprocess.Popen([sys.executable,'-u','-c',code,mode,str(identity_path)],
                    env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
                runner_fd=os.pidfd_open(runner.pid)
                owned_fds={}
                def pin_published():
                    # Exact literal private files, including records written
                    # before stdout announcement. Never infer a PID from argv.
                    records={}
                    for role in ('sentinel','root','grandchild'):
                        path=Path(tmp)/(role+'.json')
                        try:
                            fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
                        except FileNotFoundError:
                            continue
                        try:
                            with os.fdopen(fd,'rb') as stream:raw=stream.read(1025)
                            self.assertLessEqual(len(raw),1024)
                            row=json.loads(raw)
                        except BaseException:
                            raise
                        self.assertEqual(row['role'],role)
                        self.assertGreater(row['pid'],0)
                        self.assertGreater(row['start'],0)
                        records[role]=row
                    if 'root' in records:
                        self.assertEqual(records['root']['sid'],records['root']['pid'])
                        self.assertEqual(records['root']['pgrp'],records['root']['pid'])
                        self.assertEqual(records['root']['parent'],runner.pid)
                    if 'sentinel' in records:
                        self.assertEqual(records['sentinel']['sid'],records['sentinel']['pid'])
                        self.assertEqual(records['sentinel']['pgrp'],records['sentinel']['pid'])
                        self.assertEqual(records['sentinel']['parent'],runner.pid)
                    if 'grandchild' in records:
                        self.assertIn('root',records)
                        self.assertEqual(records['grandchild']['sid'],records['root']['pid'])
                        self.assertEqual(records['grandchild']['pgrp'],records['grandchild']['pid'])
                        self.assertNotEqual(records['grandchild']['pgrp'],records['root']['pgrp'])
                        self.assertEqual(records['grandchild']['parent'],records['root']['pid'])
                    for role,row in records.items():
                        if role in owned_fds:continue
                        pid=row['pid']
                        def matches():
                            stat=Path('/proc/'+str(pid)+'/stat').read_text().rsplit(')',1)[1].split()
                            return int(stat[19])==row['start'] and int(stat[3])==row['sid'] and int(stat[2])==row['pgrp']
                        try:
                            if not matches():continue
                            fd=os.pidfd_open(pid)
                            try:
                                if not matches():continue
                                owned_fds[role]=fd;fd=None
                            finally:
                                if fd is not None:os.close(fd)
                        except (FileNotFoundError,ProcessLookupError):
                            pass
                    return records
                try:
                    import select
                    self.assertTrue(select.select([runner.stdout],[],[],6)[0], 'owned grandchild not announced')
                    identity=json.loads(runner.stdout.readline())
                    published=pin_published()
                    self.assertEqual(set(published),{'sentinel','root','grandchild'})
                    if mode.endswith('-interrupt'):
                        self.assertEqual(identity['interrupt_ready'],mode)
                        signal.pidfd_send_signal(runner_fd,signal.SIGINT)
                    elif mode=='sigterm':
                        signal.pidfd_send_signal(runner_fd,signal.SIGTERM)
                    stdout,stderr=runner.communicate(timeout=15)
                    self.assertEqual(runner.returncode,0,stderr)
                    result=json.loads(stdout)
                    self.assertTrue(result['sessions_retired'] and result['adopted_grandchild_reaped'] and result['sentinel_alive'])
                    print(json.dumps({'event':'owned-lifecycle-mode','mode':mode,'runner_pid':runner.pid,
                        'identities':published,'result':result}),flush=True)
                finally:
                    # Failure may precede announcement. Read the already-known
                    # private records BEFORE stopping the runner, and preserve
                    # exact pidfds while its cooperative finally still reaps.
                    cleanup_errors=[]
                    def try_pin():
                        try:return pin_published()
                        except BaseException as error:
                            cleanup_errors.append(repr(error));return {}
                    try:
                        try_pin()
                        if runner.poll() is None:
                            try:signal.pidfd_send_signal(runner_fd,signal.SIGTERM)
                            except ProcessLookupError:pass
                            try:runner.communicate(timeout=10)
                            except subprocess.TimeoutExpired:pass
                        try_pin()
                        if runner.poll() is None:
                            # Descendants/sentinel first; runner LAST. Numeric
                            # PID reuse cannot redirect these identity-joined fds.
                            for role in ('grandchild','root','sentinel'):
                                if role in owned_fds:
                                    try:signal.pidfd_send_signal(owned_fds[role],signal.SIGKILL)
                                    except ProcessLookupError:pass
                            try:runner.communicate(timeout=2)
                            except subprocess.TimeoutExpired:
                                try:signal.pidfd_send_signal(runner_fd,signal.SIGKILL)
                                except ProcessLookupError:pass
                                runner.communicate(timeout=3)
                        # A runner may already have died while its recorded
                        # children survive. Their same pidfds still own cleanup.
                        for fd in owned_fds.values():
                            try:signal.pidfd_send_signal(fd,signal.SIGKILL)
                            except ProcessLookupError:pass
                        runner.wait(timeout=3)
                        import select
                        for role,fd in owned_fds.items():
                            poller=select.poll();poller.register(fd,select.POLLIN)
                            if not poller.poll(2000):cleanup_errors.append(role+' did not exit after exact pidfd cleanup')
                        if cleanup_errors:self.fail('owned cleanup unresolved: '+repr(cleanup_errors))
                    finally:
                        for fd in owned_fds.values():os.close(fd)
                        os.close(runner_fd)


if __name__ == "__main__":
    unittest.main()
