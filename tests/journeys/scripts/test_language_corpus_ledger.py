"""Receipt validation tests; these do not run or accept backend packages."""

import hashlib
import json
import os
import tempfile
import unittest
from pathlib import Path

import language_corpus_ledger as ledger


class CorpusLedgerTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.journal = self.root / "attempts.jsonl"
        self.candidate = "a" * 64

    def attempt(self, ordinal=1, version="1", setup="stock", status="PASS", **changes):
        run = {
            "schema": ledger.RESULT_SCHEMA, "status": status,
            "scope": {"kind": "language-corpus", "language": "python"},
            "source": {"build_manifest_sha256": self.candidate},
            "projects": [{"id": "requests", "source_tree_unchanged": True,
                          "root_identity_sha256": "f" * 64,
                          "package_provenance": {
                              "verification": "verified-source-inventory-v1",
                              "package": {"ecosystem": "pypi", "id": "requests", "version": version},
                              "source_tree_sha256": "b" * 64,
                              "acquired_inventory_sha256": "e" * 64,
                              "corpus_manifest_sha256": "a" * 64,
                              "target_subdir": ".", "target_root_identity_sha256": "f" * 64,
                          }}],
            "runtime_setup": {"mode": setup, "compiler_snapshot_injected": setup == "configured",
                              "compiler_override_keys": (["BACKEND_LOCALD_COMPILER_ENVIRONMENT"]
                                                         if setup == "configured" else []),
                              "owner_environment_sha256": "e" * 64},
            "environment_witnesses": {"owner_allowlisted_environment_sha256": "e" * 64},
            "operations": [{"project_id": "requests", "terminal_state": "published",
                            "cold_status_same_receipt": True, "same_key_replay_same_receipt": True,
                            "publication_receipt_sha256": "c" * 64}],
            "queries": [{"project_id": "requests", "profile": "python",
                         "matched_records": 1, "identity_sha256": "d" * 64}],
            "semantic_versions_before_restart": {"requests": {"identity": "stable"}},
            "semantic_versions_after_restart": {"requests": {"identity": "stable"}},
        }
        run.update(changes)
        path = self.root / f"result-{ordinal}-{version}-{setup}-{status}.json"
        raw = json.dumps(run).encode()
        path.write_bytes(raw)
        return {
            "schema": ledger.ATTEMPT_SCHEMA, "language": "python", "package": "pypi:requests",
            "version": version, "source_sha256": "b" * 64,
            "candidate_manifest_sha256": self.candidate, "attempt": ordinal, "setup": setup,
            "result_path": str(path), "result_sha256": hashlib.sha256(raw).hexdigest(),
        }

    def write(self, *attempts):
        self.journal.write_bytes(b"".join(json.dumps(a).encode() + b"\n" for a in attempts))

    def counts(self):
        return ledger.summarize([self.journal], self.candidate)["languages"]["python"]["counts"]

    def test_versions_and_out_of_order_retries_cannot_inflate_or_resurrect_passes(self):
        first = self.attempt()
        second = self.attempt(2, "2")
        self.write(second, first)
        self.assertEqual(self.counts().get("passed_stock"), 1)
        failed = self.attempt(3, "3", status="FAIL")
        self.write(failed, first, second)
        self.assertEqual(self.counts().get("passed_stock", 0), 0)
        self.assertEqual(self.counts()["fail"], 1)

    def test_other_builds_and_configured_setup_do_not_meet_stock_target(self):
        configured = self.attempt(setup="configured")
        foreign = self.attempt(2, "2")
        foreign["candidate_manifest_sha256"] = "e" * 64
        self.write(configured, foreign)
        summary = ledger.summarize([self.journal], self.candidate, target=1)
        self.assertEqual(summary["other_build_attempts_excluded"], 1)
        self.assertEqual(summary["languages"]["python"]["counts"]["passed_configured"], 1)
        self.assertFalse(summary["target_met"])

    def test_claimed_pass_without_publication_query_or_restart_is_invalid(self):
        for changes in [{"operations": []}, {"queries": []},
                        {"semantic_versions_after_restart": {}},
                        {"source": {"build_manifest_sha256": "e" * 64}},
                        {"projects": [{"id": "requests", "source_tree_unchanged": False}]}]:
            with self.subTest(changes=changes):
                self.write(self.attempt(**changes))
                self.assertEqual(self.counts()["invalid_evidence"], 1)

    def test_changed_result_and_conflicting_attempt_refuse_credit(self):
        attempt = self.attempt()
        self.write(attempt)
        Path(attempt["result_path"]).write_text("{}")
        self.assertEqual(self.counts()["invalid_evidence"], 1)
        first = self.attempt()
        conflicting = dict(first, source_sha256="e" * 64)
        self.write(first, conflicting)
        with self.assertRaises(ledger.InvalidEvidence):
            ledger.summarize([self.journal], self.candidate)

    def test_declared_source_or_wrong_package_tree_and_target_refuse_credit(self):
        for changes in [
            {"verification": "declared-by-corpus-manifest"},
            {"source_tree_sha256": "e" * 64},
            {"package": {"ecosystem": "pypi", "id": "urllib3", "version": "1"}},
            {"package": {"ecosystem": "pypi", "id": "requests", "version": "2"}},
            {"target_root_identity_sha256": "d" * 64},
            {"target_subdir": "src/../other"},
            {"target_subdir": "src//other"},
        ]:
            with self.subTest(changes=changes):
                attempt = self.attempt()
                path = Path(attempt["result_path"])
                run = json.loads(path.read_bytes())
                run["projects"][0]["package_provenance"].update(changes)
                raw = json.dumps(run).encode()
                path.write_bytes(raw)
                attempt["result_sha256"] = hashlib.sha256(raw).hexdigest()
                self.write(attempt)
                self.assertEqual(self.counts()["invalid_evidence"], 1)

    def test_stock_label_cannot_hide_compiler_injection_or_another_environment(self):
        for changes in [{"mode": "configured"}, {"compiler_snapshot_injected": True},
                        {"compiler_override_keys": ["NUDOX_GO"]},
                        {"owner_environment_sha256": "a" * 64}]:
            with self.subTest(changes=changes):
                attempt = self.attempt()
                path = Path(attempt["result_path"])
                run = json.loads(path.read_bytes())
                run["runtime_setup"].update(changes)
                raw = json.dumps(run).encode()
                path.write_bytes(raw)
                attempt["result_sha256"] = hashlib.sha256(raw).hexdigest()
                self.write(attempt)
                self.assertEqual(self.counts()["invalid_evidence"], 1)

    def test_special_or_multilink_result_cannot_block_reader_or_count(self):
        for kind in ["fifo", "symlink", "hardlink"]:
            with self.subTest(kind=kind):
                attempt = self.attempt()
                original = Path(attempt["result_path"])
                selected = self.root / f"selected-{kind}"
                if kind == "fifo":
                    os.mkfifo(selected)
                elif kind == "symlink":
                    selected.symlink_to(original)
                else:
                    os.link(original, selected)
                attempt["result_path"] = str(selected)
                self.write(attempt)
                self.assertEqual(self.counts()["invalid_evidence"], 1)

    def test_partial_tail_and_pending_attempt_cannot_manufacture_a_pass(self):
        pending = dict(self.attempt(), result_path=None, result_sha256=None)
        self.write(pending)
        with self.journal.open("ab") as stream:
            stream.write(b'{"schema":"unfinished')
        summary = ledger.summarize([self.journal], self.candidate)
        self.assertEqual(summary["incomplete_journal_tails"], 1)
        self.assertEqual(summary["languages"]["python"]["counts"]["pending"], 1)
        self.assertFalse(summary["target_met"])

    def test_exact_pending_attempt_can_complete_but_new_pending_retry_revokes_credit(self):
        completed = self.attempt()
        pending = dict(completed, result_path=None, result_sha256=None)
        for order in [(pending, completed), (completed, pending)]:
            self.write(*order)
            self.assertEqual(self.counts()["passed_stock"], 1)
        next_pending = dict(pending, attempt=2)
        self.write(completed, next_pending)
        self.assertEqual(self.counts().get("passed_stock", 0), 0)
        self.assertEqual(self.counts()["pending"], 1)

    def test_canonical_package_identity_and_strict_json_are_required(self):
        for name in ["pypi:Requests", "pypi:my_pkg", "requests", "pypi:bad name"]:
            with self.subTest(name=name), self.assertRaises(ledger.InvalidEvidence):
                ledger.Attempt.parse(dict(self.attempt(), package=name))
        with self.assertRaises(ledger.InvalidEvidence):
            ledger.decode(b'{"attempt":1,"attempt":2}')


if __name__ == "__main__":
    unittest.main()
