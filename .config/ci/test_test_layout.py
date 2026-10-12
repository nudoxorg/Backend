#!/usr/bin/env python3
"""Contract tests for test_layout.py; run with `python3 test_test_layout.py`."""

import tempfile
import unittest
from pathlib import Path

import test_layout


def write(root: Path, relative: str, text: str = "") -> Path:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return path


class LayoutTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.root = Path(self.scratch.name).resolve()

    def tearDown(self):
        self.scratch.cleanup()

    def problems(self, targets):
        return test_layout.check_package(self.root, {(self.root / t).resolve() for t in targets})

    def test_unit_module_in_discovered_tests_root_is_rejected(self):
        # The canonical breakage: lib.rs -> tests.rs -> tests/name_ranking.rs.
        write(self.root, "lib.rs", "#[cfg(test)]\nmod tests;\n")
        write(self.root, "tests.rs", "mod name_ranking;\n")
        write(self.root, "tests/name_ranking.rs", "use super::*;\n")
        problems = self.problems(["tests/name_ranking.rs"])
        self.assertEqual(len(problems), 1)
        self.assertIn("tests/name_ranking/mod.rs", problems[0])

    def test_mod_rs_layout_is_accepted(self):
        write(self.root, "lib.rs", "mod tests;\n")
        write(self.root, "tests.rs", "mod name_ranking;\n")
        write(self.root, "tests/name_ranking/mod.rs", "use super::*;\n")
        self.assertEqual(self.problems([]), [])

    def test_real_integration_tests_and_their_support_modules_are_accepted(self):
        write(self.root, "lib.rs", "pub fn f() {}\n")
        write(self.root, "tests/journey.rs", "mod support;\n")
        write(self.root, "tests/support.rs", "")
        self.assertEqual(self.problems(["tests/journey.rs"]), [])

    def test_path_attribute_into_tests_root_is_rejected(self):
        write(self.root, "src/lib.rs", '#[cfg(test)]\n#[path = "../tests/shared.rs"]\nmod shared;\n')
        write(self.root, "tests/shared.rs", "")
        self.assertEqual(len(self.problems(["tests/shared.rs"])), 1)

    def test_inline_module_is_ignored(self):
        write(self.root, "lib.rs", "mod tests { use super::*; }\n")
        write(self.root, "tests/tests.rs", "")
        self.assertEqual(self.problems(["tests/tests.rs"]), [])

    def test_bench_root_is_covered(self):
        write(self.root, "lib.rs", "mod benches;\n")
        write(self.root, "benches.rs", "mod sync;\n")
        write(self.root, "benches/sync.rs", "")
        self.assertEqual(len(self.problems(["benches/sync.rs"])), 1)

    def test_visibility_and_raw_identifiers(self):
        self.assertEqual(test_layout.declarations("pub(crate) mod r#match;\n"), [("match", None)])


if __name__ == "__main__":
    unittest.main()
