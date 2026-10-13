"""A successful exit or lost diagnostic must never admit a negative CLI case."""
import json
import subprocess
import unittest

from cli_contract import check_fault


class RefusalAdmissionTests(unittest.TestCase):
    def test_successful_exit_cannot_pass_a_refusal(self):
        result = subprocess.CompletedProcess([], 0, json.dumps({"answer": "fault", "slug": "usage", "operand": "serch", "cause": "did you mean `search`"}), "")
        with self.assertRaises(RuntimeError):
            check_fault(result, "json", 64, "serch", ["did you mean `search`"])

    def test_lost_suggestion_or_operand_cannot_pass(self):
        for field in ("operand", "cause"):
            body = {"answer": "fault", "slug": "usage", "operand": "serch", "cause": "did you mean `search`"}
            body[field] = "wrong"
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                check_fault(subprocess.CompletedProcess([], 64, json.dumps(body), ""), "json", 64, "serch", ["did you mean `search`"])


if __name__ == "__main__":
    unittest.main()
