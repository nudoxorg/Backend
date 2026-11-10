"""Source-bound SQLite companion phase for an ad hoc macOS development bundle.

The caller owns source/toolchain admission, its global deadline, and final app
signing. This phase admits neither a user-selected executable nor a worker's
self-reported basis: it builds an independent observer from the same SQLite
source and lock, then records the final worker bytes and observed native basis.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re

import record_embedding_helper_build as common

SCHEMA = 'nudox.sqlite-macos-development-worker.v1'
REVISION = '4e4de76312429fb2235ae093ab2125914320d5bd'
RECIPE = 'syntaqlite-sqlite-fmt-analysis-dqs-default-v1'
MAX_BASIS_BYTES = 8192


def require(condition: bool, detail: str) -> None:
    if not condition:
        raise ValueError(detail)


def validate_observation(value: object, lock_hash: str) -> tuple[str, str]:
    require(isinstance(value, dict) and set(value) == {'state', 'artifact_identity', 'basis'}
            and value['state'] == 'observed-build-helper', 'unexpected SQLite observer output')
    identity = value['artifact_identity']
    require(isinstance(identity, str) and re.fullmatch(r'[0-9a-f]{64}', identity) is not None
            and identity != '0' * 64, 'invalid SQLite artifact identity')
    basis = value['basis']
    require(isinstance(basis, dict) and set(basis) == {'engine', 'native_revision', 'recipe', 'lock_sha256', 'support_sha256', 'settings'},
            'invalid SQLite producer basis')
    support = basis['support_sha256']
    require(basis['engine'] == 'Syntaqlite4e4de76' and basis['native_revision'] == REVISION and basis['recipe'] == RECIPE,
            'unsupported SQLite producer')
    lock = basis['lock_sha256']
    require(isinstance(lock, list) and len(lock) == 32 and all(type(x) is int and 0 <= x <= 255 for x in lock)
            and lock == list(bytes.fromhex(lock_hash)), 'SQLite source lock mismatch')
    require(isinstance(support, list) and len(support) == 32 and all(type(x) is int and 0 <= x <= 255 for x in support)
            and any(support), 'SQLite native support fingerprint unavailable')
    settings = basis['settings']
    require(isinstance(settings, dict) and set(settings) == {'Sqlite'} and isinstance(settings['Sqlite'], dict)
            and set(settings['Sqlite']) == {'version', 'cflags', 'analyzer_dqs_fallback'}
            and type(settings['Sqlite']['version']) is int and settings['Sqlite']['version'] == 2147483647
            and type(settings['Sqlite']['cflags']) is int and settings['Sqlite']['cflags'] == 0
            and settings['Sqlite']['analyzer_dqs_fallback'] is True,
            'unsupported SQLite native defaults')
    encoded = json.dumps(basis, sort_keys=True, separators=(',', ':'), ensure_ascii=True)
    require(len(encoded.encode('ascii')) <= MAX_BASIS_BYTES, 'SQLite basis exceeds bound')
    return identity, encoded


def prepare(*, source: Path, macos: Path, target: Path, build: Path, env: dict,
            target_triple: str, deadline: float, logs: Path, companion, copy_exact,
            reject_private_paths, private_paths: list[Path]) -> dict:
    """Build worker+observer, sign worker ad hoc and observe actual final bytes.

    The worker and observer use one fixed Cargo invocation, source checkout,
    standalone lock, native target and effective environment. The caller must
    compile both desktop and locald with returned owner_environment and must
    call verify_after_signing before finalizing its receipt.
    """
    manifest = source / 'frontends/sqlite/Cargo.toml'
    lock = source / 'frontends/sqlite/Cargo.lock'
    lock_size, lock_hash = companion.digest_regular(lock, 1024 * 1024, deadline=deadline)
    worker_target = target / 'sqlite-worker-build'
    worker_build = build / 'sqlite-worker-build'
    worker_target.mkdir(mode=0o700)
    worker_build.mkdir(mode=0o700)
    worker_env = {**env, 'CARGO_TARGET_DIR': str(worker_target), 'CARGO_BUILD_BUILD_DIR': str(worker_build),
                  'HOME': str(worker_build), 'TMPDIR': str(worker_build)}
    command = ['cargo', 'build', '--manifest-path', str(manifest), '--release', '--locked', '--offline',
               '--bin', 'sqlite-worker', '--example', 'installed_basis_observer']
    logs.mkdir(mode=0o700)
    execution = common.execute(command, source, worker_env, logs, deadline, 64 * 1024 * 1024)
    require(execution['exit'] == 0 and execution['process_group_retired'], 'SQLite worker/observer build failed')
    release = worker_target / target_triple / 'release'
    worker = macos / 'sqlite-worker'
    copy_exact(release / 'sqlite-worker', worker, deadline)
    companion.run_tool(['/usr/bin/codesign', '--force', '--sign', '-', str(worker)], timeout=180, deadline=deadline)
    companion.run_tool(['/usr/bin/codesign', '--verify', '--strict', str(worker)], deadline=deadline)
    image = companion.inspect_system_executable(worker, target_triple, deadline=deadline)
    reject_private_paths(worker, private_paths, deadline)
    observer = release / 'examples/installed_basis_observer'
    observer_image = companion.inspect_system_executable(observer, target_triple, deadline=deadline)
    raw = companion.run_tool([str(observer), str(worker)], maximum=MAX_BASIS_BYTES, deadline=deadline)
    value = json.loads(raw)
    identity, basis_json = validate_observation(value, lock_hash)
    require(companion.inspect_system_executable(observer, target_triple, deadline=deadline) == observer_image,
            'SQLite observer changed while running')
    require(companion.inspect_system_executable(worker, target_triple, deadline=deadline) == image,
            'SQLite worker changed while observing')
    require(companion.digest_regular(lock, 1024 * 1024, deadline=deadline) == (lock_size, lock_hash),
            'SQLite lock changed during worker build')
    return {'schema': SCHEMA, 'scope': 'macOS ad hoc development; no distribution authority',
            'command': command, 'execution': execution, 'environment': worker_env,
            'lock_sha256': lock_hash, 'worker_image': image, 'observer_image': observer_image,
            'worker_path': str(worker), 'observer_path': str(observer), 'artifact_identity': identity,
            'basis_json': basis_json, 'owner_environment': {'NUDOX_BUILD_SQLITE_WORKER_ID': identity,
                                                            'NUDOX_BUILD_SQLITE_BASIS_JSON': basis_json}}


def verify_after_signing(record: dict, *, source: Path, target_triple: str, deadline: float, companion) -> None:
    """Recheck observer, worker, native basis and lock after outer app signing."""
    worker, observer = Path(record['worker_path']), Path(record['observer_path'])
    companion.run_tool(['/usr/bin/codesign', '--verify', '--strict', str(worker)], deadline=deadline)
    require(companion.inspect_system_executable(worker, target_triple, deadline=deadline) == record['worker_image'],
            'outer signing changed SQLite worker')
    require(companion.inspect_system_executable(observer, target_triple, deadline=deadline) == record['observer_image'],
            'SQLite observer changed during owner build')
    raw = companion.run_tool([str(observer), str(worker)], maximum=MAX_BASIS_BYTES, deadline=deadline)
    require(validate_observation(json.loads(raw), record['lock_sha256']) ==
            (record['artifact_identity'], record['basis_json']), 'SQLite worker or basis changed')
    require(companion.inspect_system_executable(observer, target_triple, deadline=deadline) == record['observer_image'],
            'SQLite observer changed while rechecking')
    require(companion.inspect_system_executable(worker, target_triple, deadline=deadline) == record['worker_image'],
            'SQLite worker changed while rechecking')
    _, lock_hash = companion.digest_regular(source / 'frontends/sqlite/Cargo.lock', 1024 * 1024, deadline=deadline)
    require(lock_hash == record['lock_sha256'], 'SQLite lock changed')
