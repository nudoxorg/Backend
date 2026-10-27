"""A real window/query alone must not be accepted as the GUI source-page proof."""
import unittest
from gui_contract import (
    emit_failure_evidence,
    open_current_result,
    require_current_result,
    require_navigation_breadcrumb,
    require_visible,
    wait_for_symbol_details,
)
import surface


class GuiAdmissionTests(unittest.TestCase):
    expected = ['itoa', 'pub struct Buffer', 'MaybeUninit']

    def test_symbol_route_waits_for_body_before_source_key(self):
        now = [0.0]
        captures = iter([
            'project TOMLDecodeError Opening page...',
            'project TOMLDecodeError What you can do with it Opening page...',
            'project TOMLDecodeError What you can do with it Makes one',
        ])
        source_keys = []
        opened = wait_for_symbol_details(
            lambda: next(captures), 'TOMLDecodeError', 10,
            now=lambda: now[0],
            pause=lambda seconds: now.__setitem__(0, now[0] + seconds))
        source_keys.append(('s', now[0]))
        self.assertIn('Makes one', opened)
        self.assertEqual(source_keys, [('s', 0.4)])

    def test_symbol_body_wait_preserves_original_deadline(self):
        now = [0.0]
        source_keys = []
        with self.assertRaisesRegex(RuntimeError, 'details did not load before its deadline'):
            wait_for_symbol_details(
                lambda: 'project TOMLDecodeError Opening page...',
                'TOMLDecodeError', 0.5,
                now=lambda: now[0],
                pause=lambda seconds: now.__setitem__(0, now[0] + seconds))
            source_keys.append('s')
        self.assertEqual(now[0], 0.5)
        self.assertEqual(source_keys, [])

    def test_search_query_without_opened_source_does_not_pass(self):
        with self.assertRaises(RuntimeError):
            require_visible('itoa Search Buffer No results', self.expected)

    def test_matching_declaration_in_another_project_does_not_pass(self):
        with self.assertRaises(RuntimeError):
            require_visible('other-project pub struct Buffer bytes MaybeUninit', self.expected)

    def test_wrapped_native_source_text_preserves_the_contract(self):
        require_visible('itoa\nlib.rs\n pub struct\n Buffer {\n bytes: [MaybeUninit<u8>; 40]', self.expected)

    def test_retained_row_with_query_and_filename_is_not_ready(self):
        with self.assertRaisesRegex(RuntimeError, 'retained'):
            require_current_result(
                'Earlier result; index changed; cannot open until verified\n'
                'TOMLDecodeError src/tomli/parser.py',
                'TOMLDecodeError', 'parser.py')

    def test_captured_closed_route_breadcrumb_passes_despite_retained_library_copy(self):
        # OCR transcribed from the successful Tomli opened-result capture.
        success_window = (
            'project TOMLDecodeError TOMLDecodeError ValueError '
            'Earlier Library reading retained: 1 package names. '
            'Checking the current index; package links are unavailable.'
        )
        require_navigation_breadcrumb(
            success_window, 'project TOMLDecodeError', 'project', 'TOMLDecodeError')

    def test_stale_ask_capture_with_query_but_without_route_breadcrumb_is_refused(self):
        stale_window = (
            'Earlier search results; index changed; Links unavailable '
            'TOMLDecodeError project'
        )
        with self.assertRaisesRegex(RuntimeError, 'breadcrumb'):
            require_navigation_breadcrumb(
                stale_window, 'Q TOMLDecodeError', 'project', 'TOMLDecodeError')

    def test_long_symbol_breadcrumb_requires_exact_observed_symbol(self):
        for observed in (
            'TOMLDecodeErrox',
            'TOMLDecodeErrxor',
            'TOMLDecodeErro',
        ):
            with self.subTest(observed=observed):
                with self.assertRaisesRegex(RuntimeError, 'breadcrumb'):
                    require_navigation_breadcrumb(
                        'opened declaration page', f'project {observed}',
                        'project', 'TOMLDecodeError')

    def test_nearby_real_declaration_cannot_certify_requested_route(self):
        with self.assertRaisesRegex(RuntimeError, 'breadcrumb'):
            require_navigation_breadcrumb(
                'opened declaration page', 'project parseStatementA',
                'project', 'parseStatementB')

    def test_symbol_breadcrumb_refuses_two_ocr_character_errors(self):
        with self.assertRaisesRegex(RuntimeError, 'breadcrumb'):
            require_navigation_breadcrumb(
                'opened declaration page', 'project TOMLDecodeErrxy',
                'project', 'TOMLDecodeError')

    def test_short_symbol_breadcrumb_does_not_use_fuzzy_matching(self):
        with self.assertRaisesRegex(RuntimeError, 'breadcrumb'):
            require_navigation_breadcrumb(
                'opened declaration page', 'project Buffex', 'project', 'Buffer')

    def test_retained_row_is_retried_before_return(self):
        now = [0.0]
        captures = iter([
            'Earlier result; index changed; cannot open until verified\n'
            'TOMLDecodeError src/tomli/parser.py',
            'TOMLDecodeError src/tomli/parser.py',
            'Opened parser.py',
        ])
        submitted = []

        def pause(seconds):
            now[0] += seconds

        opened = open_current_result(
            lambda: next(captures), lambda: 'project TOMLDecodeError',
            lambda: submitted.append(now[0]),
            'project', 'TOMLDecodeError', 'parser.py', 10,
            now=lambda: now[0], pause=pause)

        self.assertEqual(opened, 'Opened parser.py')
        self.assertEqual(len(submitted), 1)
        self.assertEqual(submitted[0], 0.5)

    def test_root_advance_between_readiness_and_return_retries_without_shell_key(self):
        now = [0.0]
        captures = iter([
            'TOMLDecodeError src/tomli/parser.py',
            'Earlier result; index changed; cannot open until verified\n'
            'TOMLDecodeError src/tomli/parser.py',
            'TOMLDecodeError src/tomli/parser.py',
            'Opened parser.py',
        ])
        plates = iter([
            'TOMLDecodeError',
            'Q TOMLDecodeError',
            'project TOMLDecodeError',
        ])
        returns = []
        shell_keys = []

        def pause(seconds):
            now[0] += seconds

        open_current_result(
            lambda: next(captures), lambda: next(plates),
            lambda: returns.append(now[0]),
            'project', 'TOMLDecodeError', 'parser.py', 10,
            now=lambda: now[0], pause=pause)
        shell_keys.append('s')

        self.assertEqual(len(returns), 2)
        self.assertEqual(shell_keys, ['s'])
        self.assertEqual(now[0], 1.5)

    def test_post_return_route_is_rechecked_before_requiring_ask_filename(self):
        now = [0.0]
        captures = iter([
            'TOMLDecodeError src/tomli/_parser.py',
            'Opened declaration page',
            'Opened declaration page',
        ])
        plates = iter(['Q TOMLDecodeError', 'project TOMLDecodeError'])
        returns = []

        opened = open_current_result(
            lambda: next(captures), lambda: next(plates),
            lambda: returns.append(now[0]),
            'project', 'TOMLDecodeError', '_parser.py', 10,
            now=lambda: now[0],
            pause=lambda seconds: now.__setitem__(0, now[0] + seconds))

        self.assertEqual(opened, 'Opened declaration page')
        self.assertEqual(returns, [0.0])
        self.assertEqual(now[0], 1.0)

    def test_matching_route_at_entry_cannot_bypass_ask_and_return(self):
        now = [0.0]
        returns = []

        def topbar():
            raise AssertionError('route proof is unavailable before Return')

        with self.assertRaisesRegex(RuntimeError, 'before its deadline'):
            open_current_result(
                lambda: 'project TOMLDecodeError declaration page', topbar,
                lambda: returns.append(now[0]),
                'project', 'TOMLDecodeError', '_parser.py', 1,
                now=lambda: now[0],
                pause=lambda seconds: now.__setitem__(0, now[0] + seconds))

        self.assertEqual(returns, [])
        self.assertGreaterEqual(now[0], 1)

    def test_deadline_exhaustion_never_returns_on_retained_row(self):
        now = [0.0]
        returns = []

        def pause(seconds):
            now[0] += seconds

        with self.assertRaisesRegex(RuntimeError, 'before its deadline'):
            open_current_result(
                lambda: 'Earlier result; index changed; itoa src/itoa/lib.rs',
                lambda: 'itoa', lambda: returns.append(now[0]),
                'itoa', 'itoa', 'lib.rs', 1,
                now=lambda: now[0], pause=pause)
        self.assertEqual(returns, [])
        self.assertGreaterEqual(now[0], 1)

    def test_success_requires_ask_dismissal_and_missing_source_still_fails(self):
        now = [0.0]
        captures = iter(['itoa src/itoa/lib.rs', 'project home'])
        opened = open_current_result(
            lambda: next(captures), lambda: 'project itoa', lambda: None,
            'project', 'itoa', 'lib.rs', 10, now=lambda: now[0],
            pause=lambda seconds: now.__setitem__(0, now[0] + seconds))
        self.assertEqual(opened, 'project home')
        with self.assertRaisesRegex(RuntimeError, 'missing visible source'):
            require_visible('project home', self.expected)

    def test_oversized_observer_output_is_not_admitted(self):
        with self.assertRaises(RuntimeError):
            require_visible('x' * 32769, self.expected)

    def test_gui_memory_and_cpu_regressions_use_existing_guards(self):
        baseline = {'seconds': {}, 'gui_peak_rss_kib': 100000, 'gui_cpu_seconds': 3}
        measured = {'seconds': {}, 'gui_peak_rss_kib': 2000000, 'gui_cpu_seconds': 12}
        failures = surface.regressions(baseline, measured)
        self.assertTrue(any('gui_peak_rss_kib' in item for item in failures))
        self.assertTrue(any('gui_cpu_seconds' in item for item in failures))


class GuiFailureEvidenceTests(unittest.TestCase):
    def test_failure_retains_exact_observed_pixels_and_ocr_with_bounds(self):
        import base64, contextlib, hashlib, io, json, tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            data = b"observed pixels"
            (root / "window.png").write_bytes(data)
            (root / "opened-result.png").write_bytes(b"x" * (512 * 1024 + 1))
            (root / "window.txt").write_text("Search failed: publication reset exceeded its time budget")
            with contextlib.redirect_stdout(io.StringIO()) as output:
                emit_failure_evidence(root, RuntimeError("source content missing"))
            packet = json.loads(output.getvalue().split(" ", 1)[1])
            self.assertEqual(len(packet["captures"]), 1)
            image = packet["captures"][0]
            self.assertEqual(base64.b64decode(image["png_base64"]), data)
            self.assertEqual(image["sha256"], hashlib.sha256(data).hexdigest())
            self.assertIn("publication reset", packet["last_ocr"])

    def test_evidence_failure_preserves_the_original_failure(self):
        import contextlib, io, tempfile
        from pathlib import Path
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory, patch.object(Path, "is_file", side_effect=OSError("evidence unavailable")), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "original content failure"):
                try:
                    raise RuntimeError("original content failure")
                except Exception as error:
                    emit_failure_evidence(Path(directory), error)
                    raise

    def test_broken_log_transport_cannot_replace_product_failure(self):
        from pathlib import Path
        from unittest.mock import patch
        with patch("builtins.print", side_effect=BrokenPipeError("closed log")):
            emit_failure_evidence(Path("/missing-gui-evidence"), RuntimeError("original failure"))


if __name__ == '__main__':
    unittest.main()
