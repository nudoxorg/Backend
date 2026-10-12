"""The snapshot admission contract must reject failed product setup."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("surface", Path(__file__).with_name("surface.py"))
surface = importlib.util.module_from_spec(spec)
spec.loader.exec_module(surface)


class AdmissionTests(unittest.TestCase):
    def test_failed_index_is_never_a_golden_answer(self):
        for answer in (
            {"exit": 1, "json": {"answer": "fault", "cause": "unreachable"}},
            {"exit": 0, "json": {"answer": "fault"}},
            {"exit": 0, "json": None},
        ):
            with self.subTest(answer=answer), self.assertRaises(RuntimeError):
                surface.require_cli_success("cli add", answer)

    def test_successful_structured_answer_is_admitted(self):
        surface.require_cli_success("cli add", {"exit": 0, "json": {"answer": "indexed"}})


if __name__ == "__main__":
    unittest.main()
