"""Pure-Python contract tests for the prepared live canary's safe boundaries."""

from __future__ import annotations

import importlib.util
import json
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


RUNNER_PATH = Path(__file__).with_name("live_registry_discovery_canary.py")
SPEC = importlib.util.spec_from_file_location("live_registry_discovery_canary", RUNNER_PATH)
assert SPEC is not None and SPEC.loader is not None
CANARY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CANARY)


def valid_parameters(**overrides: object) -> SimpleNamespace:
    values: dict[str, object] = {
        "max_pages": 1,
        "search_limit": 50,
        "repetitions": 3,
        "max_operations": 2048,
        "ingest_wait_seconds": 60.0,
        "ingest_poll_seconds": 1.0,
        "timeout_seconds": 90.0,
        "source_timeout_seconds": 10.0,
        "source_budget_seconds": 180.0,
        "owner_runtime_seconds": 1200.0,
        "forge_repository": "owner/repository",
        "forge_tag": "v1.2.3",
    }
    values.update(overrides)
    return SimpleNamespace(**values)


class OsVResponseTests(unittest.TestCase):
    def test_empty_and_token_only_pages_are_closed_or_explicitly_paged(self) -> None:
        self.assertEqual(CANARY.parse_osv_query_page({}), ([], None))
        self.assertEqual(
            CANARY.parse_osv_query_page({"future_optional_field": True}),
            ([], None),
        )
        self.assertEqual(
            CANARY.parse_osv_query_page({"next_page_token": "next"}),
            ([], "next"),
        )
        self.assertEqual(
            CANARY.parse_osv_query_page({"next_page_token": ""}),
            ([], None),
        )
        self.assertEqual(
            CANARY.parse_osv_query_page({"vulns": [{"id": "GHSA-1"}]}),
            ([{"id": "GHSA-1"}], None),
        )

    def test_malformed_osv_shapes_are_rejected(self) -> None:
        malformed = (
            None,
            [],
            {"vulns": None},
            {"vulns": {}},
            {"vulns": [{}]},
            {"vulns": [{"id": "   "}]},
            {"next_page_token": None},
            {"next_page_token": 1},
        )
        for value in malformed:
            with self.subTest(value=value), self.assertRaises(ValueError):
                CANARY.parse_osv_query_page(value)


class JournalAndDownloadEvidenceTests(unittest.TestCase):
    def test_journal_signature_ignores_only_workspace_location(self) -> None:
        source = {
            "path": "/live/registry-discovery/catalog.journal",
            "exists": True,
            "bytes": 48,
            "sha256": "a" * 64,
            "transaction_count": 1,
            "fact_count": 2,
            "transactions": [{"offset": 0, "fact_count": 2, "caught_up": False}],
        }
        copied = {**source, "path": "/backup/registry-discovery/catalog.journal"}
        self.assertEqual(
            CANARY.discovery_journal_content_signature(source),
            CANARY.discovery_journal_content_signature(copied),
        )
        for key, changed in (
            ("bytes", 49),
            ("sha256", "b" * 64),
            ("fact_count", 3),
            ("transactions", [{"offset": 0, "fact_count": 1, "caught_up": False}]),
        ):
            different = {**copied, key: changed}
            with self.subTest(key=key):
                self.assertNotEqual(
                    CANARY.discovery_journal_content_signature(source),
                    CANARY.discovery_journal_content_signature(different),
                )

    def test_known_zero_download_requires_exact_per_release_source_fact(self) -> None:
        projection = {"state": "known", "value": 0}
        exact_release_zero = {
            "downloads_source_state": "known-count",
            "downloads_source_scope": "per-release",
            "downloads_source_raw": 0,
        }
        self.assertTrue(CANARY.download_zero_was_not_manufactured(exact_release_zero, projection))
        self.assertFalse(
            CANARY.download_zero_was_not_manufactured(
                {**exact_release_zero, "downloads_source_scope": "project-level"}, projection
            )
        )
        self.assertFalse(
            CANARY.download_zero_was_not_manufactured(
                {**exact_release_zero, "downloads_source_raw": False}, projection
            )
        )
        self.assertFalse(
            CANARY.download_zero_was_not_manufactured(
                {"downloads_source_state": "unknown"}, projection
            )
        )
        self.assertTrue(
            CANARY.download_zero_was_not_manufactured(
                {"downloads_source_state": "unknown"}, {"state": "unknown"}
            )
        )


class ParameterAdmissionTests(unittest.TestCase):
    def test_valid_bounded_configuration_and_shared_cli_cap(self) -> None:
        CANARY.validate_canary_parameters(valid_parameters())
        budget = CANARY.CliOperationBudget(1)
        self.assertEqual(budget.admit("first"), 1)
        with self.assertRaises(ValueError):
            budget.admit("second")
        self.assertEqual(budget.used, 1)

    def test_all_durations_reject_nonfinite_nonpositive_and_over_limit_values(self) -> None:
        maxima = {
            "ingest_wait_seconds": CANARY.MAX_INGEST_WAIT_SECONDS,
            "ingest_poll_seconds": CANARY.MAX_INGEST_POLL_SECONDS,
            "timeout_seconds": CANARY.MAX_CLI_TIMEOUT_SECONDS,
            "source_timeout_seconds": CANARY.MAX_SOURCE_TIMEOUT_SECONDS,
            "source_budget_seconds": CANARY.MAX_SOURCE_BUDGET_SECONDS,
            "owner_runtime_seconds": CANARY.MAX_OWNER_RUNTIME_SECONDS,
        }
        for name, maximum in maxima.items():
            for bad in (float("nan"), float("inf"), 0.0, maximum + 1.0):
                overrides = {name: bad}
                if name == "ingest_wait_seconds":
                    overrides["ingest_poll_seconds"] = 0.5
                if name == "ingest_poll_seconds":
                    overrides["ingest_wait_seconds"] = max(1.0, bad if bad != float("inf") else 60.0)
                with self.subTest(name=name, bad=bad), self.assertRaises(ValueError):
                    CANARY.validate_canary_parameters(valid_parameters(**overrides))

    def test_operation_repetition_and_query_bounds_are_explicit(self) -> None:
        invalid = (
            {"max_operations": 0},
            {"max_operations": CANARY.MAX_CLI_OPERATIONS + 1},
            {"repetitions": 1},
            {"repetitions": CANARY.MAX_CANARY_REPETITIONS + 1},
            {"search_limit": 201},
            {"max_pages": 3},
        )
        for overrides in invalid:
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                CANARY.validate_canary_parameters(valid_parameters(**overrides))


class BoundedProcessTests(unittest.TestCase):
    def make_process(
        self,
        root: Path,
        name: str,
        code: str,
        *,
        stdout_limit: int = 1024,
        stderr_limit: int = 1024,
        timeout: float = 2.0,
        drain_timeout: float | None = None,
        input_data: bytes | None = None,
    ) -> CANARY.BoundedProcess:
        return CANARY.BoundedProcess(
            [sys.executable, "-c", code],
            stdout_path=root / f"{name}.stdout",
            stderr_path=root / f"{name}.stderr",
            receipt_path=root / f"{name}.receipt.json",
            max_stdout_bytes=stdout_limit,
            max_stderr_bytes=stderr_limit,
            timeout_seconds=timeout,
            drain_timeout_seconds=drain_timeout,
            input_data=input_data,
        )

    def test_stdout_and_stderr_are_separately_capped_with_receipts(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-bounded-process-test-") as temporary:
            root = Path(temporary)
            for stream in ("stdout", "stderr"):
                code = f"import sys; sys.{stream}.buffer.write(b'x' * 262144); sys.{stream}.flush()"
                process = self.make_process(
                    root, stream, code, stdout_limit=128, stderr_limit=128
                )
                with self.subTest(stream=stream), self.assertRaises(CANARY.BoundedProcessError) as caught:
                    process.wait()
                receipt = caught.exception.receipt
                self.assertIn("byte limit exceeded", receipt["failure_reason"])
                self.assertLessEqual(receipt[f"{stream}_bytes_stored"], 128)
                self.assertGreater(receipt[f"{stream}_bytes_observed"], 128)
                self.assertTrue(Path(receipt["receipt_path"]).is_file())
                self.assertEqual(json.loads(Path(receipt["receipt_path"]).read_text()), receipt)

    def test_timeout_kills_owned_descendant_but_spares_another_process_group(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-bounded-process-timeout-") as temporary:
            root = Path(temporary)
            owned_marker = root / "owned-child-ran"
            independent_marker = root / "independent-process-ran"
            owned_child = (
                "import pathlib,time; time.sleep(0.8); "
                f"pathlib.Path({str(owned_marker)!r}).write_text('ran')"
            )
            parent = (
                "import subprocess,sys,time; "
                f"subprocess.Popen([sys.executable,'-c',{owned_child!r}]); "
                "time.sleep(10)"
            )
            independent_code = (
                "import pathlib,time; time.sleep(0.55); "
                f"pathlib.Path({str(independent_marker)!r}).write_text('ran')"
            )
            independent = subprocess.Popen(
                [sys.executable, "-c", independent_code], start_new_session=True
            )
            process = self.make_process(root, "timeout", parent, timeout=0.25)
            try:
                with self.assertRaises(CANARY.BoundedProcessError) as caught:
                    process.wait()
                self.assertIn("wall-time limit", caught.exception.receipt["failure_reason"])
                independent.wait(timeout=2)
                time.sleep(0.35)
                self.assertFalse(owned_marker.exists(), "owned descendant survived group cleanup")
                self.assertTrue(independent_marker.exists(), "collector killed an unrelated process group")
            finally:
                if independent.poll() is None:
                    CANARY.kill_process_group_if_present(independent.pid)
                    independent.wait(timeout=2)

    def test_forced_wait_timeout_retains_partial_receipt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-bounded-process-forced-") as temporary:
            root = Path(temporary)
            process = self.make_process(
                root,
                "forced",
                "import sys,time; print('partial', flush=True); time.sleep(10)",
                timeout=5.0,
            )
            with self.assertRaises(CANARY.BoundedProcessError) as caught:
                process.wait(timeout=0.05, force_on_timeout=True)
            receipt = caught.exception.receipt
            self.assertIn("wall-time deadline", receipt["failure_reason"])
            self.assertEqual(Path(receipt["stdout_path"]).read_bytes(), b"partial\n")
            self.assertTrue(Path(receipt["receipt_path"]).is_file())

    def test_detached_descendant_holding_pipes_returns_bounded_partial_failure(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-bounded-process-detached-") as temporary:
            root = Path(temporary)
            marker = root / "detached-child-finished"
            detached_child = (
                "import pathlib,time; time.sleep(0.7); "
                f"pathlib.Path({str(marker)!r}).write_text('survived')"
            )
            parent = (
                "import subprocess,sys; "
                f"subprocess.Popen([sys.executable,'-c',{detached_child!r}],start_new_session=True); "
                "print('parent output', flush=True)"
            )
            process = self.make_process(
                root, "detached", parent, timeout=2.0, drain_timeout=0.15
            )
            started = time.monotonic()
            with self.assertRaises(CANARY.BoundedProcessError) as caught:
                process.wait()
            elapsed = time.monotonic() - started
            receipt = caught.exception.receipt
            self.assertLess(elapsed, 1.0)
            self.assertIn("kept an output pipe open", receipt["failure_reason"])
            self.assertEqual(Path(receipt["stdout_path"]).read_bytes(), b"parent output\n")
            self.assertTrue(receipt["drain_threads_stopped"])
            deadline = time.monotonic() + 2.0
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(marker.exists(), "group cleanup killed a new-session descendant")

    def test_wait_racing_wall_timeout_finishes_and_never_signals_after_reap(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-bounded-process-race-") as temporary:
            root = Path(temporary)
            process = self.make_process(
                root,
                "race",
                "import time; time.sleep(0.08); print('race complete', flush=True)",
                timeout=0.08,
                drain_timeout=0.2,
            )
            outcomes: list[object] = []

            def waiter() -> None:
                try:
                    outcomes.append(process.wait())
                except BaseException as error:
                    outcomes.append(error)

            thread = threading.Thread(target=waiter)
            thread.start()
            thread.join(timeout=1.0)
            self.assertFalse(thread.is_alive(), "wait and the timeout callback deadlocked")
            self.assertEqual(len(outcomes), 1)
            self.assertIsNotNone(process.poll())
            with mock.patch.object(CANARY.os, "killpg") as killpg:
                process._on_timeout()
                process.terminate_owned_group()
                killpg.assert_not_called()
            if isinstance(outcomes[0], BaseException):
                self.assertIsInstance(outcomes[0], CANARY.BoundedProcessError)
                self.assertTrue(outcomes[0].receipt["receipt_path"])
            else:
                self.assertEqual(outcomes[0]["exit_code"], 0)  # type: ignore[index]

    def test_detached_child_holding_stdin_is_cancelled_after_parent_exit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="nudox-bounded-process-stdin-") as temporary:
            root = Path(temporary)
            marker = root / "stdin-holder-finished"
            detached_child = (
                "import pathlib,time; time.sleep(0.7); "
                f"pathlib.Path({str(marker)!r}).write_text('survived')"
            )
            parent = (
                "import subprocess,sys; "
                f"subprocess.Popen([sys.executable,'-c',{detached_child!r}],start_new_session=True); "
                "print('leader exiting', flush=True)"
            )
            process = self.make_process(
                root,
                "stdin-holder",
                parent,
                timeout=3.0,
                drain_timeout=0.15,
                input_data=b"x" * (4 * 1024 * 1024),
            )
            started = time.monotonic()
            with self.assertRaises(CANARY.BoundedProcessError) as caught:
                process.wait()
            elapsed = time.monotonic() - started
            with mock.patch.object(CANARY.os, "killpg") as killpg:
                process.terminate_owned_group()
                killpg.assert_not_called()
            receipt = caught.exception.receipt
            self.assertLess(elapsed, 1.2)
            self.assertLess(receipt["input_bytes_written"], receipt["input_bytes"])
            self.assertTrue(receipt["input_writer_stopped"])
            self.assertIsNotNone(receipt["input_writer_failure_reason"])
            self.assertTrue(Path(receipt["stdout_path"]).read_bytes().startswith(b"leader exiting\n"))
            deadline = time.monotonic() + 2.0
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(marker.exists(), "cleanup signalled the detached stdin holder")


class _LoopbackHttpHandler(socketserver.StreamRequestHandler):
    def handle(self) -> None:
        request_line = self.rfile.readline(8192)
        pieces = request_line.split()
        path = pieces[1].decode("ascii", errors="replace") if len(pieces) > 1 else "/"
        for _ in range(100):
            line = self.rfile.readline(8192)
            if not line or line in (b"\r\n", b"\n"):
                break
        mode = self.server.mode  # type: ignore[attr-defined]
        try:
            if mode == "slow-headers":
                self.wfile.write(b"HTTP/1.1 200 OK\r\n")
                self.wfile.flush()
                time.sleep(0.20)
                self.wfile.write(b"Content-Length: 2\r\n")
                self.wfile.flush()
                time.sleep(0.20)
                self.wfile.write(b"\r\nok")
                self.wfile.flush()
            elif mode == "slow-body":
                self.wfile.write(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n"
                )
                self.wfile.flush()
                for _ in range(8):
                    self.wfile.write(b"x")
                    self.wfile.flush()
                    time.sleep(0.16)
            elif mode == "redirect-chain":
                self.server.paths.append(path)  # type: ignore[attr-defined]
                if path != "/end":
                    time.sleep(0.30 if path == "/start" else 0.90)
                    next_path = "/middle" if path == "/start" else "/end"
                    location = f"{self.server.base_url}{next_path}".encode()  # type: ignore[attr-defined]
                    self.wfile.write(
                        b"HTTP/1.1 302 Found\r\nLocation: " + location
                        + b"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    self.wfile.flush()
                else:
                    self.wfile.write(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                    )
                    self.wfile.flush()
            else:
                payload = b'{"next_page_token":""}'
                self.wfile.write(
                    b"HTTP/1.1 200 OK\r\nContent-Length: "
                    + str(len(payload)).encode()
                    + b"\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n"
                    + payload
                )
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass


class _LoopbackHttpServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
    block_on_close = False


class HttpEvidenceDeadlineTests(unittest.TestCase):
    def start_server(self, mode: str) -> tuple[_LoopbackHttpServer, threading.Thread, str]:
        server = _LoopbackHttpServer(("127.0.0.1", 0), _LoopbackHttpHandler)
        server.mode = mode  # type: ignore[attr-defined]
        server.paths = []  # type: ignore[attr-defined]
        server.base_url = f"http://127.0.0.1:{server.server_address[1]}"  # type: ignore[attr-defined]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        return server, thread, server.base_url

    def assert_bounded_timeout(self, mode: str, path: str = "/") -> None:
        server, thread, base_url = self.start_server(mode)
        try:
            with tempfile.TemporaryDirectory(prefix="nudox-http-deadline-") as temporary:
                root = Path(temporary)
                evidence = CANARY.HttpEvidence(root, timeout_seconds=2.0, total_seconds=0.35)
                started = time.monotonic()
                with self.assertRaises(TimeoutError):
                    evidence.request("adversarial", f"{base_url}{path}", maximum_bytes=128)
                elapsed = time.monotonic() - started
                self.assertLess(elapsed, 1.1)
                self.assertEqual(len(evidence.rows), 1)
                row = evidence.rows[0]
                self.assertEqual(row["status"], "unavailable")
                receipt_path = Path(str(row["transport_process_receipt_path"]))
                self.assertTrue(receipt_path.is_file())
                self.assertTrue(Path(str(row["transport_stdout_path"])).is_file())
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=1.0)

    def test_whole_request_deadline_covers_slow_headers(self) -> None:
        self.assert_bounded_timeout("slow-headers")

    def test_whole_request_deadline_covers_slow_response_body_and_keeps_partial_bytes(self) -> None:
        server, thread, base_url = self.start_server("slow-body")
        try:
            with tempfile.TemporaryDirectory(prefix="nudox-http-slow-body-") as temporary:
                root = Path(temporary)
                evidence = CANARY.HttpEvidence(root, timeout_seconds=2.0, total_seconds=0.35)
                started = time.monotonic()
                with self.assertRaises(TimeoutError):
                    evidence.request("slow-body", base_url, maximum_bytes=128)
                self.assertLess(time.monotonic() - started, 1.1)
                row = evidence.rows[0]
                self.assertGreater(row["partial_response_bytes"], 0)
                partial = Path(str(row["partial_response_path"]))
                self.assertTrue(partial.is_file())
                self.assertEqual(
                    CANARY.sha256_file(partial), row["partial_response_sha256"]
                )
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=1.0)

    def test_whole_request_deadline_covers_multiple_individually_fast_redirects(self) -> None:
        server, thread, base_url = self.start_server("redirect-chain")
        try:
            with tempfile.TemporaryDirectory(prefix="nudox-http-redirects-") as temporary:
                root = Path(temporary)
                evidence = CANARY.HttpEvidence(root, timeout_seconds=3.0, total_seconds=1.0)
                started = time.monotonic()
                with self.assertRaises(TimeoutError):
                    evidence.request("redirects", f"{base_url}/start", maximum_bytes=128)
                self.assertLess(time.monotonic() - started, 1.6)
                self.assertIn("/start", server.paths)  # type: ignore[attr-defined]
                self.assertIn("/middle", server.paths)  # type: ignore[attr-defined]
                self.assertEqual(evidence.rows[0]["status"], "unavailable")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=1.0)

    def test_empty_osv_token_is_normalized_but_raw_http_body_is_retained(self) -> None:
        server, thread, base_url = self.start_server("normal")
        try:
            with tempfile.TemporaryDirectory(prefix="nudox-http-osv-empty-token-") as temporary:
                root = Path(temporary)
                evidence = CANARY.HttpEvidence(root, timeout_seconds=1.0, total_seconds=2.0)
                payload = evidence.request("osv-empty-token", base_url, maximum_bytes=128)
                self.assertEqual(CANARY.parse_osv_query_page(json.loads(payload)), ([], None))
                row = evidence.rows[0]
                response_path = Path(str(row["response_path"]))
                self.assertEqual(response_path.read_bytes(), b'{"next_page_token":""}')
                self.assertEqual(CANARY.sha256_file(response_path), row["response_sha256"])
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=1.0)


if __name__ == "__main__":
    unittest.main()
