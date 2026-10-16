"""A real window/query alone must not be accepted as the GUI source-page proof."""
import unittest
from gui_contract import require_visible
import surface


class GuiAdmissionTests(unittest.TestCase):
    expected = ['itoa', 'pub struct Buffer', 'MaybeUninit']

    def test_search_query_without_opened_source_does_not_pass(self):
        with self.assertRaises(RuntimeError):
            require_visible('itoa Search Buffer No results', self.expected)

    def test_matching_declaration_in_another_project_does_not_pass(self):
        with self.assertRaises(RuntimeError):
            require_visible('other-project pub struct Buffer bytes MaybeUninit', self.expected)

    def test_wrapped_native_source_text_preserves_the_contract(self):
        require_visible('itoa\nlib.rs\n pub struct\n Buffer {\n bytes: [MaybeUninit<u8>; 40]', self.expected)

    def test_oversized_observer_output_is_not_admitted(self):
        with self.assertRaises(RuntimeError):
            require_visible('x' * 32769, self.expected)

    def test_gui_memory_and_cpu_regressions_use_existing_guards(self):
        baseline = {'seconds': {}, 'gui_peak_rss_kib': 100000, 'gui_cpu_seconds': 3}
        measured = {'seconds': {}, 'gui_peak_rss_kib': 2000000, 'gui_cpu_seconds': 12}
        failures = surface.regressions(baseline, measured)
        self.assertTrue(any('gui_peak_rss_kib' in item for item in failures))
        self.assertTrue(any('gui_cpu_seconds' in item for item in failures))


if __name__ == '__main__':
    unittest.main()
