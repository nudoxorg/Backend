"""Strict custody checks for an already signed, system-library-only Stylo worker.

A receipt produced by the companion build actor is build evidence, not an
independent code-signing trust anchor. Release assembly must also bind it to
its pinned clean source and preserve the final worker bytes after signing.
"""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import selectors
import shutil
import stat
import subprocess
import time

MAX_WORKER_BYTES = 512 * 1024 * 1024
SYSTEM_ROOTS = ('/usr/lib/', '/System/Library/')
TARGET_ARCH = {'aarch64-apple-darwin': 'arm64', 'x86_64-apple-darwin': 'x86_64'}


def fail(message: str) -> None:
    raise ValueError(message)


def digest_regular(path: Path, maximum: int = MAX_WORKER_BYTES, *, deadline: float | None = None) -> tuple[int, str]:
    if deadline is not None and time.monotonic() >= deadline:
        fail('artifact inspection deadline expired')
    if not path.is_absolute() or path.is_symlink():
        fail('artifact must be an absolute non-symlink path')
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > maximum:
            fail('artifact is not a bounded regular file')
        digest = hashlib.sha256()
        count = 0
        while block := stream.read(min(1024 * 1024, maximum - count + 1)):
            if deadline is not None and time.monotonic() >= deadline:
                fail('artifact inspection deadline expired')
            count += len(block)
            if count > maximum:
                fail('artifact exceeds its byte bound')
            digest.update(block)
        if deadline is not None and time.monotonic() >= deadline:
            fail('artifact inspection deadline expired')
        after = os.fstat(stream.fileno())
        selected = path.lstat()
        identity = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
        if count != before.st_size or identity(before) != identity(after) or identity(after) != identity(selected):
            fail('artifact changed during inspection')
        return count, digest.hexdigest()


def copy_admitted(source: Path, destination: Path, size: int, expected_sha: str,
                  maximum: int = MAX_WORKER_BYTES) -> None:
    """Copy exact admitted bytes without following or overreading the source."""
    if not source.is_absolute() or source.is_symlink() or type(size) is not int or size < 0 or size > maximum or not isinstance(expected_sha, str) or re.fullmatch(r'[0-9a-f]{64}', expected_sha) is None:
        fail('invalid admitted copy input')
    created = False
    try:
        fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as incoming:
            before = os.fstat(incoming.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size != size:
                fail('admitted copy source size differs')
            digest = hashlib.sha256()
            remaining = size
            with destination.open('xb') as outgoing:
                created = True
                while remaining:
                    block = incoming.read(min(1024 * 1024, remaining))
                    if not block:
                        fail('admitted copy source shortened')
                    outgoing.write(block)
                    digest.update(block)
                    remaining -= len(block)
            after = os.fstat(incoming.fileno())
            selected = source.lstat()
            identity = lambda item: (item.st_dev,item.st_ino,item.st_size,item.st_mtime_ns,item.st_ctime_ns)
            if identity(before) != identity(after) or identity(after) != identity(selected) or digest.hexdigest() != expected_sha:
                fail('admitted copy source changed')
        if digest_regular(destination, maximum) != (size, expected_sha):
            fail('staged artifact differs from admitted bytes')
    except Exception:
        if created:
            destination.unlink(missing_ok=True)
        raise


def run_tool(command: list[str], *, maximum: int = 65536, timeout: int = 30, deadline: float | None = None) -> bytes:
    tool_deadline = min(time.monotonic() + timeout, deadline if deadline is not None else float('inf'))
    if time.monotonic() >= tool_deadline:
        fail('macOS inspection deadline expired')
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=True, close_fds=True,
                               env={'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C'})
    result = bytearray()
    try:
        with selectors.DefaultSelector() as pending:
            pending.register(process.stdout, selectors.EVENT_READ)
            while pending.get_map():
                remaining = tool_deadline - time.monotonic()
                if remaining <= 0:
                    fail('macOS inspection tool timed out')
                for key, _ in pending.select(min(remaining, 0.1)):
                    block = os.read(key.fd, min(65536, maximum - len(result) + 1))
                    if not block:
                        pending.unregister(key.fileobj)
                    elif len(block) > maximum - len(result):
                        fail('macOS inspection tool exceeded its output limit')
                    else:
                        result.extend(block)
        if process.wait(timeout=max(0.001, tool_deadline - time.monotonic())):
            fail('macOS inspection tool failed')
        return bytes(result)
    finally:
        import record_embedding_helper_build as common
        retired = common.runtime.terminate_owned_group(process)
        try:
            process.wait(timeout=2)
        finally:
            process.stdout.close()
        if not retired:
            fail('macOS inspection process group did not retire')


def parse_system_loads(output: bytes, image: Path) -> list[str]:
    lines = output.decode('utf-8', 'strict').splitlines()
    if not lines or lines[0].strip() != str(image) + ':':
        fail('invalid otool dependency output')
    loads = []
    for line in lines[1:]:
        match = re.fullmatch(r'\s*(\S+) \(compatibility version [^,]+, current version [^)]+\)', line)
        if match is None:
            fail('invalid Mach-O dependency line')
        value = match.group(1)
        if not value.startswith(SYSTEM_ROOTS) or '..' in Path(value).parts:
            fail('worker has a non-system or relative Mach-O dependency')
        loads.append(value)
    return loads


def inspect_system_executable(path: Path, target: str, *, deadline: float | None = None) -> dict:
    arch = TARGET_ARCH.get(target)
    if arch is None:
        fail('invalid macOS target')
    before = digest_regular(path, deadline=deadline)
    if path.lstat().st_mode & 0o111 == 0:
        fail('worker is not executable')
    architectures = run_tool(['/usr/bin/lipo', '-archs', str(path)], deadline=deadline).decode('ascii', 'strict').split()
    if architectures != [arch]:
        fail('worker architecture does not match the single target')
    header = run_tool(['/usr/bin/otool', '-arch', arch, '-hv', str(path)], deadline=deadline).decode('utf-8', 'strict').splitlines()
    if len(header) < 4 or header[0].strip() != str(path) + ':' or len(header[3].split()) < 5 or header[3].split()[4] != 'EXECUTE':
        fail('worker is not a Mach-O executable')
    loads = parse_system_loads(run_tool(['/usr/bin/otool', '-arch', arch, '-L', str(path)], deadline=deadline), path)
    commands = run_tool(['/usr/bin/otool', '-arch', arch, '-l', str(path)], maximum=1024 * 1024, deadline=deadline).decode('utf-8', 'strict')
    if re.search(r'^\s*cmd LC_(?:RPATH|DYLD_ENVIRONMENT)\s*$', commands, re.MULTILINE):
        fail('worker carries an unsupported runtime search path or dyld environment')
    loaders = re.findall(r'^\s*cmd LC_LOAD_DYLINKER\s*\n\s*cmdsize \d+\s*\n\s*name (\S+) \(offset \d+\)', commands, re.MULTILINE)
    if loaders != ['/usr/lib/dyld']:
        fail('worker does not use the expected platform dynamic linker')
    after = digest_regular(path, deadline=deadline)
    if after != before:
        fail('executable changed during load-command checks')
    return {'bytes': before[0], 'sha256': before[1], 'architecture': arch, 'system_loads': loads}


def developer_id_requirement(team: str) -> str:
    if re.fullmatch(r'[A-Z0-9]{10}', team) is None:
        fail('invalid signing team')
    return ('=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists '
            'and certificate leaf[field.1.2.840.113635.100.6.1.13] exists '
            f'and certificate leaf[subject.OU] = "{team}"')


def inspect_signed_worker(path: Path, target: str, expected_team: str, *, deadline: float | None = None) -> dict:
    requirement = developer_id_requirement(expected_team)
    image = inspect_system_executable(path, target, deadline=deadline)
    run_tool(['/usr/bin/codesign', '--verify', '--strict', '--test-requirement', requirement, str(path)], deadline=deadline)
    signature = run_tool(['/usr/bin/codesign', '--display', '--verbose=4', str(path)], deadline=deadline).decode('utf-8', 'strict')
    teams = re.findall(r'^TeamIdentifier=(.+)$', signature, re.MULTILINE)
    if teams != [expected_team] or 'Signature=adhoc' in signature:
        fail('worker signing team is absent or mismatched')
    after = digest_regular(path, deadline=deadline)
    if after != (image['bytes'], image['sha256']):
        fail('worker changed during signature checks')
    return {**image, 'team_id': expected_team, 'signature_requirement': requirement}


def read_receipt(path: Path) -> tuple[dict, str]:
    import json
    size, digest = digest_regular(path, 65536)
    with path.open('rb') as stream:
        raw = stream.read(65537)
    if len(raw) != size or hashlib.sha256(raw).hexdigest() != digest:
        fail('companion receipt changed while reading')
    value = json.loads(raw)
    if not isinstance(value, dict):
        fail('invalid companion receipt')
    return value, digest


def admit_bound_owner(receipt_path: Path, source_root: Path, source: dict, target: str,
                      expected_receipt_sha: str, expected_actor_sha: str, expected_team: str) -> tuple[dict, str]:
    """Recheck source-bound signed worker and the owner built with its anchor."""
    import json
    if re.fullmatch(r'[0-9a-f]{64}', expected_receipt_sha) is None or re.fullmatch(r'[0-9a-f]{64}', expected_actor_sha) is None:
        fail('companion receipt or actor pin is malformed')
    if not receipt_path.is_absolute() or receipt_path.resolve(strict=True) != receipt_path:
        fail('companion receipt path must be canonical')
    receipt, receipt_sha = read_receipt(receipt_path)
    expected_source = {'commit': source.get('git_revision'), 'tree': source.get('git_tree'),
                       'cargo_lock_sha256': source.get('cargo_lock_sha256'), 'clean_before': True, 'clean_after': True}
    if receipt_sha != expected_receipt_sha or receipt.get('schema') != 'nudox.macos-stylo-bound-owner.v1' or receipt.get('source') != expected_source or receipt.get('target') != target or type(receipt.get('exit')) is not int or receipt['exit'] != 0:
        fail('companion receipt does not bind the exact successful source build')
    actor_path = source_root / 'tools/package/record_macos_stylo_build.py'
    companion_path = source_root / 'tools/package/macos_stylo_companion.py'
    if digest_regular(actor_path, 1024 * 1024)[1] != expected_actor_sha or receipt.get('actor_sha256') != expected_actor_sha or digest_regular(companion_path, 1024 * 1024)[1] != receipt.get('companion_sha256'):
        fail('companion actor differs from the pinned source')
    limits = receipt.get('limits')
    if not isinstance(limits, dict) or isinstance(limits.get('timeout_seconds'), bool) or not isinstance(limits.get('timeout_seconds'), (int,float)) or not 0 < limits['timeout_seconds'] <= 1800 or type(limits.get('max_log_bytes')) is not int or not 0 < limits['max_log_bytes'] <= 64 * 1024 * 1024:
        fail('companion receipt has invalid finite bounds')
    commands = {
        'worker': ['cargo','build','--release','--locked','--offline','-p','backend-frontend-stylo','--bin','backend-stylo-worker'],
        'observer': ['cargo','build','--release','--locked','--offline','-p','backend-compile','--example','macos_stylo_identity_observer'],
        'owner': ['cargo','build','--release','--locked','--offline','-p','backend-locald','--bin','backend-locald'],
    }
    executions = receipt.get('executions')
    if receipt.get('commands') != commands or not isinstance(executions, dict) or set(executions) != set(commands):
        fail('companion receipt does not attest the fixed build commands')
    for execution in executions.values():
        if not isinstance(execution, dict) or type(execution.get('exit')) is not int or execution['exit'] != 0 or execution.get('process_group_retired') is not True:
            fail('companion build did not finish with retired processes')
    env = receipt.get('environment')
    owner_env = receipt.get('owner_environment')
    anchor = receipt.get('worker_anchor')
    if not isinstance(env, dict) or not isinstance(owner_env, dict) or not isinstance(anchor, str) or re.fullmatch(r'[0-9a-f]{64}', anchor) is None or anchor == '0' * 64 or owner_env != {**env, 'NUDOX_BUILD_STYLO_WORKER_ID': anchor}:
        fail('owner build is not bound to the observed worker identity')
    allowed = {'PATH','HOME','TMPDIR','LANG','LC_ALL','TZ','RUSTC','CARGO_HOME','CARGO_TARGET_DIR','CARGO_BUILD_BUILD_DIR','CARGO_BUILD_TARGET','CARGO_NET_OFFLINE','CARGO_BUILD_JOBS','CARGO_INCREMENTAL','CARGO_CACHE_AUTO_CLEAN_FREQUENCY','CARGO_TERM_COLOR','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','CFLAGS','CXXFLAGS'}
    if set(env) != allowed or any(not isinstance(value, str) for value in env.values()):
        fail('companion receipt has an unadmitted build environment')
    private = [str(source_root), env.get('CARGO_HOME'), env.get('CARGO_TARGET_DIR'), env.get('CARGO_BUILD_BUILD_DIR')]
    if any(not isinstance(path, str) or not Path(path).is_absolute() or '=' in path or any(ch.isspace() for ch in path) for path in private):
        fail('companion receipt has invalid remap inputs')
    labels = ('source','cargo-home','target','build')
    rustflags = '\x1f'.join(f'--remap-path-prefix={path}=/nudox/{label}' for path,label in zip(private,labels))
    cflags = ' '.join(f'-{kind}-prefix-map={path}=/nudox/{label}' for path,label in zip(private,labels) for kind in ('ffile','fmacro'))
    if env.get('CARGO_BUILD_TARGET') != target or env.get('CARGO_NET_OFFLINE') != 'true' or env.get('CARGO_BUILD_JOBS') not in {'1','2','3','4'} or env.get('CARGO_INCREMENTAL') != '0' or env.get('CARGO_CACHE_AUTO_CLEAN_FREQUENCY') != 'never' or env.get('CARGO_TERM_COLOR') != 'never' or env.get('HOME') != env.get('CARGO_BUILD_BUILD_DIR') or env.get('TMPDIR') != env.get('CARGO_BUILD_BUILD_DIR') or env.get('RUSTFLAGS') != '' or env.get('RUSTC_WRAPPER') != '' or env.get('RUSTC_WORKSPACE_WRAPPER') != '' or env.get('CARGO_ENCODED_RUSTFLAGS') != rustflags or env.get('CFLAGS') != cflags or env.get('CXXFLAGS') != cflags:
        fail('companion build environment differs from the fixed recipe')
    toolchain = receipt.get('toolchain')
    if not isinstance(toolchain, dict) or set(toolchain) != {'cargo','rustc'} or any(not isinstance(toolchain[name], dict) or not isinstance(toolchain[name].get('path'), str) or not isinstance(toolchain[name].get('sha256'), str) for name in ('cargo','rustc')):
        fail('companion receipt omits pinned Cargo or rustc')
    expected_path = str(Path(toolchain['cargo']['path']).parent) + ':/usr/bin:/bin:/usr/sbin:/sbin'
    if env.get('PATH') != expected_path or env.get('RUSTC') != toolchain['rustc']['path'] or env.get('LANG') != 'C' or env.get('LC_ALL') != 'C' or env.get('TZ') != 'UTC':
        fail('companion toolchain environment differs from the fixed recipe')
    for name in ('cargo','rustc'):
        item = toolchain[name]
        if digest_regular(Path(item['path']))[1] != item['sha256']:
            fail('companion build tool changed')
    apple_tools = receipt.get('apple_tools')
    if not isinstance(apple_tools, dict) or set(apple_tools) != {'codesign','otool','lipo','xcrun','xcode-select','git'}:
        fail('companion receipt omits Apple tools')
    for name, identity in apple_tools.items():
        if not isinstance(identity, list) or len(identity) != 2 or digest_regular(Path('/usr/bin')/name) != tuple(identity):
            fail('Apple tool changed after companion build')
    developer = run_tool(['/usr/bin/xcode-select','-p']).decode('utf-8','strict').strip()
    if developer != receipt.get('developer_dir'):
        fail('selected Apple developer directory changed')
    sdk = receipt.get('sdk')
    if not isinstance(sdk, dict) or not isinstance(sdk.get('selected'), str) or not isinstance(sdk.get('resolved'), str) or not isinstance(sdk.get('settings'), list) or len(sdk['settings']) != 2:
        fail('companion receipt omits the selected macOS SDK')
    selected_sdk = Path(run_tool(['/usr/bin/xcrun','--sdk','macosx','--show-sdk-path']).decode('utf-8','strict').strip())
    sdk_version = run_tool(['/usr/bin/xcrun','--sdk','macosx','--show-sdk-version']).decode('utf-8','strict').strip()
    if str(selected_sdk) != sdk['selected'] or str(selected_sdk.resolve(strict=True)) != sdk['resolved'] or sdk_version != sdk.get('version') or digest_regular((Path(sdk['resolved'])/'SDKSettings.json').resolve(strict=True),1024*1024) != tuple(sdk['settings']):
        fail('selected macOS SDK changed after companion build')
    selected_tools = receipt.get('selected_tools')
    if not isinstance(selected_tools, dict) or set(selected_tools) != {'clang','otool','lipo','git'}:
        fail('companion receipt omits selected Apple tools')
    for name, item in selected_tools.items():
        selected = Path(run_tool(['/usr/bin/xcrun','--find',name]).decode('utf-8','strict').strip()).resolve(strict=True)
        if not isinstance(item, dict) or str(selected) != item.get('path') or not isinstance(item.get('identity'), list) or len(item['identity']) != 2 or digest_regular(selected) != tuple(item['identity']):
            fail('selected Apple tool changed after companion build')
    helpers = receipt.get('build_helpers')
    if not isinstance(helpers, dict) or set(helpers) != {'cc','make','python3','sh'}:
        fail('companion receipt omits build helpers')
    for name, item in helpers.items():
        selected = shutil.which(name, path=env['PATH'])
        if not isinstance(item, dict) or selected != item.get('selected') or selected is None:
            fail('selected companion build helper changed')
        resolved = Path(selected).resolve(strict=True)
        if str(resolved) != item.get('path') or digest_regular(resolved) != (item.get('bytes'),item.get('sha256')):
            fail('companion build helper changed')
    worker, observer, owner, raw = (receipt.get(name) for name in ('worker','observer','owner','raw_worker'))
    if any(not isinstance(item, dict) or not isinstance(item.get('path'), str) or type(item.get('bytes')) is not int or not isinstance(item.get('sha256'), str) for item in (worker,observer,owner,raw)):
        fail('companion receipt omits an artifact')
    output = receipt_path.parent
    if Path(worker['path']) != output/'backend-stylo-worker' or Path(owner['path']) != output/'backend-locald' or Path(raw['path']) != output/'backend-stylo-worker.raw' or Path(observer['path']) != Path(env['CARGO_TARGET_DIR'])/target/'release/examples/macos_stylo_identity_observer':
        fail('companion artifact paths differ from the fixed build outputs')
    sign = receipt.get('sign_command')
    if not isinstance(sign, list) or len(sign) != 8 or sign[:6] != ['/usr/bin/codesign','--force','--timestamp','--options','runtime','--sign'] or not isinstance(sign[6],str) or not sign[6] or sign[6] == '-' or sign[7] != worker['path']:
        fail('companion receipt does not attest its fixed signing command')
    for name,item in (('worker',worker),('observer',observer),('owner',owner),('raw worker',raw)):
        if digest_regular(Path(item['path'])) != (item['bytes'],item['sha256']):
            fail(f'{name} differs from companion receipt')
    actual_worker = inspect_signed_worker(Path(worker['path']), target, expected_team)
    if any(worker.get(key) != actual_worker.get(key) for key in ('bytes','sha256','architecture','team_id','system_loads','signature_requirement')):
        fail('signed worker differs from its admitted system closure')
    observer_image = inspect_system_executable(Path(observer['path']), target)
    if (observer_image['bytes'],observer_image['sha256']) != (observer['bytes'],observer['sha256']):
        fail('observer differs from its admitted executable')
    observed = json.loads(run_tool([observer['path'],worker['path']]))
    if observed != {'state':'observed-helper','identity':anchor}:
        fail('signed worker differs from the compiled anchor')
    if digest_regular(Path(worker['path'])) != (worker['bytes'],worker['sha256']) or digest_regular(Path(observer['path'])) != (observer['bytes'],observer['sha256']) or digest_regular(Path(owner['path'])) != (owner['bytes'],owner['sha256']) or read_receipt(receipt_path)[1] != receipt_sha:
        fail('companion inputs changed during observation')
    return receipt, receipt_sha
