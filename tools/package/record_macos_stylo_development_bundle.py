#!/usr/bin/env python3
"""Build a source-bound, ad hoc macOS development app; never a release receipt.

This actor never uses a keychain identity. Both the embedded desktop owner and
the locald companion are compiled with the identity of the final signed worker.
It does not install, launch, notarize, or authenticate the resulting bundle.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import sys
import time
import tomllib

import macos_stylo_companion as companion
import record_embedding_helper_build as common
import sqlite_macos_development as sqlite
from record_macos_stylo_build import copy_exact, reject_private_paths, source_snapshot

SCHEMA = 'nudox.macos-stylo-development-bundle.v1'
WORKER = ['cargo', 'build', '--release', '--locked', '--offline', '-p', 'backend-frontend-stylo', '--bin', 'backend-stylo-worker']
OBSERVER = ['cargo', 'build', '--release', '--locked', '--offline', '-p', 'backend-compile', '--example', 'macos_stylo_identity_observer']
PRODUCT = ['cargo', 'build', '--release', '--locked', '--offline', '-p', 'backend-locald', '--bin', 'backend-locald', '-p', 'backend-desktop', '--bin', 'backend-desktop', '-p', 'backend-mcp', '--bin', 'backend-mcp', '-p', 'backend-cli', '--bin', 'backend-cli']


def require(condition: bool, detail: str) -> None:
    if not condition:
        raise ValueError(detail)


def digest(path: Path, deadline: float) -> dict:
    size, checksum = companion.digest_regular(path, deadline=deadline)
    return {'path': str(path), 'bytes': size, 'sha256': checksum}


def bundle_digest(app: Path, deadline: float) -> dict:
    """Hash the final file set and modes after signing, under finite bounds."""
    records = []
    total = 0
    for path in sorted(app.rglob('*')):
        require(not path.is_symlink(), 'bundle contains an unexpected symlink')
        if path.is_dir():
            continue
        require(len(records) < 1024, 'bundle has too many files')
        size, checksum = companion.digest_regular(path, deadline=deadline)
        total += size
        require(total <= 1024 * 1024 * 1024, 'bundle exceeds byte bound')
        records.append({'path': str(path.relative_to(app)), 'mode': path.stat().st_mode & 0o777,
                        'bytes': size, 'sha256': checksum})
    encoded = common.shared.canonical_json(records)
    return {'algorithm': 'sha256-canonical-file-manifest-v1', 'sha256': hashlib.sha256(encoded).hexdigest(),
            'files': records}


def apple_provenance(deadline: float) -> dict:
    """Observe the selected native SDK and build/signing tools without authentication."""
    tools = {name: companion.digest_regular(Path('/usr/bin') / name, deadline=deadline)
             for name in ('codesign', 'otool', 'lipo', 'xcrun', 'xcode-select', 'git', 'ditto')}
    developer = companion.run_tool(['/usr/bin/xcode-select', '-p'], deadline=deadline).decode().strip()
    sdk_path = Path(companion.run_tool(['/usr/bin/xcrun', '--sdk', 'macosx', '--show-sdk-path'], deadline=deadline).decode().strip())
    sdk_resolved = sdk_path.resolve(strict=True)
    sdk_version = companion.run_tool(['/usr/bin/xcrun', '--sdk', 'macosx', '--show-sdk-version'], deadline=deadline).decode().strip()
    settings = companion.digest_regular((sdk_resolved / 'SDKSettings.json').resolve(strict=True), 1024 * 1024, deadline=deadline)
    selected = {}
    for name in ('clang', 'otool', 'lipo', 'git'):
        path = Path(companion.run_tool(['/usr/bin/xcrun', '--find', name], deadline=deadline).decode().strip()).resolve(strict=True)
        selected[name] = {'path': str(path), 'identity': companion.digest_regular(path, deadline=deadline)}
    return {'apple_tools': tools, 'developer_dir': developer, 'sdk': {'selected': str(sdk_path),
            'resolved': str(sdk_resolved), 'version': sdk_version, 'settings': settings}, 'selected_tools': selected}


def workspace_version(source: Path, deadline: float) -> str:
    manifest_path = source / 'Cargo.toml'
    size, checksum = companion.digest_regular(manifest_path, 1024 * 1024, deadline=deadline)
    descriptor = os.open(manifest_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        raw = stream.read(1024 * 1024 + 1)
    require(len(raw) == size and hashlib.sha256(raw).hexdigest() == checksum,
            'workspace manifest changed during read')
    version = tomllib.loads(raw.decode('utf-8'))['workspace']['package']['version']
    require(isinstance(version, str) and re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?', version) is not None,
            'invalid workspace package version')
    return version


def capture(args: argparse.Namespace) -> Path:
    require(sys.platform == 'darwin', 'development bundle requires macOS')
    require(re.fullmatch(r'[0-9a-f]{40}', args.commit) is not None, 'invalid expected commit')
    require(1 <= args.jobs <= 2 and 0 < args.timeout_seconds <= 7200, 'invalid development build limits')
    deadline = time.monotonic() + args.timeout_seconds
    source = source_snapshot(args.source_checkout)
    require(source['path'] == common.ROOT and source['clean'] and source['commit'] == args.commit,
            'source must be this exact clean checkout')
    host = platform.machine().lower()
    host = 'arm64' if host == 'aarch64' else host
    require(companion.TARGET_ARCH.get(args.target) == host, 'target must match this native host')
    for name in ('cargo_home', 'target_dir', 'build_dir'):
        setattr(args, name, common.directory(getattr(args, name)))
    out = common.runtime.output_path(args.output_dir, 'macOS development bundle')
    require(out.resolve(strict=False) == out, 'output path must be canonical')
    paths = [source['path'], args.cargo_home, args.target_dir, args.build_dir, out]
    require(len(set(paths)) == len(paths), 'build and output paths must be distinct')
    require(all(source['path'] not in p.parents and p != source['path'] for p in paths[1:]),
            'build and output must be outside source')
    require(all(out not in p.parents and p not in out.parents for p in paths[1:-1]),
            'output overlaps a build directory')
    require(all(not any(c.isspace() or c in '=\x1f' for c in str(p)) for p in paths[:4]), 'invalid remap path')
    tools = {name: common.tool(args, name) for name in ('cargo', 'rustc')}
    apple = apple_provenance(deadline)
    actor_path = source['path'] / 'tools/package/record_macos_stylo_development_bundle.py'
    companion_path = source['path'] / 'tools/package/macos_stylo_companion.py'
    sqlite_path = source['path'] / 'tools/package/sqlite_macos_development.py'
    actor_hash = hashlib.sha256(actor_path.read_bytes()).hexdigest()
    companion_hash = hashlib.sha256(companion_path.read_bytes()).hexdigest()
    sqlite_hash = hashlib.sha256(sqlite_path.read_bytes()).hexdigest()
    build_path = str(args.cargo.parent) + ':/usr/bin:/bin:/usr/sbin:/sbin'
    require(shutil.which('cargo', path=build_path) == str(args.cargo), 'Cargo path mismatch')
    build_helpers = {}
    for name in ('cc', 'make', 'python3', 'sh'):
        selected = shutil.which(name, path=build_path)
        require(selected is not None, f'build PATH does not resolve {name}')
        build_helpers[name] = common.observe_build_tool(selected, 'build helper ' + name)
    private = paths[:4]
    env = {'PATH': build_path, 'HOME': str(args.build_dir), 'TMPDIR': str(args.build_dir),
           'LANG': 'C', 'LC_ALL': 'C', 'TZ': 'UTC', 'RUSTC': str(args.rustc),
           'CARGO_HOME': str(args.cargo_home), 'CARGO_TARGET_DIR': str(args.target_dir),
           'CARGO_BUILD_BUILD_DIR': str(args.build_dir), 'CARGO_BUILD_TARGET': args.target,
           'CARGO_NET_OFFLINE': 'true', 'CARGO_BUILD_JOBS': str(args.jobs), 'CARGO_INCREMENTAL': '0',
           'CARGO_CACHE_AUTO_CLEAN_FREQUENCY': 'never', 'CARGO_TERM_COLOR': 'never',
           'RUSTC_WRAPPER': '', 'RUSTC_WORKSPACE_WRAPPER': '', 'RUSTFLAGS': '',
           'CARGO_ENCODED_RUSTFLAGS': '\x1f'.join(f'--remap-path-prefix={p}=/nudox/{label}' for p, label in zip(private, ('source', 'cargo-home', 'target', 'build')))}
    env['CFLAGS'] = env['CXXFLAGS'] = ' '.join(f'-{kind}-prefix-map={p}=/nudox/{label}' for p, label in zip(private, ('source', 'cargo-home', 'target', 'build')) for kind in ('ffile', 'fmacro'))
    out.mkdir(mode=0o700)
    try:
        executions = {}
        for label, command in (('worker', WORKER), ('observer', OBSERVER)):
            logs = out / (label + '-build')
            logs.mkdir(mode=0o700)
            execution = common.execute(command, source['path'], env, logs, deadline, 64 * 1024 * 1024)
            require(execution['exit'] == 0 and execution['process_group_retired'], f'{label} build failed')
            executions[label] = execution
        release = args.target_dir / args.target / 'release'
        app = out / 'Nudox Development.app'
        macos = app / 'Contents/MacOS'
        resources = app / 'Contents/Resources'
        macos.mkdir(parents=True, mode=0o700)
        resources.mkdir(mode=0o700)
        worker = macos / 'backend-stylo-worker'
        copy_exact(release / 'backend-stylo-worker', worker, deadline)
        companion.run_tool(['/usr/bin/codesign', '--force', '--sign', '-', str(worker)], timeout=180, deadline=deadline)
        companion.run_tool(['/usr/bin/codesign', '--verify', '--strict', str(worker)], deadline=deadline)
        worker_image = companion.inspect_system_executable(worker, args.target, deadline=deadline)
        reject_private_paths(worker, private, deadline)
        observer = release / 'examples/macos_stylo_identity_observer'
        observer_image = companion.inspect_system_executable(observer, args.target, deadline=deadline)
        require(digest(observer, deadline)['sha256'] == observer_image['sha256'], 'observer changed before use')
        observed = json.loads(companion.run_tool([str(observer), str(worker)], deadline=deadline))
        anchor = observed.get('identity', '')
        require(observed.get('state') == 'observed-helper' and re.fullmatch(r'[0-9a-f]{64}', anchor) is not None and anchor != '0' * 64,
                'worker observer returned an invalid identity')
        sqlite_record = sqlite.prepare(source=source['path'], macos=macos, target=args.target_dir, build=args.build_dir,
            env=env, target_triple=args.target, deadline=deadline, logs=out/'sqlite-worker-build', companion=companion,
            copy_exact=copy_exact, reject_private_paths=reject_private_paths, private_paths=private)
        anchored = {**env, 'NUDOX_BUILD_STYLO_WORKER_ID': anchor, **sqlite_record['owner_environment']}
        logs = out / 'product-build'
        logs.mkdir(mode=0o700)
        execution = common.execute(PRODUCT, source['path'], anchored, logs, deadline, 64 * 1024 * 1024)
        require(execution['exit'] == 0 and execution['process_group_retired'], 'anchored product build failed')
        executions['product'] = execution
        for build_name, bundle_name in (('backend-desktop', 'Nudox'), ('backend-locald', 'backend-locald'), ('backend-mcp', 'backend-mcp'), ('backend-cli', 'backend-cli')):
            copy_exact(release / build_name, macos / bundle_name, deadline)
        for name in ('backend-locald', 'backend-mcp', 'backend-cli'):
            companion.run_tool(['/usr/bin/codesign', '--force', '--sign', '-', str(macos / name)], timeout=180, deadline=deadline)
        plist_source = source['path'] / 'apps/desktop/macos/Info.plist'
        plist_size, plist_hash = companion.digest_regular(plist_source, 1024 * 1024, deadline=deadline)
        descriptor = os.open(plist_source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, 'rb') as stream:
            plist_bytes = stream.read(1024 * 1024 + 1)
        require(len(plist_bytes) == plist_size and hashlib.sha256(plist_bytes).hexdigest() == plist_hash,
                'source plist changed during read')
        plist = plistlib.loads(plist_bytes)
        require(isinstance(plist, dict) and plist.get('CFBundleExecutable') == 'Nudox', 'unexpected desktop executable')
        version = workspace_version(source['path'], deadline)
        plist['CFBundleIdentifier'] = 'dev.nudox.support.development'
        plist['CFBundleName'] = plist['CFBundleDisplayName'] = 'Nudox Development'
        plist['CFBundleShortVersionString'] = plist['CFBundleVersion'] = re.split(r'[-+]', version, maxsplit=1)[0]
        plist_target = app / 'Contents/Info.plist'
        with plist_target.open('xb') as f:
            plistlib.dump(plist, f)
        plist_target.chmod(0o600)
        icon_target = resources / 'Nudox.icns'
        copy_exact(source['path'] / 'apps/desktop/macos/Nudox.icns', icon_target, deadline)
        icon_target.chmod(0o600)
        notices = resources / 'Stylo Licenses'
        notices.mkdir(mode=0o700)
        for directory, name in (('stylo-native', 'stylo-native.LICENSE'), ('selectors-native', 'selectors-native.LICENSE')):
            destination = notices / name
            copy_exact(source['path'] / 'vendor' / directory / 'LICENSE', destination, deadline)
            destination.chmod(0o600)
        for relative, name in (('LICENSE', 'syntaqlite.LICENSE'), ('syntaqlite-common/LICENSE', 'syntaqlite-common.LICENSE'),
                               ('syntaqlite-syntax/LICENSE', 'syntaqlite-syntax.LICENSE'), ('syntaqlite/LICENSE', 'syntaqlite-crate.LICENSE')):
            destination = notices / name
            copy_exact(source['path'] / 'vendor/syntaqlite-native' / relative, destination, deadline)
            destination.chmod(0o600)
        # Outer signing signs the main desktop image; never recursively sign the worker.
        companion.run_tool(['/usr/bin/codesign', '--force', '--sign', '-', str(app)], timeout=180, deadline=deadline)
        for image in (app, macos / 'backend-locald', macos / 'backend-mcp', macos / 'backend-cli', worker, macos / 'backend-sqlite-worker'):
            companion.run_tool(['/usr/bin/codesign', '--verify', '--strict', str(image)], deadline=deadline)
        require(companion.inspect_system_executable(worker, args.target, deadline=deadline) == worker_image,
                'outer signing changed the admitted worker')
        require(digest(observer, deadline)['sha256'] == observer_image['sha256'], 'observer changed before recheck')
        require(json.loads(companion.run_tool([str(observer), str(worker)], deadline=deadline)) == observed,
                'worker identity changed after outer signing')
        require(companion.inspect_system_executable(observer, args.target, deadline=deadline) == observer_image,
                'observer changed during product build')
        sqlite.verify_after_signing(sqlite_record, source=source['path'], target_triple=args.target,
                                    deadline=deadline, companion=companion)
        executable_names = ('Nudox', 'backend-locald', 'backend-mcp', 'backend-cli', 'backend-stylo-worker', 'backend-sqlite-worker')
        images = {name: companion.inspect_system_executable(macos / name, args.target, deadline=deadline)
                  for name in executable_names}
        for image in (macos / name for name in executable_names):
            reject_private_paths(image, private, deadline)
        require(workspace_version(source['path'], deadline) == version, 'workspace version changed during build')
        cli = macos / 'backend-cli'
        cli_version = companion.run_tool([str(cli), '--version'], maximum=65536, deadline=deadline)
        cli_help = companion.run_tool([str(cli), '--help'], maximum=1024 * 1024, deadline=deadline)
        require(cli_version.decode('utf-8').strip() == 'backend ' + version and bool(cli_help.strip()),
                'packaged CLI version or help mismatch')
        cli_checks = {'workspace_version': version, 'version': {'bytes': len(cli_version), 'sha256': hashlib.sha256(cli_version).hexdigest()},
                      'help': {'bytes': len(cli_help), 'sha256': hashlib.sha256(cli_help).hexdigest()},
                      'scope': 'clean-environment version/help only; no account or query claim'}
        signed_bundle = bundle_digest(app, deadline)
        archive = out / 'Nudox Development.zip'
        companion.run_tool(['/usr/bin/ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', str(app), str(archive)],
                           timeout=600, deadline=deadline)
        archive_size, archive_hash = companion.digest_regular(archive, 1024 * 1024 * 1024, deadline=deadline)
        require(bundle_digest(app, deadline) == signed_bundle, 'bundle changed during archive creation')
        require(source_snapshot(args.source_checkout) == source, 'source changed during build')
        require({name: common.tool(args, name) for name in tools} == tools, 'toolchain changed during build')
        require(apple_provenance(deadline) == apple, 'selected Apple build or signing tools changed')
        require(shutil.which('cargo', path=build_path) == str(args.cargo), 'Cargo path changed')
        for name, identity in build_helpers.items():
            selected = shutil.which(name, path=build_path)
            require(selected is not None and common.observe_build_tool(selected, 'build helper ' + name) == identity,
                    'selected build helper changed')
        require(hashlib.sha256(actor_path.read_bytes()).hexdigest() == actor_hash and
                hashlib.sha256(companion_path.read_bytes()).hexdigest() == companion_hash and
                hashlib.sha256(sqlite_path.read_bytes()).hexdigest() == sqlite_hash,
                'bundle actor or companion checker changed')
        require(time.monotonic() < deadline, 'development build deadline expired')
        records = {name: digest(macos / name, deadline) for name in executable_names}
        require(all(records[name]['sha256'] == images[name]['sha256'] and records[name]['bytes'] == images[name]['bytes']
                    for name in executable_names), 'final executable changed after loader inspection')
        receipt = {'schema': SCHEMA, 'scope': 'ad hoc local development; not Developer ID, notarized, or distributable; build inputs observed before and after, not immutable during execution; platform loader and system dylibs remain outside the artifact identity',
                   'source': {'commit': source['commit'], 'tree': source['tree'], 'cargo_lock_sha256': source['cargo_lock_sha256']},
                   'worker_anchor': anchor, 'worker_image': worker_image, 'observer_image': observer_image,
                   'sqlite_worker': sqlite_record,
                   'outputs': records, 'native_images': images,
                   'resources': {name: digest(path, deadline) for name, path in [('Info.plist', app / 'Contents/Info.plist'), ('Nudox.icns', resources / 'Nudox.icns')]},
                   'bundle_digest': signed_bundle,
                   'archive': {'path': str(archive), 'bytes': archive_size, 'sha256': archive_hash},
                   'cli_checks': cli_checks,
                   'commands': {'worker': WORKER, 'observer': OBSERVER, 'product': PRODUCT},
                   'executions': executions, 'environment': env, 'product_environment': anchored, 'toolchain': tools,
                   'apple_provenance': apple, 'build_helpers': build_helpers,
                   'actor_sha256': actor_hash, 'companion_sha256': companion_hash, 'sqlite_phase_sha256': sqlite_hash,
                   'outer_signing': ['/usr/bin/codesign', '--force', '--sign', '-', str(app)], 'exit': 0}
        final = out / 'development-bundle-receipt.json'
        common.runtime.write_new_json(final, receipt)
        return final
    except Exception as error:
        common.runtime.write_new_json(out / 'FAILED.json', {'status': 'failed', 'error': type(error).__name__, 'detail': str(error)[:2048]})
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source-checkout', 'cargo', 'rustc', 'cargo-home', 'target-dir', 'build-dir', 'output-dir'):
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ('cargo', 'rustc'):
        parser.add_argument('--' + name + '-sha256', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--target', required=True, choices=tuple(companion.TARGET_ARCH))
    parser.add_argument('--jobs', type=int, default=2)
    parser.add_argument('--timeout-seconds', type=float, default=7200)
    try:
        print(capture(parser.parse_args()))
        return 0
    except (ValueError, OSError) as error:
        print(f'development bundle refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
