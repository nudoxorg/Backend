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


if __name__ == "__main__":
    unittest.main()
