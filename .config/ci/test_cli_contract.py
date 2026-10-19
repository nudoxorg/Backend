"""A successful exit or lost diagnostic must never admit a negative CLI case."""
import json
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest

from cli_contract import FAULTS, check_fault, run_cases, run_one


class RefusalAdmissionTests(unittest.TestCase):
    def test_broken_executable_is_rejected_even_with_a_plausible_fault(self):
        for mutation in ("success", "state"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                binary = Path(temporary) / "backend-cli"
                body = {"answer": "fault", "slug": "usage", "operand": "serch", "cause": "unknown", "detail": "did you mean `search`"}
                state = "Path(sys.argv[sys.argv.index('--workspace') + 1]).mkdir()" if mutation == "state" else "pass"
                binary.write_text(f"#!{sys.executable}\nimport sys\nfrom pathlib import Path\n{state}\nprint({json.dumps(body)!r})\nraise SystemExit({0 if mutation == 'success' else 64})\n")
                binary.chmod(0o700)
                with self.assertRaises(RuntimeError):
                    run_cases(Path(temporary), ["unknown-command"])

    def test_current_declaration_usage_requires_selected_id_alternative(self):
        for name, verb, operand, suffix in (
            ("missing-coordinate", "show", None, " (supply a coordinate or --selected-id)"),
            ("missing-alias-coordinate", "document", "document", ""),
        ):
            fragments = next(case[5] for case in FAULTS if case[0] == name)
            detail = f"usage: nudox {verb} [COORDINATE] [--selected-id SELECTED_ID] [--cursor CURSOR]{suffix}"
            body = {"answer": "fault", "slug": "usage", "operand": operand, "detail": detail}
            result = subprocess.CompletedProcess([], 64, json.dumps(body), "")
            check_fault(result, "json", 64, operand, fragments)
            for removed in fragments:
                wrong = dict(body, detail=detail.replace(removed, ""))
                with self.subTest(case=name, removed=removed), self.assertRaises(RuntimeError):
                    check_fault(subprocess.CompletedProcess([], 64, json.dumps(wrong), ""),
                                "json", 64, operand, fragments)

    def test_help_requires_safe_installed_mcp_setup_in_actual_runner(self):
        from unittest.mock import patch
        help_text = ("--format human|markdown|json --passive nudox add . "
                     'nudox search "error handling" nudox mcp-config '
                     "Claude Code/Codex stdio setup; no config writes or service start surface <JSON>")
        with patch("cli_contract.process_custody.run", return_value=
                   subprocess.CompletedProcess([], 0, help_text, "")):
            run_one(Path("/not-executed"), "help")
        for removed in ("nudox mcp-config", "Claude Code/Codex stdio setup",
                        "no config writes or service start"):
            with self.subTest(removed=removed), patch("cli_contract.process_custody.run", return_value=
                    subprocess.CompletedProcess([], 0, help_text.replace(removed, ""), "")):
                with self.assertRaises(RuntimeError):
                    run_one(Path("/not-executed"), "help")

    def test_lost_suggestion_or_operand_cannot_pass(self):
        for field in ("operand", "detail"):
            body = {"answer": "fault", "slug": "usage", "operand": "serch", "cause": "unknown", "detail": "did you mean `search`"}
            body[field] = "wrong"
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                check_fault(subprocess.CompletedProcess([], 64, json.dumps(body), ""), "json", 64, "serch", ["did you mean `search`"])


if __name__ == "__main__":
    unittest.main()
