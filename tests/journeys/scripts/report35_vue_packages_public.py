#!/usr/bin/env python3
"""Strict development-mode CLI/MCP Vue and installed-metadata journey.

Uses private synthetic files, a real owner and cold restart. No package code runs.
The binaries must be built from the supplied commit; their hashes are recorded,
not independently tied to that commit. NuGet discovery requires a separate
configured loopback-provider public journey and is not claimed here.
"""
import argparse
import hashlib
import json
import pathlib
import sys

from csharp_public_route import Journey
from csharp_aspnet_acceptance import coverage_word, declarations, index_published, spans


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--bin-dir', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--source-commit', required=True)
    args = parser.parse_args()
    output = pathlib.Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    checks = []
    journey = Journey(args.bin_dir, output, label='vue-packages')

    def require(name, condition, detail=''):
        checks.append({'name': name, 'pass': bool(condition), 'detail': str(detail)[:500]})
        if not condition:
            raise AssertionError(f'{name}: {detail}')

    def mcp(name, arguments, label):
        result = journey.tool(name, dict(arguments, detail='full'))
        (output / f'{label}.json').write_text(json.dumps(result, indent=2))
        require(label + ' MCP success', not result.get('isError') and isinstance(result.get('structuredContent'), dict), result)
        return result['structuredContent']

    def verify(phase):
        rc, search, raw = journey.cli(phase + '-search', ['search', 'mounted', '--limit', '100'])
        rows = declarations(search, 'mounted', {'function'}, None)
        rows = [row for row in rows if 'App.vue' in str(row.get('identity', {}))]
        require(phase + ' CLI search', rc == 0 and len(rows) == 1, raw)
        coord = rows[0]['identity']['coordinate']
        result = mcp('backend.search', {'query': 'mounted', 'limit': 100}, phase + '-mcp-search')
        require(phase + ' MCP search', coord in json.dumps(result), result)
        rc, _, raw = journey.cli(phase + '-source', ['source', coord])
        require(phase + ' CLI source', rc == 0 and "return 'é漢'" in raw and '<template>' not in raw, raw)
        result = mcp('backend.source', {'coordinate': coord}, phase + '-mcp-source')
        require(phase + ' MCP source', "return 'é漢'" in json.dumps(result, ensure_ascii=False), result)
        module = f'{journey.project}::App.vue:1::App'
        rc, _, raw = journey.cli(phase + '-component-source', ['source', module])
        require(phase + ' CLI full component source', rc == 0 and '<template>' in raw and '<script setup' in raw, raw)
        result = mcp('backend.source', {'coordinate': module}, phase + '-mcp-component-source')
        component = json.dumps(result, ensure_ascii=False)
        require(phase + ' MCP full component source', '<template>' in component and '<script setup' in component, result)
        rc, refs, raw = journey.cli(phase + '-refs', ['references', coord])
        observed = spans(refs)
        require(phase + ' CLI references', rc == 0 and len(observed) == 2 and coverage_word(refs) == 'partial', raw)
        for filename, start, end in observed:
            require(phase + ' exact span', filename == 'App.vue' and (journey.project / filename).read_bytes()[start:end] == b'mounted', (filename, start, end))
        result = mcp('backend.references', {'coordinate': coord}, phase + '-mcp-refs')
        require(phase + ' MCP reference parity', spans(result) == observed and coverage_word(result) == 'partial', result)
        purl = 'pkg:npm/vue@3.5.44'
        for command, tool, witnesses in [
            ('package', 'backend.package', ('createApp', 'local installation', 'partial')),
            ('dependencies', 'backend.dependencies', ('@vue/runtime-dom', 'local installation', 'partial')),
            ('package-versions', 'backend.package_versions', ('3.5.44', 'installed')),
        ]:
            rc, _, raw = journey.cli(phase + '-' + command, [command, purl])
            require(phase + ' CLI ' + command, rc == 0 and all(w.lower() in raw.lower() for w in witnesses), raw)
            result = mcp(tool, {'package': purl}, phase + '-mcp-' + command)
            rendered = json.dumps(result, ensure_ascii=False).lower()
            require(phase + ' MCP ' + command, all(w.lower() in rendered for w in witnesses), result)
        return observed

    try:
        project = journey.root / 'project'
        project.mkdir()
        journey.project = project
        package = project / 'frontend/node_modules/vue'
        package.mkdir(parents=True)
        (package / 'package.json').write_text(json.dumps({'name': 'vue', 'version': '3.5.44', 'types': 'index.d.ts', 'engines': {'node': '>=22'}, 'dependencies': {'@vue/runtime-dom': '3.5.44'}}))
        (package / 'index.d.ts').write_text('export declare function createApp(component: unknown): unknown;\n')
        (package / 'README.md').write_text('# Installed Vue API documentation\n')
        (project / 'App.vue').write_bytes('<template><button @click="mounted">Run</button></template>\r\n<script setup lang="ts">\r\n// é漢\r\nfunction mounted() { return \'é漢\'; }\r\nmounted();\r\nfunction wrapper() { mounted(); }\r\nfunction shadow(mounted: () => void) { mounted(); }\r\n</script>\r\n'.encode())
        journey.start_owner(project)
        journey.start_mcp()
        rc, state, raw = index_published(journey, project, 'vue')
        require('index published', rc == 0 and state == 'published', raw)
        warm = verify('warm')
        journey.stop_mcp()
        journey.stop_owner()
        journey.start_owner(project)
        journey.start_mcp()
        cold = verify('cold')
        require('cold spans match warm', cold == warm, (warm, cold))
        (project / 'App.vue').write_text('<script setup>function replacement() { return 2; }</script>')
        rc, state, raw = index_published(journey, project, 'edited')
        require('edit published', rc == 0 and state == 'published', raw)
        rc, result, raw = journey.cli('edited-search', ['search', 'mounted', '--limit', '100'])
        require('edit retires declaration', rc == 0 and not declarations(result, 'mounted', {'function'}, None), raw)
        result = mcp('backend.search', {'query': 'mounted', 'limit': 100}, 'edited-mcp-search')
        require('MCP edit retires declaration', not declarations(result, 'mounted', {'function'}, None), result)
    except Exception as error:
        checks.append({'name': 'journey completed', 'pass': False, 'detail': repr(error)})
    finally:
        journey.close()
    receipt = {'source_commit_claimed': args.source_commit, 'binary_sha256': {name: digest(pathlib.Path(args.bin_dir) / name) for name in ('backend-cli', 'backend-mcp', 'backend-locald')}, 'mode': 'development; account gate not exercised', 'fixture': 'synthetic; package code not executed', 'nuget_public_journey': 'not exercised', 'checks': checks}
    (output / 'receipt.json').write_text(json.dumps(receipt, indent=2))
    print(json.dumps(receipt, indent=2))
    return 0 if checks and all(check['pass'] for check in checks) else 1


if __name__ == '__main__':
    sys.exit(main())
