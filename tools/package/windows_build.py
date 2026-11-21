#!/usr/bin/env python3
"""Build a source-bound Windows preview with the CSS and SQLite workers.

This development driver produces unsigned binaries and a build receipt. It is
not signing, native QA, or a release candidate. Run on native Windows with the
pinned Rust toolchain and the MSVC build tools on PATH.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

TARGET = 'x86_64-pc-windows-msvc'
PRODUCT = ('backend-desktop', 'backend-cli', 'backend-mcp', 'backend-locald', 'backend-metrics')


def run(args, *, cwd, env, capture=False):
    return subprocess.run(args, cwd=cwd, env=env, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def source_snapshot(source, env):
    def git(*args):
        return run(['git', '-c', 'core.autocrlf=false', *args], cwd=source, env=env, capture=True).strip()
    if git('status', '--porcelain', '--untracked-files=all'):
        raise ValueError('source checkout must be clean, including untracked files')
    head = git('rev-parse', 'HEAD')
    tree = git('rev-parse', 'HEAD^{tree}')
    if not re.fullmatch(r'[0-9a-f]{40}', head) or not re.fullmatch(r'[0-9a-f]{40}', tree):
        raise ValueError('source revision or tree is invalid')
    for path in ('Cargo.lock', 'frontends/sqlite/Cargo.lock'):
        blob = subprocess.check_output(['git', '-c', 'core.autocrlf=false', 'show', head + ':' + path], cwd=source, env=env)
        if hashlib.sha256(blob).hexdigest() != digest(source / path):
            raise ValueError('checkout lock bytes differ from source; disable Git autocrlf before checkout')
    return {'git_revision': head, 'git_tree': tree, 'cargo_lock_sha256': digest(source / 'Cargo.lock')}


def observe(executable, worker, key, source, env):
    result = json.loads(run([str(executable), str(worker)], cwd=source, env=env, capture=True))
    anchor = result.get(key)
    if not isinstance(anchor, str) or not re.fullmatch(r'[0-9a-f]{64}', anchor) or anchor == '0' * 64:
        raise ValueError('worker observer did not return a valid typed identity')
    return anchor, result


def sqlite_basis(result, lock_digest):
    if set(result) != {'state', 'artifact_identity', 'basis'} or result['state'] != 'observed-build-helper':
        raise ValueError('invalid SQLite observer response')
    basis = result['basis']
    if not isinstance(basis, dict) or set(basis) != {'engine', 'native_revision', 'recipe', 'lock_sha256', 'support_sha256', 'settings'}:
        raise ValueError('invalid SQLite producer basis')
    if (basis['engine'] != 'Syntaqlite4e4de76' or basis['native_revision'] != '4e4de76312429fb2235ae093ab2125914320d5bd'
            or basis['recipe'] != 'syntaqlite-sqlite-fmt-analysis-dqs-default-v1'
            or basis['lock_sha256'] != list(bytes.fromhex(lock_digest))):
        raise ValueError('SQLite producer basis does not match the selected source')
    support = basis['support_sha256']
    settings = basis['settings']
    if (not isinstance(support, list) or len(support) != 32 or not all(type(x) is int and 0 <= x <= 255 for x in support)
            or not any(support) or not isinstance(settings, dict) or set(settings) != {'Sqlite'}
            or not isinstance(settings['Sqlite'], dict) or set(settings['Sqlite']) != {'version', 'cflags', 'analyzer_dqs_fallback'}
            or type(settings['Sqlite']['version']) is not int or settings['Sqlite']['version'] != 2147483647
            or type(settings['Sqlite']['cflags']) is not int or settings['Sqlite']['cflags'] != 0
            or settings['Sqlite']['analyzer_dqs_fallback'] is not True):
        raise ValueError('SQLite producer basis has unsupported native settings')
    encoded = json.dumps(basis, sort_keys=True, separators=(',', ':'), ensure_ascii=True)
    if len(encoded) > 8192:
        raise ValueError('SQLite producer basis exceeds its bound')
    return encoded


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.name != 'nt':
        raise ValueError('this driver requires a native Windows host')
    source = args.source.resolve(strict=True)
    target = args.target_dir.resolve()
    output = args.output.resolve()
    if source == target or source in target.parents or source == output or source in output.parents:
        raise ValueError('target and output must be outside the source checkout')
    if output.exists():
        raise ValueError('output must not already exist')
    env = os.environ.copy()
    for key in list(env):
        if key.startswith(('GIT_', 'NUDOX_BUILD_')) or key in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET'):
            raise ValueError(f'ambient build override is not admitted: {key}')
    env['CARGO_TARGET_DIR'] = str(target)
    env['CARGO_BUILD_JOBS'] = env.get('CARGO_BUILD_JOBS', '2')
    env['GIT_CONFIG_NOSYSTEM'] = '1'
    before = source_snapshot(source, env)
    output.mkdir(parents=True)
    bin_dir = output / 'bin'
    bin_dir.mkdir()
    cargo = shutil.which('cargo')
    if not cargo:
        raise ValueError('cargo is unavailable')
    base = [cargo, '+1.97.1', 'build', '--release', '--locked', '--target', TARGET]
    run(base + ['-p', 'backend-frontend-stylo', '--bin', 'backend-stylo-worker'], cwd=source, env=env)
    run(base + ['-p', 'backend-compile', '--example', 'macos_stylo_identity_observer'], cwd=source, env=env)
    run(base + ['--manifest-path', str(source / 'frontends/sqlite/Cargo.toml'),
                '--bin', 'sqlite-worker', '--example', 'installed_basis_observer'], cwd=source, env=env)
    release = target / TARGET / 'release'
    stylo = bin_dir / 'backend-stylo-worker.exe'
    sqlite = bin_dir / 'backend-sqlite-worker.exe'
    shutil.copyfile(release / 'backend-stylo-worker.exe', stylo)
    shutil.copyfile(release / 'sqlite-worker.exe', sqlite)
    stylo_anchor, stylo_observation = observe(release / 'examples/macos_stylo_identity_observer.exe', stylo,
                                              'identity', source, env)
    if set(stylo_observation) != {'state', 'identity'} or stylo_observation['state'] != 'observed-helper':
        raise ValueError('invalid Stylo observer response')
    sqlite_anchor, sqlite_observation = observe(release / 'examples/installed_basis_observer.exe', sqlite,
                                                'artifact_identity', source, env)
    basis = sqlite_basis(sqlite_observation, digest(source / 'frontends/sqlite/Cargo.lock'))
    owner_env = env | {'NUDOX_BUILD_STYLO_WORKER_ID': stylo_anchor,
                       'NUDOX_BUILD_SQLITE_WORKER_ID': sqlite_anchor,
                       'NUDOX_BUILD_SQLITE_BASIS_JSON': basis}
    command = base + [item for name in PRODUCT for item in ('-p', name)]
    run(command, cwd=source, env=owner_env)
    for name in PRODUCT:
        shutil.copyfile(release / (name + '.exe'), bin_dir / (name + '.exe'))
    if source_snapshot(source, env) != before:
        raise ValueError('source changed during the build')
    if (observe(release / 'examples/macos_stylo_identity_observer.exe', stylo, 'identity', source, env) != (stylo_anchor, stylo_observation)
            or observe(release / 'examples/installed_basis_observer.exe', sqlite, 'artifact_identity', source, env) != (sqlite_anchor, sqlite_observation)):
        raise ValueError('worker changed after owner compilation')
    files = {p.name: {'bytes': p.stat().st_size, 'sha256': digest(p)} for p in sorted(bin_dir.iterdir())}
    receipt = {'schema': 'nudox.windows-unsigned-preview-build.v1', 'source': before,
               'target': TARGET, 'profile': 'release', 'signed': False, 'workers': {'stylo': stylo_anchor, 'sqlite': sqlite_anchor},
               'sqlite_basis': json.loads(basis), 'executables': files}
    (output / 'build-receipt.json').write_text(json.dumps(receipt, indent=2, sort_keys=True) + '\n')
    print(json.dumps({'output': str(output), 'source': before['git_revision'], 'signed': False}))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f'Windows preview build failed: {error}', file=sys.stderr)
        sys.exit(1)
