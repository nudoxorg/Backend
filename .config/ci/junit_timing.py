"""Summarizes nextest JUnit timing for one CI step; observational, never a verdict."""
import argparse
from collections import defaultdict
from pathlib import Path
import sys
import xml.etree.ElementTree as ET


def summarize(paths: list[Path], lane: str, step_seconds: float | None, top: int = 20) -> list[str]:
    run_seconds = 0.0
    tests = 0
    cases = []
    for path in paths:
        root = ET.parse(path).getroot()
        run_seconds += float(root.get('time', 0))
        tests += int(root.get('tests', 0))
        for suite in root.iter('testsuite'):
            for case in suite.iter('testcase'):
                binary = case.get('classname') or suite.get('name', '')
                cases.append((float(case.get('time', 0)), binary, case.get('name', '')))
    lines = [f'CI-TIMING lane={lane} step="nextest runs" seconds={round(run_seconds)} tests={tests} runs={len(paths)}']
    if step_seconds is not None:
        build = max(step_seconds - run_seconds, 0)
        lines.append(f'CI-TIMING lane={lane} step="build and link (step minus nextest runs)" seconds={round(build)}')
    by_binary = defaultdict(float)
    for seconds, binary, _ in cases:
        by_binary[binary] += seconds
    lines.append(f'== {top} slowest tests ==')
    lines += [f'{seconds:9.2f}s  {binary} {name}' for seconds, binary, name in sorted(cases, reverse=True)[:top]]
    lines.append(f'== {top} binaries by summed test time ==')
    lines += [f'{seconds:9.2f}s  {binary}' for binary, seconds in sorted(by_binary.items(), key=lambda item: item[1], reverse=True)[:top]]
    return lines


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--lane', required=True)
    parser.add_argument('--step-seconds', type=float)
    parser.add_argument('junit', nargs='*', type=Path)
    args = parser.parse_args()
    if not args.junit:
        print(f'CI-TIMING lane={args.lane} step="nextest runs" seconds=0 tests=0 runs=0 (no JUnit report)')
        return 0
    print('\n'.join(summarize(args.junit, args.lane, args.step_seconds)))
    return 0


if __name__ == '__main__':
    sys.exit(main())
