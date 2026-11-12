#!/usr/bin/env python3
"""Strict development-mode CLI/MCP Vue and installed-metadata journey.

Uses private synthetic files, a real owner and cold restart. No package code runs.
A verified Root Cargo build receipt binds the binaries to the exact checkout. NuGet discovery requires a separate
configured loopback-provider public journey and is not claimed here.
"""
import argparse
import hashlib
import json
import pathlib
import os
import shutil
import signal
import time
import subprocess
import sys

from csharp_public_route import Journey
from runtime_build_receipt import verify_runtime_build_receipt
from csharp_aspnet_acceptance import coverage_word, declarations, index_published, spans


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class StrictJourney(Journey):
    """Retire only process groups started by this journey, and report failures."""
    def _stop(self, proc):
        if proc is None:
            return
        if proc.stdin:
            try:
                proc.stdin.close()
            except OSError:
                pass
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            proc.poll()
            try:
                os.killpg(proc.pid, 0)
            except ProcessLookupError:
                break
            except PermissionError:
                pass  # A terminating group may temporarily be unsignalable.
            time.sleep(.05)
        else:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        proc.wait(timeout=10)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                os.killpg(proc.pid, 0)
            except ProcessLookupError:
                self.records.append({'retired_process_group': proc.pid, 'exit': proc.returncode})
                return
            except PermissionError:
                pass
            time.sleep(.05)
        raise RuntimeError(f'owned process group {proc.pid} survived cleanup')

    def rpc(self, method, params, allow_error=False, timeout=300):
        number = self.seq + 1
        path = self.out / f'{self.label}-mcp-{self.counter}-rpc-{number}.json'
        observation = {'method': method, 'params': params}
        try:
            result = super().rpc(method, params, allow_error=allow_error, timeout=timeout)
            observation['result'] = result
            return result
        except Exception as error:
            observation['error'] = repr(error)
            raise
        finally:
            path.write_text(json.dumps(observation, ensure_ascii=False, indent=2))

    def cli(self, label, args, project=None, timeout=900):
        project = project or self.project
        cmd = [str(self.bin / 'backend-cli'), '--format', 'json', '--project', str(project),
               '--workspace', str(self.workspace), '--endpoint', str(self.endpoint), *args]
        for attempt in range(6):
            proc = subprocess.Popen(cmd, cwd=project, env=self.env, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, start_new_session=True)
            timed_out = False
            try:
                out, err = proc.communicate(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                self._stop(proc)
                out, err = proc.communicate()
            except BaseException:
                self._stop(proc)
                raise
            else:
                self._stop(proc)
            raw, stderr = out.decode('utf-8', 'replace'), err.decode('utf-8', 'replace')
            (self.out / f'{self.label}-{label}-attempt-{attempt + 1}.stdout').write_text(raw)
            (self.out / f'{self.label}-{label}-attempt-{attempt + 1}.stderr').write_text(stderr)
            if timed_out:
                raise TimeoutError(f'CLI {label} timed out; output preserved')
            try:
                obj = json.loads(raw)
            except ValueError:
                obj = None
            self.records.append({'label': label, 'attempt': attempt + 1,
                                 'argv': cmd[1:], 'exit': proc.returncode})
            repeatable = (proc.returncode != 0 and isinstance(obj, dict)
                          and obj.get('slug') == 'endpoint' and obj.get('cause') == 'unreachable'
                          and 'can be repeated' in str(obj.get('detail', '')))
            if not repeatable:
                break
            time.sleep(10)
        return proc.returncode, obj, raw + '\n' + stderr

    def close(self, preserve_fixture):
        errors = []
        for attribute, stop in (('mcp', self.stop_mcp), ('owner', self.stop_owner)):
            try:
                if getattr(self, attribute, None) is not None:
                    stop()
            except Exception as error:
                errors.append(repr(error))
        if not preserve_fixture and not errors:
            try:
                shutil.rmtree(self.root)
            except Exception as error:
                errors.append(repr(error))
        return errors


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--bin-dir', required=True)
    parser.add_argument('--source-root', type=pathlib.Path, default=pathlib.Path(__file__).resolve().parents[3])
    parser.add_argument('--output', required=True)
    parser.add_argument('--source-commit', required=True)
    parser.add_argument('--build-receipt', required=True, type=pathlib.Path)
    args = parser.parse_args()
    repo = args.source_root.resolve(strict=True)
    git = lambda *cmd: subprocess.check_output(['git', '-C', str(repo), *cmd], text=True).strip()
    head = git('rev-parse', 'HEAD')
    if head != args.source_commit or git('status', '--porcelain'):
        raise SystemExit('source checkout must be clean and match --source-commit exactly')
    tree = git('rev-parse', 'HEAD^{tree}')
    build = verify_runtime_build_receipt(args.build_receipt, repo)
    binary_dir = pathlib.Path(args.bin_dir).resolve(strict=True)
    for name, artifact in build['artifacts'].items():
        if (binary_dir / name).resolve(strict=True) != pathlib.Path(artifact['path']):
            raise SystemExit(f'{name} is not the verified build receipt artifact')
    output = pathlib.Path(args.output).resolve()
    if output == repo or repo in output.parents:
        raise SystemExit('output must be outside the source checkout')
    output.mkdir(parents=True, exist_ok=False)
    checks = []
    journey = StrictJourney.__new__(StrictJourney)
    driver_path = pathlib.Path(__file__).resolve(strict=True)
    driver_before = digest(driver_path)

    def require(name, condition, detail=''):
        checks.append({'name': name, 'pass': bool(condition), 'detail': str(detail)[:500]})
        if not condition:
            raise AssertionError(f'{name}: {detail}')

    def mcp(name, arguments, label):
        for attempt in range(1, 7):
            result = journey.tool(name, dict(arguments, detail='full'))
            (output / f'{label}-attempt-{attempt}.json').write_text(json.dumps(result, indent=2))
            content = result.get('structuredContent')
            repeatable = (result.get('isError') is True and isinstance(content, dict)
                          and content.get('slug') == 'endpoint'
                          and content.get('cause') == 'unreachable'
                          and 're-run the same command' in str(content.get('shell', '')))
            if not repeatable or attempt == 6:
                break
            time.sleep(10)
        require(label + ' MCP success', ('isError' not in result or result['isError'] is False) and isinstance(content, dict) and content.get('answer') != 'fault', result)
        return content

    def require_full_search(label, reply, query):
        require(label + ' typed full lexical search',
                isinstance(reply, dict) and reply.get('answer') == 'records'
                and reply.get('query') == query and isinstance(reply.get('records'), list)
                and reply.get('search_scope') != 'published_names_only'
                and reply.get('full_search') != 'preparing', reply)

    def verify(phase):
        rc, search, raw = journey.cli(phase + '-search', ['search', 'mounted', '--limit', '100'])
        require_full_search(phase + ' CLI', search, 'mounted')
        rows = declarations(search, 'mounted', {'function'}, None)
        rows = [row for row in rows if 'App.vue' in str(row.get('identity', {}))]
        require(phase + ' CLI search', rc == 0 and len(rows) == 1, raw)
        coord = rows[0]['identity']['coordinate']
        result = mcp('backend.search', {'query': 'mounted', 'limit': 100}, phase + '-mcp-search')
        require_full_search(phase + ' MCP', result, 'mounted')
        require(phase + ' MCP search', coord in json.dumps(result), result)
        rc, _, raw = journey.cli(phase + '-source', ['source', coord])
        require(phase + ' CLI source', rc == 0 and "return 'é漢'" in raw and '<template>' not in raw, raw)
        result = mcp('backend.source', {'coordinate': coord}, phase + '-mcp-source')
        require(phase + ' MCP source', "return 'é漢'" in json.dumps(result, ensure_ascii=False), result)
        rc, modules, raw = journey.cli(phase + '-component-search', ['search', 'App.vue', '--limit', '100'])
        module_rows = [r for r in (modules or {}).get('records', [])
                       if str(r.get('identity', {}).get('coordinate', '')).endswith('::App.vue')
                       and r.get('kind') == 'module']
        require(phase + ' published component module', rc == 0 and len(module_rows) == 1, raw)
        module = module_rows[0]['identity']['coordinate']
        require(phase + ' component path identity', module == f'{journey.project}::App.vue', module)
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
            require(phase + ' CLI confinement ' + command, 'PRIVATE_SYMLINK_SENTINEL' not in raw, raw)
            result = mcp(tool, {'package': purl}, phase + '-mcp-' + command)
            rendered = json.dumps(result, ensure_ascii=False).lower()
            require(phase + ' MCP ' + command, all(w.lower() in rendered for w in witnesses), result)
            require(phase + ' MCP confinement ' + command, 'private_symlink_sentinel' not in rendered, result)
        return observed

    try:
        StrictJourney.__init__(journey, binary_dir, output, label='vue-packages')
        project = journey.root / 'project'
        project.mkdir()
        journey.project = project
        package = project / 'frontend/node_modules/vue'
        package.mkdir(parents=True)
        (package / 'package.json').write_text(json.dumps({'name': 'vue', 'version': '3.5.44', 'types': 'index.d.ts', 'main': 'leak.js', 'engines': {'node': '>=22'}, 'dependencies': {'@vue/runtime-dom': '3.5.44'}}))
        (package / 'index.d.ts').write_text('export declare function createApp(component: unknown): unknown;\n')
        (package / 'README.md').write_text('# Installed Vue API documentation\n')
        outside = journey.root / 'private.js'
        outside.write_text('PRIVATE_SYMLINK_SENTINEL')
        (package / 'leak.js').symlink_to(outside)
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
        require_full_search('edit CLI', result, 'mounted')
        require('edit retires declaration', rc == 0 and result.get('more') is False and not declarations(result, 'mounted', {'function'}, None), raw)
        result = mcp('backend.search', {'query': 'mounted', 'limit': 100}, 'edited-mcp-search')
        require_full_search('edit MCP', result, 'mounted')
        require('MCP edit retires declaration', result.get('more') is False and not declarations(result, 'mounted', {'function'}, None), result)
    except Exception as error:
        checks.append({'name': 'journey completed', 'pass': False, 'detail': repr(error)})
    finally:
        cleanup_errors = journey.close(preserve_fixture=True)
        checks.append({'name': 'owned process cleanup', 'pass': not cleanup_errors, 'detail': str(cleanup_errors)})
        try:
            after = verify_runtime_build_receipt(args.build_receipt, repo)
            unchanged = (after['receipt_sha256'] == build['receipt_sha256']
                         and after['artifacts'] == build['artifacts']
                         and digest(driver_path) == driver_before)
            checks.append({'name': 'build provenance unchanged', 'pass': unchanged, 'detail': ''})
        except Exception as error:
            checks.append({'name': 'build provenance unchanged', 'pass': False, 'detail': repr(error)})
        if all(c['pass'] for c in checks):
            try:
                shutil.rmtree(journey.root)
            except Exception as error:
                checks.append({'name': 'fixture cleanup', 'pass': False, 'detail': repr(error)})
    receipt = {'driver_path': str(driver_path), 'driver_sha256': driver_before, 'source_commit': head, 'source_tree': tree, 'cargo_lock_sha256': digest(repo / 'Cargo.lock'), 'binary_sha256': {name: item['sha256'] for name, item in build['artifacts'].items()}, 'build_receipt': {'path': build['receipt_path'], 'sha256': build['receipt_sha256']}, 'records': getattr(journey, 'records', []), 'fixture_root_if_preserved': str(journey.root) if hasattr(journey, 'root') and journey.root.exists() else None, 'mode': 'development assumed; authenticated account not exercised', 'account_gate_environment_set': 'NUDOX_ACCOUNT_GATE' in getattr(journey, 'env', {}), 'fixture': 'synthetic; package code not executed', 'nuget_public_journey': 'not exercised', 'checks': checks}
    (output / 'receipt.json').write_text(json.dumps(receipt, indent=2))
    print(json.dumps(receipt, indent=2))
    return 0 if checks and all(check['pass'] for check in checks) else 1


if __name__ == '__main__':
    sys.exit(main())
