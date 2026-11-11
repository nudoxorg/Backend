"""SQLite Linux worker phase for a source-bound owner build.

The enclosing actor must admit a clean source, toolchain, closed build environment,
loader digest, deadline and private output directory; compile the owner with both
returned constants; then call verify_after_owner before publishing its receipt.
This phase is not an installed-worker admission or a standalone release receipt.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re

import linux_release_package as package
import record_embedding_helper_build as common
import record_stylo_owner_build as elf
import sqlite_macos_development as sqlite

LIBRARY = 'libgcc_s.so.1'
MAX_LIBRARY = 16 * 1024 * 1024
SCHEMA = 'nudox.sqlite-linux-worker-phase.v1'


def prepare(*, source: Path, target: Path, env: dict[str, str], output: Path,
            readelf: Path, patchelf: Path, loader: Path, loader_sha256: str,
            deadline: float, private_paths: list[Path]) -> dict:
    """Build, relocate and independently observe the final paired worker bytes."""
    require = elf.require
    snapshot = common.clean_source_snapshot(source)
    require(snapshot['clean'] and snapshot['path'] == common.ROOT,
            'SQLite source must be this exact clean checkout')
    require(output.is_absolute() and not output.exists(), 'SQLite phase output must be new')
    require(env.get('CARGO_TARGET_DIR') == str(target) and env.get('CARGO_BUILD_TARGET') == common.TARGET,
            'SQLite target does not match build environment')
    require(not any(key.startswith('LD_') or key.startswith('DYLD_') or key.startswith('NUDOX_BUILD_SQLITE_')
                    for key in env), 'SQLite build environment contains loader or preexisting anchor overrides')
    require(output.resolve(strict=False) == output and source not in output.parents and target not in output.parents,
            'SQLite phase output must be canonical and outside source and target')
    require(re.fullmatch(r'[0-9a-f]{64}', loader_sha256) is not None,
            'invalid admitted loader digest')
    admitted_loader = package.require_tool(loader, 'SQLite source loader')
    tools = {name: common.observe_build_tool(str(path), name) for name, path in
             [('readelf', readelf), ('patchelf', patchelf), ('loader', admitted_loader)]}
    require(tools['loader']['sha256'] == loader_sha256, 'SQLite source loader digest mismatch')
    lock = source / 'frontends/sqlite/Cargo.lock'
    lock_size, lock_hash = package.admit_file_digest(lock, 1024 * 1024)
    output.mkdir(mode=0o700)
    logs = output / 'build'; logs.mkdir(mode=0o700)
    command = ['cargo', 'build', '--manifest-path', str(source / 'frontends/sqlite/Cargo.toml'),
               '--release', '--locked', '--offline', '--bin', 'sqlite-worker',
               '--example', 'installed_basis_observer']
    execution = common.execute(command, source, env, logs, deadline, 64 * 1024 * 1024)
    require(execution['exit'] == 0 and execution['process_group_retired'], 'SQLite worker build failed')
    release = target / common.TARGET / 'release'
    raw_worker = release / 'sqlite-worker'
    observer = release / 'examples/installed_basis_observer'
    raw_size, raw_hash = package.admit_file_digest(raw_worker, package.MAX_BINARY_BYTES)
    observer_before = common.artifact_identity(observer)
    worker = output / 'backend-sqlite-worker'
    package.copy_admitted_file(raw_worker, worker, raw_hash, raw_size, package.MAX_BINARY_BYTES)
    worker.chmod(0o700)
    old_run = package.run
    package.run = lambda command: elf.bounded_process(command, deadline, env).decode('utf-8').strip()
    try:
        before = package.elf_info(readelf, worker)
        require(before['interpreter'] and Path(before['interpreter']).resolve(strict=True) == admitted_loader,
                'SQLite worker interpreter differs from admitted loader')
        require(set(before['needed']) - package.GLIBC_SONAMES == {LIBRARY},
                'SQLite worker has unexpected non-platform dependencies')
        linkage = elf.bounded_process([str(admitted_loader), '--list', str(raw_worker)], deadline, env).decode('utf-8')
        matches = [p for name, p in package.ldd_dependencies(linkage) if name == LIBRARY]
        require(len(matches) == 1 and Path(matches[0]).is_absolute(), 'SQLite libgcc selection is ambiguous')
        source_library = Path(matches[0]).resolve(strict=True)
        library_size, library_hash = package.admit_file_digest(source_library, MAX_LIBRARY)
        library = output / LIBRARY
        package.copy_admitted_file(source_library, library, library_hash, library_size, MAX_LIBRARY)
        library.chmod(0o700)
        library_before = package.elf_info(readelf, library)
        dynamic_before = elf.bounded_process([str(readelf), '-d', str(library)], deadline, env).decode('utf-8')
        require(set(library_before['needed']).issubset(package.GLIBC_SONAMES)
                and re.findall(r'\(SONAME\).*\[(.*?)\]', dynamic_before) == [LIBRARY],
                'SQLite libgcc has an unexpected closure or SONAME')
        library_command = [str(patchelf), '--set-rpath', '$ORIGIN', str(library)]
        package.run(library_command)
        library_after = package.elf_info(readelf, library)
        dynamic_after = elf.bounded_process([str(readelf), '-d', str(library)], deadline, env).decode('utf-8')
        require(library_after['needed'] == library_before['needed']
                and re.findall(r'\(SONAME\).*\[(.*?)\]', dynamic_after) == [LIBRARY],
                'SQLite libgcc relocation changed its closure')
        elf.require_runpath(readelf, library, '$ORIGIN', deadline, env)
        worker_command = [str(patchelf), '--set-interpreter', package.INTERPRETER,
                          '--set-rpath', '$ORIGIN/../lib', str(worker)]
        package.run(worker_command)
        after = package.elf_info(readelf, worker)
        require(after['interpreter'] == package.INTERPRETER and after['needed'] == before['needed'],
                'SQLite worker relocation changed its closure')
        elf.require_runpath(readelf, worker, '$ORIGIN/../lib', deadline, env)
    finally:
        package.run = old_run
    worker_identity = common.artifact_identity(worker)
    library_identity = common.artifact_identity(library)
    require(library_identity['bytes'] <= MAX_LIBRARY, 'relocated SQLite library exceeds bound')
    for image in (worker, library):
        package.reject_private_paths(image, [str(p) for p in private_paths], 'SQLite worker closure')
    raw = elf.bounded_process([str(observer), str(worker), str(library)], deadline, env, sqlite.MAX_BASIS_BYTES)
    anchor, basis = sqlite.validate_observation(json.loads(raw), lock_hash)
    require(common.artifact_identity(worker) == worker_identity and common.artifact_identity(library) == library_identity
            and common.artifact_identity(observer) == observer_before, 'SQLite observer inputs changed')
    require(package.admit_file_digest(lock, 1024 * 1024) == (lock_size, lock_hash)
            and package.admit_file_digest(raw_worker, package.MAX_BINARY_BYTES) == (raw_size, raw_hash)
            and package.admit_file_digest(source_library, MAX_LIBRARY) == (library_size, library_hash)
            and common.clean_source_snapshot(source) == snapshot, 'SQLite source or build input changed')
    require({name: common.observe_build_tool(str(path), name) for name, path in
             [('readelf', readelf), ('patchelf', patchelf), ('loader', admitted_loader)]} == tools,
            'SQLite ELF tool changed')
    return {'schema': SCHEMA, 'source': snapshot, 'command': command, 'execution': execution,
            'environment': dict(env), 'sqlite_lock_sha256': lock_hash, 'observer': observer_before,
            'worker_raw': {'path': str(raw_worker), 'bytes': raw_size, 'sha256': raw_hash},
            'worker': worker_identity, 'dependency': library_identity,
            'dependency_source': {'path': str(source_library), 'bytes': library_size, 'sha256': library_hash},
            'tools': tools, 'worker_transformation': {'argv': worker_command, 'before': before, 'after': after},
            'dependency_transformation': {'argv': library_command, 'before': library_before, 'after': library_after},
            'artifact_identity': anchor, 'basis_json': basis,
            'observation_sha256': hashlib.sha256(raw).hexdigest(),
            'owner_environment': {'NUDOX_BUILD_SQLITE_WORKER_ID': anchor,
                                  'NUDOX_BUILD_SQLITE_BASIS_JSON': basis}}


def verify_after_owner(record: dict, *, source: Path, env: dict[str, str], deadline: float) -> None:
    """Recheck custody after the enclosing actor builds its anchored owner."""
    require = elf.require
    require(record['schema'] == SCHEMA and common.clean_source_snapshot(source) == record['source'],
            'SQLite source changed during owner build')
    lock = source / 'frontends/sqlite/Cargo.lock'
    require(package.admit_file_digest(lock, 1024 * 1024)[1] == record['sqlite_lock_sha256'],
            'SQLite lock changed during owner build')
    for label in ('worker', 'dependency', 'observer'):
        image = record[label]
        require(common.artifact_identity(Path(image['path'])) == image,
                f'SQLite {label} changed during owner build')
    for label, image in record['tools'].items():
        require(common.observe_build_tool(image['selected'], label) == image,
                f'SQLite {label} changed during owner build')
    raw_worker = record['worker_raw']
    require(package.admit_file_digest(Path(raw_worker['path']), package.MAX_BINARY_BYTES) ==
            (raw_worker['bytes'], raw_worker['sha256']), 'SQLite raw worker changed')
    library = record['dependency_source']
    require(package.admit_file_digest(Path(library['path']), MAX_LIBRARY) ==
            (library['bytes'], library['sha256']), 'SQLite source libgcc changed')
    raw = elf.bounded_process([record['observer']['path'], record['worker']['path'],
                               record['dependency']['path']], deadline, env, sqlite.MAX_BASIS_BYTES)
    require(sqlite.validate_observation(json.loads(raw), record['sqlite_lock_sha256']) ==
            (record['artifact_identity'], record['basis_json']), 'SQLite paired identity changed')
    for label in ('worker', 'dependency', 'observer'):
        require(common.artifact_identity(Path(record[label]['path'])) == record[label],
                f'SQLite {label} changed while reobserving')
