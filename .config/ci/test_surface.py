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

            def consume(call, reply, exchange):
                observed.append(exchange(surface.mcp_continuation_call(call, reply["structuredContent"])))

            answer, _ = surface.run_mcp_interactive(owner, root,
                [{"tool": "backend.outline", "arguments": {}}], 3, consume)
            self.assertEqual(observed, [{"resumed": True}])
            self.assertEqual(answer["exit"], 0)
            self.assertEqual(len(answer["calls"]), 1)
            self.assertEqual(answer["calls"][0]["reply"]["structuredContent"]["nextCursor"], "mcp2-issued")


class ParallelCaseTests(unittest.TestCase):
    def invoke(self, failure=None, duplicate=False):
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
            reached = []

            def run(case, binaries):
                reached.append(case["name"])
                self.assertEqual(surface.CURRENT_CASE.get(), case["name"])
                rendezvous.wait(timeout=3)  # A serial executor cannot satisfy this.
                if case["name"] == failure:
                    raise RuntimeError("intentional setup failure")
                return {"answers": {"case": case["name"]}, "performance": {"seconds": {"query": 0.01}}}

            stdout, stderr = io.StringIO(), io.StringIO()
            argv = ["surface.py", "--bin-dir", str(root), "--jobs", "2", "--report", str(report)]
            with patch.object(surface, "SURFACE", root), patch.object(surface, "SCRATCH", root / "scratch"), patch.object(surface, "run_case", side_effect=run), patch.object(sys, "argv", argv), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                try:
                    result = surface.main()
                except SystemExit as error:
                    result = error.code
            return result, reached, json.loads(report.read_text()) if report.exists() else {}, stdout.getvalue(), stderr.getvalue()

    def test_distinct_cases_overlap_and_preserve_both_results(self):
        import json
        result, reached, report, output, error = self.invoke()
        self.assertEqual(result, 0, error + output)
        self.assertEqual(set(reached), {"alpha", "beta"})
        self.assertEqual(set(report), {"alpha", "beta"})
        metrics = [json.loads(line.split(" ", 1)[1]) for line in output.splitlines() if line.startswith("SURFACE-PERFORMANCE ")]
        self.assertEqual({row["case"] for row in metrics}, {"alpha", "beta"})

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


if __name__ == "__main__":
    unittest.main()
