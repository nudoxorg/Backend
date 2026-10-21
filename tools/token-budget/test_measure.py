"""Focused writer/check regressions for measure.py, using a stub payload source."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("measure.py")
SPEC = importlib.util.spec_from_file_location("token_budget_measure", MODULE_PATH)
measure = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(measure)


class StubEncoding:
    def encode(self, payload):
        return [payload]


def generated_payloads(text="canonical"):
    payloads = {}
    for name in measure.PAYLOAD_FILES:
        payloads[name] = {
            "admitted": True,
            "payload": text,
            "bytes": len(text.encode("utf-8")),
            "estimated_tokens": 1,
            "allocations": {"count_total": 1, "bytes_total": 1},
        }
    payloads["atomic_oversized_refusal"] = {
        "admitted": False,
        "refusal": {
            "partial_bytes": 0,
            "budget_bytes": 64,
            "observed_bytes": len(text.encode("utf-8")),
        },
        "estimated_tokens": 1,
        "allocations": {"count_total": 1, "bytes_total": 1},
    }
    return payloads


class MeasureWriteTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.fixtures = self.root / "fixtures"
        self.fixtures.mkdir()
        self.metadata = {
            "version": "stub",
            "encoding": "stub",
            "artifact_sha256": "stub",
            "caps": {
                "default_bytes": 64,
                "default_tokens": 64,
                "hard_bytes": 64,
                "hard_tokens": 64,
            },
        }
        self.metadata_path = self.root / "tokenizer.json"
        self.metadata_path.write_text(json.dumps(self.metadata), encoding="utf-8")

    def tearDown(self):
        self.temp.cleanup()

    def run_main(self, writing, generated=None):
        argv = ["measure.py"] + (["--write"] if writing else [])
        with (
            mock.patch.object(measure, "ROOT", self.root),
            mock.patch.object(measure, "FIXTURE_ROOT", self.fixtures),
            mock.patch.object(measure, "METADATA_PATH", self.metadata_path),
            mock.patch.object(measure, "load_tokenizer", return_value=StubEncoding()),
            mock.patch.object(measure, "canonical_tool", return_value=generated or generated_payloads()),
            mock.patch("sys.argv", argv),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            return measure.main()

    def test_write_replaces_stale_payload_then_check_accepts_it(self):
        for filename in measure.PAYLOAD_FILES.values():
            (self.fixtures / filename).write_text("stale", encoding="utf-8")

        self.assertEqual(self.run_main(writing=True), 0)
        for filename in measure.PAYLOAD_FILES.values():
            self.assertEqual((self.fixtures / filename).read_text(encoding="utf-8"), "canonical")
        self.assertEqual(self.run_main(writing=False), 0)

        report_path = self.fixtures / "payload-budgets.json"
        report = json.loads(report_path.read_text(encoding="utf-8"))
        report["fixtures"]["common_empty_records_summary"]["sha256"] = "0" * 64
        report_path.write_text(json.dumps(report), encoding="utf-8")
        with self.assertRaisesRegex(SystemExit, "sha256 disagrees with canonical bytes"):
            self.run_main(writing=False)

        (self.fixtures / measure.PAYLOAD_FILES["common_empty_records_summary"]).write_text(
            "stale-again", encoding="utf-8"
        )
        with self.assertRaisesRegex(SystemExit, "differs from canonical typed DTO output"):
            self.run_main(writing=False)

    def test_write_rejects_over_budget_payload_without_replacing_fixtures(self):
        for filename in measure.PAYLOAD_FILES.values():
            (self.fixtures / filename).write_text("preserve", encoding="utf-8")
        self.metadata["caps"]["default_bytes"] = 2
        self.metadata_path.write_text(json.dumps(self.metadata), encoding="utf-8")
        with self.assertRaisesRegex(SystemExit, "exceeds its 2 byte cap"):
            self.run_main(writing=True)
        for filename in measure.PAYLOAD_FILES.values():
            self.assertEqual((self.fixtures / filename).read_text(encoding="utf-8"), "preserve")

        for filename in measure.PAYLOAD_FILES.values():
            (self.fixtures / filename).write_text("canonical", encoding="utf-8")
        self.metadata["caps"]["default_bytes"] = 64
        self.metadata["caps"]["default_tokens"] = 0
        self.metadata_path.write_text(json.dumps(self.metadata), encoding="utf-8")
        name = "common_empty_records_summary"
        with self.assertRaisesRegex(SystemExit, "exceeds its 0 token cap"):
            with mock.patch.object(measure, "FIXTURE_ROOT", self.fixtures):
                measure.check_payload(
                    name,
                    generated_payloads()[name],
                    StubEncoding(),
                    self.metadata,
                    writing=False,
                )


if __name__ == "__main__":
    unittest.main()
