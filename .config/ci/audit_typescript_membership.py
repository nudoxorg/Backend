#!/usr/bin/env python3
"""Verify the reviewed TypeScript view and diagnostic fact join from captured logs."""
import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / 'tests/surface/typescript-query-core-membership.json'
PREFIX = 'SURFACE-VIEW-ROW typescript-query-core\t'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('replay', type=Path)
    parser.add_argument('owner', type=Path)
    parser.add_argument('--pre-owner', type=Path, help='optional pre-fix diagnostic owner log')
    args = parser.parse_args()
    manifest = json.loads(MANIFEST.read_text())
    rows = [line[len(PREFIX):] for line in args.replay.read_text().splitlines() if line.startswith(PREFIX)]
    assert len(rows) == len(set(rows)) and set(rows) == set(manifest['rows']), 'view membership differs'
    lines = args.owner.read_text().splitlines()
    marker = 'T2-JOIN-BEGIN view=StateRoot:' + manifest['view_root']
    starts = [i for i, line in enumerate(lines) if line.startswith(marker + ' ')]
    assert len(starts) == 1, 'missing or ambiguous view audit'
    start = starts[0]
    assert 'rows=3173 facts=3173 unmatched_rows=0 unmatched_facts=0' in lines[start]
    end = lines.index('T2-JOIN-END', start + 1)
    facts = {}
    for line in lines[start + 1:end]:
        match = re.match(r'T2-JOIN-FACT id="([^"]+)" evidence=(\w+)', line)
        assert match and match[1] not in facts, 'missing or duplicate fact ID'
        facts[match[1]] = (match[2], line)
    assert len(facts) == len(rows)
    external = {}
    for row in rows:
        match = re.match(r'(Symbol|Package)\(ObjectKey:([0-9a-f]{64})\)', row)
        assert match, 'unknown row identity'
        kind, ident = match.groups()
        fact_type, line = facts.pop(kind.lower() + ':' + ident)
        if '::external::' not in row:
            assert fact_type == ('Compiler' if kind == 'Symbol' else 'Package')
            continue
        assert fact_type == 'CompilerExternalTarget'
        assert 'provenance: Captured' in line and 'TypeScriptCompiler' in line
        source = re.search(r'source: SourceIdentity \{ identity: ContentId\(([0-9a-f]{64})\)', line)
        image = re.search(r'image: \[([^]]+)\]', line)
        assert source and image
        external[ident] = {'source_identity': source[1], 'image': bytes(map(int, image[1].split(', '))).hex()}
    assert not facts and external == manifest['external_facts']
    print(f"verified {len(rows)} rows and facts; {len(external)} captured external compiler targets")
    if args.pre_owner:
        pre = manifest['pre_fix_diagnostic']
        lines = args.pre_owner.read_text().splitlines()
        marker = 'T2-JOIN-BEGIN view=StateRoot:' + pre['view_root']
        matches = [line for line in lines if line.startswith(marker + ' ')]
        assert len(matches) == 1 and 'rows=3387 facts=3173 unmatched_rows=214 unmatched_facts=0' in matches[0]
        aliases = {}
        unmatched = set()
        for line in lines:
            if line.startswith('T2-QUERY-ALIAS '):
                match = re.fullmatch(r'T2-QUERY-ALIAS external=(\S+) target=(\S+)', line)
                assert match and (match[1] not in aliases or aliases[match[1]] == match[2])
                aliases[match[1]] = match[2]
            elif line.startswith('T2-JOIN-ROW '):
                match = re.search(r' key="([^"]+)"', line)
                assert match and match[1] not in unmatched
                unmatched.add(match[1])
        assert aliases == pre['aliases'] and set(aliases) == unmatched and len(unmatched) == 214
        owned_ids = {
            'symbol:' + re.match(r'Symbol\(ObjectKey:([0-9a-f]{64})\)', row)[1]
            for row in rows if row.startswith('Symbol(') and '::external::' not in row
        }
        assert set(aliases.values()) <= owned_ids
        print('verified 214 unmatched pre-fix rows against selected-program query aliases on that diagnostic basis')


if __name__ == '__main__':
    main()
