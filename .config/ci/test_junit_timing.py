"""The timing summary separates build from nextest run time and ranks slow tests."""
from pathlib import Path
import tempfile
import unittest

import junit_timing

MAIN = """<testsuites name="backend-pr" tests="3" failures="1" time="100.5">
  <testsuite name="backend-engine" tests="2">
    <testcase name="slow" classname="backend-engine" time="40.0"/>
    <testcase name="quick" classname="backend-engine" time="0.5"/>
  </testsuite>
  <testsuite name="backend-desktop" tests="1">
    <testcase name="middle" classname="backend-desktop" time="10.0"/>
  </testsuite>
</testsuites>"""
CONTENTION = """<testsuites name="backend-pr" tests="1" time="19.5">
  <testsuite name="backend-store" tests="1">
    <testcase name="concurrent_writers" classname="backend-store" time="19.0"/>
  </testsuite>
</testsuites>"""


class TimingTests(unittest.TestCase):
    def reports(self, directory: str) -> list[Path]:
        paths = []
        for index, body in enumerate([MAIN, CONTENTION]):
            path = Path(directory) / f'{index}.xml'
            path.write_text(body)
            paths.append(path)
        return paths

    def test_build_time_is_step_time_minus_every_nextest_run(self):
        with tempfile.TemporaryDirectory() as directory:
            lines = junit_timing.summarize(self.reports(directory), 'linux', 300.0)
        self.assertIn('CI-TIMING lane=linux step="nextest runs" seconds=120 tests=4 runs=2', lines)
        self.assertIn('CI-TIMING lane=linux step="build and link (step minus nextest runs)" seconds=180', lines)

    def test_slowest_tests_and_binaries_are_ranked_across_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            lines = junit_timing.summarize(self.reports(directory), 'linux', None, top=2)
        tests = lines[lines.index('== 2 slowest tests ==') + 1:][:2]
        self.assertEqual([line.split()[-1] for line in tests], ['slow', 'concurrent_writers'])
        binaries = lines[lines.index('== 2 binaries by summed test time ==') + 1:]
        self.assertEqual([line.split()[-1] for line in binaries], ['backend-engine', 'backend-store'])
        self.assertFalse(any('build and link' in line for line in lines))


if __name__ == '__main__':
    unittest.main()
