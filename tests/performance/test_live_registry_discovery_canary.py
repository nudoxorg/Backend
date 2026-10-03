"""Pure-Python contract tests for the prepared live canary's safe boundaries."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace


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
            {"next_page_token": ""},
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
    ) -> CANARY.BoundedProcess:
        return CANARY.BoundedProcess(
            [sys.executable, "-c", code],
            stdout_path=root / f"{name}.stdout",
            stderr_path=root / f"{name}.stderr",
            receipt_path=root / f"{name}.receipt.json",
            max_stdout_bytes=stdout_limit,
            max_stderr_bytes=stderr_limit,
            timeout_seconds=timeout,
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


if __name__ == "__main__":
    unittest.main()
