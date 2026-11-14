#!/usr/bin/env python3
"""Build a signed macOS Stylo worker, then a same-source anchored owner.

The worker is signed and checked before its typed identity is embedded in the
owner. This records build custody only; assembly, final app signing,
notarization, and native route acceptance remain separate release steps.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import sys
import subprocess
import time

import macos_stylo_companion as companion
import record_embedding_helper_build as common

SCHEMA = 'nudox.macos-stylo-bound-owner.v1'
WORKER = ['cargo', 'build', '--release', '--locked', '--offline', '-p', 'backend-frontend-stylo', '--bin', 'backend-stylo-worker']
OBSERVER = ['cargo', 'build', '--release', '--locked', '--offline', '-p', 'backend-compile', '--example', 'macos_stylo_identity_observer']
OWNER = ['cargo', 'build', '--release', '--locked', '--offline', '-p', 'backend-locald', '--bin', 'backend-locald']


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def copy_exact(source: Path, destination: Path, deadline: float | None = None) -> dict:
    require(source.is_absolute() and not source.is_symlink(), 'artifact source must be absolute and non-symlink')
    created = False
    try:
        fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as incoming:
            before = os.fstat(incoming.fileno())
            require(stat.S_ISREG(before.st_mode) and before.st_size <= companion.MAX_WORKER_BYTES,
                    'artifact exceeds its regular-file byte bound')
            digest = hashlib.sha256()
            remaining = before.st_size
            with destination.open('xb') as outgoing:
                created = True
                while remaining:
                    require(deadline is None or time.monotonic() < deadline, 'artifact copy deadline expired')
                    block = incoming.read(min(1024 * 1024, remaining))
                    require(bool(block), 'artifact shortened during copy')
                    outgoing.write(block)
                    digest.update(block)
                    remaining -= len(block)
            after = os.fstat(incoming.fileno())
            selected = source.lstat()
            identity = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
            require(identity(before) == identity(after) == identity(selected), 'artifact changed during copy')
        destination.chmod(0o700)
        result = (before.st_size, digest.hexdigest())
        require(companion.digest_regular(destination, deadline=deadline) == result, 'staged bytes changed')
        return {'path': str(destination), 'bytes': result[0], 'sha256': result[1]}
    except Exception:
        if created:
            destination.unlink(missing_ok=True)
        raise


def reject_private_paths(path: Path, private_paths: list[Path], deadline: float | None = None) -> None:
    patterns = [os.fsencode(value) for value in private_paths]
    overlap = max(map(len, patterns), default=1) - 1
    tail = b''
    with path.open('rb') as stream:
        while block := stream.read(1024 * 1024):
            require(deadline is None or time.monotonic() < deadline, 'privacy scan deadline expired')
            chunk = tail + block
            require(not any(pattern in chunk for pattern in patterns), 'artifact contains a private build path')
            tail = chunk[-overlap:] if overlap else b''


def source_snapshot(path: Path) -> dict:
    # Git observations must not inherit executable or toolchain redirection.
    keys = ('PATH', 'DEVELOPER_DIR', 'SDKROOT', 'TOOLCHAINS')
    previous = {key: os.environ.get(key) for key in keys}
    try:
        os.environ['PATH'] = '/usr/bin:/bin:/usr/sbin:/sbin'
        for key in keys[1:]:
            os.environ.pop(key, None)
        return common.clean_source_snapshot(path)
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def capture(args: argparse.Namespace) -> Path:
    require(platform.system() == 'Darwin', 'this actor requires macOS')
    host = platform.machine().lower()
    host = 'arm64' if host == 'aarch64' else host
    require(companion.TARGET_ARCH.get(args.target) == host, 'target must match this native host')
    require(re.fullmatch(r'[A-Z0-9]{10}', args.team_id) is not None, 'invalid signing team')
    require(bool(args.signing_identity) and args.signing_identity != '-' and len(args.signing_identity) <= 256,
            'a non-ad-hoc signing identity is required')
    require(1 <= args.jobs <= 4 and 0 < args.timeout_seconds <= 1800 and 0 < args.max_log_bytes <= 64 * 1024 * 1024, 'invalid finite build bounds')
    deadline = time.monotonic() + args.timeout_seconds
    source = source_snapshot(args.source_checkout)
    require(time.monotonic() < deadline, 'build deadline expired during source inspection')
    require(source['clean'] and source['path'] == common.ROOT, 'source must be this exact clean checkout')
    for name in ('cargo_home', 'target_dir', 'build_dir'):
        setattr(args, name, common.directory(getattr(args, name)))
    output = common.runtime.output_path(args.output_dir, 'macOS Stylo evidence')
    require(output.resolve(strict=False) == output, 'evidence path must be canonical')
    private = [source['path'], args.cargo_home, args.target_dir, args.build_dir]
    require(len(set(private + [output])) == 5 and all(source['path'] not in p.parents and p != source['path'] for p in private[1:] + [output]),
            'build and evidence paths must be distinct and outside source')
    require(all('\x1f' not in str(p) and '=' not in str(p) and not any(ch.isspace() for ch in str(p)) for p in private), 'invalid remap input')
    require(all(output not in p.parents and p not in output.parents for p in private[1:]), 'evidence overlaps a build directory')
    tools = {name: common.tool(args, name) for name in ('cargo', 'rustc')}
    require(time.monotonic() < deadline, 'build deadline expired during tool inspection')
    actor_path = source['path'] / 'tools/package/record_macos_stylo_build.py'
    actor_sha = hashlib.sha256(actor_path.read_bytes()).hexdigest()
    companion_path = source['path'] / 'tools/package/macos_stylo_companion.py'
    companion_sha = hashlib.sha256(companion_path.read_bytes()).hexdigest()
    apple_tools = {name: companion.digest_regular(Path('/usr/bin') / name, deadline=deadline) for name in ('codesign', 'otool', 'lipo', 'xcrun', 'xcode-select', 'git')}
    selected_developer = companion.run_tool(['/usr/bin/xcode-select', '-p'], deadline=deadline).decode('utf-8', 'strict').strip()
    sdk_selected = Path(companion.run_tool(['/usr/bin/xcrun', '--sdk', 'macosx', '--show-sdk-path'], deadline=deadline).decode('utf-8', 'strict').strip())
    sdk_resolved = sdk_selected.resolve(strict=True)
    sdk_version = companion.run_tool(['/usr/bin/xcrun', '--sdk', 'macosx', '--show-sdk-version'], deadline=deadline).decode('utf-8', 'strict').strip()
    sdk_settings = companion.digest_regular((sdk_resolved / 'SDKSettings.json').resolve(strict=True), 1024 * 1024, deadline=deadline)
    selected_tools = {}
    for name in ('clang', 'otool', 'lipo', 'git'):
        selected = Path(companion.run_tool(['/usr/bin/xcrun', '--find', name], deadline=deadline).decode('utf-8', 'strict').strip()).resolve(strict=True)
        selected_tools[name] = {'path': str(selected), 'identity': companion.digest_regular(selected, deadline=deadline)}
    build_path = str(args.cargo.parent) + ':/usr/bin:/bin:/usr/sbin:/sbin'
    require(shutil.which('cargo', path=build_path) == str(args.cargo), 'build PATH does not resolve pinned Cargo')
    build_helpers = {}
    for name in ('cc', 'make', 'python3', 'sh'):
        selected = shutil.which(name, path=build_path)
        require(selected is not None, f'build PATH does not resolve {name}')
        build_helpers[name] = common.observe_build_tool(selected, 'build helper ' + name)
    env = {'PATH': build_path, 'HOME': str(args.build_dir), 'TMPDIR': str(args.build_dir), 'LANG': 'C', 'LC_ALL': 'C', 'TZ': 'UTC',
           'RUSTC': str(args.rustc), 'CARGO_HOME': str(args.cargo_home), 'CARGO_TARGET_DIR': str(args.target_dir),
           'CARGO_BUILD_BUILD_DIR': str(args.build_dir), 'CARGO_BUILD_TARGET': args.target, 'CARGO_NET_OFFLINE': 'true',
           'CARGO_BUILD_JOBS': str(args.jobs), 'CARGO_INCREMENTAL': '0', 'CARGO_CACHE_AUTO_CLEAN_FREQUENCY': 'never',
           'CARGO_TERM_COLOR': 'never', 'RUSTC_WRAPPER': '', 'RUSTC_WORKSPACE_WRAPPER': '', 'RUSTFLAGS': '',
           'CARGO_ENCODED_RUSTFLAGS': '\x1f'.join(f'--remap-path-prefix={p}=/nudox/{label}' for p,label in zip(private, ('source','cargo-home','target','build')))}
    cflags = ' '.join(f'-{kind}-prefix-map={p}=/nudox/{label}' for p,label in zip(private, ('source','cargo-home','target','build')) for kind in ('ffile','fmacro'))
    env['CFLAGS'] = cflags
    env['CXXFLAGS'] = cflags
    output.mkdir(mode=0o700)
    try:
        executions = {}
        for label, argv in (('worker', WORKER), ('observer', OBSERVER)):
            logs = output / (label + '-build')
            logs.mkdir(mode=0o700)
            execution = common.execute(argv, source['path'], env, logs, deadline, args.max_log_bytes)
            require(execution['exit'] == 0 and execution['process_group_retired'], f'{label} build failed')
            executions[label] = execution
        raw_path = output / 'backend-stylo-worker.raw'
        raw_worker = copy_exact(args.target_dir / args.target / 'release/backend-stylo-worker', raw_path, deadline)
        reject_private_paths(raw_path, private, deadline)
        worker = output / 'backend-stylo-worker'
        copy_exact(raw_path, worker, deadline)
        sign_command = ['/usr/bin/codesign', '--force', '--timestamp', '--options', 'runtime', '--sign', args.signing_identity, str(worker)]
        require(time.monotonic() < deadline, 'build deadline expired before signing')
        companion.run_tool(sign_command, timeout=180, deadline=deadline)
        signed_worker = companion.inspect_signed_worker(worker, args.target, args.team_id, deadline=deadline)
        reject_private_paths(worker, private, deadline)
        require(companion.digest_regular(worker, deadline=deadline) == (signed_worker['bytes'], signed_worker['sha256']), 'signed worker changed during privacy scan')
        observer = args.target_dir / args.target / 'release/examples/macos_stylo_identity_observer'
        observer_image = companion.inspect_system_executable(observer, args.target, deadline=deadline)
        observer_size, observer_sha = observer_image['bytes'], observer_image['sha256']
        observed = json.loads(companion.run_tool([str(observer), str(worker)], deadline=deadline))
        anchor = observed.get('identity')
        require(observed.get('state') == 'observed-helper' and isinstance(anchor, str) and re.fullmatch(r'[0-9a-f]{64}', anchor) is not None and anchor != '0' * 64,
                'observer returned an invalid worker identity')
        owner_env = {**env, 'NUDOX_BUILD_STYLO_WORKER_ID': anchor}
        logs = output / 'owner-build'
        logs.mkdir(mode=0o700)
        execution = common.execute(OWNER, source['path'], owner_env, logs, deadline, args.max_log_bytes)
        require(execution['exit'] == 0 and execution['process_group_retired'], 'anchored owner build failed')
        executions['owner'] = execution
        owner = output / 'backend-locald'
        owner_record = copy_exact(args.target_dir / args.target / 'release/backend-locald', owner, deadline)
        reject_private_paths(owner, private, deadline)
        require(companion.digest_regular(owner, deadline=deadline) == (owner_record['bytes'], owner_record['sha256']), 'owner changed during privacy scan')
        require(companion.inspect_signed_worker(worker, args.target, args.team_id, deadline=deadline) == signed_worker, 'signed worker changed during owner build')
        require(companion.digest_regular(observer, deadline=deadline) == (observer_size, observer_sha), 'observer changed during owner build')
        require(source_snapshot(args.source_checkout) == source, 'source changed during build')
        require(hashlib.sha256(actor_path.read_bytes()).hexdigest() == actor_sha and hashlib.sha256(companion_path.read_bytes()).hexdigest() == companion_sha, 'actor or companion checker changed during build')
        require({name: common.tool(args, name) for name in tools} == tools, 'toolchain changed during build')
        require(time.monotonic() < deadline, 'build deadline expired during tool recheck')
        require({name: companion.digest_regular(Path('/usr/bin') / name, deadline=deadline) for name in apple_tools} == apple_tools, 'Apple tool changed during build')
        require(companion.run_tool(['/usr/bin/xcode-select', '-p'], deadline=deadline).decode('utf-8', 'strict').strip() == selected_developer, 'selected Apple developer directory changed')
        require(Path(companion.run_tool(['/usr/bin/xcrun', '--sdk', 'macosx', '--show-sdk-path'], deadline=deadline).decode('utf-8', 'strict').strip()) == sdk_selected and sdk_selected.resolve(strict=True) == sdk_resolved and companion.run_tool(['/usr/bin/xcrun', '--sdk', 'macosx', '--show-sdk-version'], deadline=deadline).decode('utf-8', 'strict').strip() == sdk_version and companion.digest_regular((sdk_resolved / 'SDKSettings.json').resolve(strict=True), 1024 * 1024, deadline=deadline) == sdk_settings, 'selected macOS SDK changed')
        for name, item in selected_tools.items():
            selected = Path(companion.run_tool(['/usr/bin/xcrun', '--find', name], deadline=deadline).decode('utf-8', 'strict').strip()).resolve(strict=True)
            require(str(selected) == item['path'] and companion.digest_regular(selected, deadline=deadline) == item['identity'], 'selected Apple tool changed')
        require(shutil.which('cargo', path=build_path) == str(args.cargo), 'build PATH Cargo changed')
        for name, identity in build_helpers.items():
            selected = shutil.which(name, path=build_path)
            require(selected is not None and common.observe_build_tool(selected, 'build helper ' + name) == identity, 'build helper changed')
        require(time.monotonic() < deadline, 'build deadline expired during helper recheck')
        receipt = {'schema': SCHEMA, 'source': {'commit': source['commit'], 'tree': source['tree'], 'cargo_lock_sha256': source['cargo_lock_sha256'], 'clean_before': True, 'clean_after': True},
                   'target': args.target, 'limits': {'timeout_seconds': args.timeout_seconds, 'max_log_bytes': args.max_log_bytes}, 'actor_sha256': actor_sha, 'companion_sha256': companion_sha, 'commands': {'worker': WORKER, 'observer': OBSERVER, 'owner': OWNER}, 'executions': executions,
                   'environment': env, 'owner_environment': owner_env, 'toolchain': tools, 'apple_tools': apple_tools, 'developer_dir': selected_developer, 'selected_tools': selected_tools, 'build_helpers': build_helpers, 'sdk': {'selected': str(sdk_selected), 'resolved': str(sdk_resolved), 'version': sdk_version, 'settings': sdk_settings},
                   'raw_worker': raw_worker, 'worker': {**signed_worker, 'path': str(worker)}, 'sign_command': sign_command,
                   'observer': {'path': str(observer), 'bytes': observer_size, 'sha256': observer_sha},
                   'worker_anchor': anchor, 'owner': owner_record, 'exit': 0}
        candidate = output / 'candidate.json'
        require(len(common.shared.canonical_json(receipt)) + 1 <= 65536, 'receipt exceeds bound')
        common.runtime.write_new_json(candidate, receipt)
        require(time.monotonic() < deadline, 'build deadline expired before receipt publication')
        final = output / 'macos-stylo-bound-owner-receipt.json'
        os.link(candidate, final)
        candidate.unlink()
        common.sync_directory(output)
        return final
    except Exception as error:
        common.runtime.write_new_json(output / 'FAILED.json', {'status': 'failed', 'error': type(error).__name__, 'detail': str(error)[:2048]})
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source-checkout','cargo','rustc','cargo-home','target-dir','build-dir','output-dir'):
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ('cargo','rustc'):
        parser.add_argument('--' + name + '-sha256', required=True)
    parser.add_argument('--target', required=True, choices=tuple(companion.TARGET_ARCH))
    parser.add_argument('--signing-identity', required=True)
    parser.add_argument('--team-id', required=True)
    parser.add_argument('--jobs', type=int, default=2)
    parser.add_argument('--timeout-seconds', type=float, default=1800)
    parser.add_argument('--max-log-bytes', type=int, default=64*1024*1024)
    try:
        print(capture(parser.parse_args()))
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f'macOS Stylo build refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
