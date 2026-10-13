"""A successful exit or lost diagnostic must never admit a negative CLI case."""
import json
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest

from cli_contract import check_fault, run_cases


class RefusalAdmissionTests(unittest.TestCase):
    def test_broken_executable_is_rejected_even_with_a_plausible_fault(self):
        for mutation in ("success", "state"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                binary = Path(temporary) / "backend-cli"
                body = {"answer": "fault", "slug": "usage", "operand": "serch", "cause": "did you mean `search`"}
                state = "Path(sys.argv[sys.argv.index('--workspace') + 1]).mkdir()" if mutation == "state" else "pass"
                binary.write_text(f"#!{sys.executable}\nimport sys\nfrom pathlib import Path\n{state}\nprint({json.dumps(body)!r})\nraise SystemExit({0 if mutation == 'success' else 64})\n")
                binary.chmod(0o700)
                with self.assertRaises(RuntimeError):
                    run_cases(Path(temporary), ["unknown-command"])

    def test_lost_suggestion_or_operand_cannot_pass(self):
        for field in ("operand", "cause"):
            body = {"answer": "fault", "slug": "usage", "operand": "serch", "cause": "did you mean `search`"}
            body[field] = "wrong"
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                check_fault(subprocess.CompletedProcess([], 64, json.dumps(body), ""), "json", 64, "serch", ["did you mean `search`"])


if __name__ == "__main__":
    unittest.main()
