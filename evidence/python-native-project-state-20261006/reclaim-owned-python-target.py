"""Root-authorized disposal of this frozen Python lane's compiled graph only."""
from pathlib import Path
import datetime
import hashlib
import json
import os
import shutil
import subprocess

ROOT = Path('/root/nudox-corpus-20261006/python/product-983fa')
CACHE = Path('/root/.cache/nudox/cargo-1.97')
SLOT = CACHE / 'build/slot-5'
TARGET = ROOT / '.local/target'
EVIDENCE = Path('/root/nudox-corpus-20261006/python/project-recovery')
FROZEN = EVIDENCE / 'public-candidate-f91ee8a83'
EXPECTED = 'f91ee8a837348ee1f67531279aa3020762bab9e8'
EXPECTED_OUTPUTS = {
    'backend-cli': '3ca288d087217230d223d218bfd1d4dc9207c5413f894ee5d4f2abbb0f78c2fe',
    'backend-mcp': 'c15329bab7bae84fbd7ff0b98c65b6476c532eb125c232402b593df9aefb9d54',
}
PID = os.getpid()
LOCKS = []


def snapshot(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        before = os.fstat(source.fileno())
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
        after = os.fstat(source.fileno())
    fields = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
    assert fields(before) == fields(after), ('file changed while hashing', str(path))
    return dict(path=str(path), sha256=digest.hexdigest(), size_bytes=after.st_size,
                allocated_bytes=after.st_blocks * 512, device=after.st_dev,
                inode=after.st_ino, links=after.st_nlink)


def allocated(paths):
    seen = set()
    count = 0
    for root in paths:
        for directory, dirs, files in os.walk(root, followlinks=False):
            for name in [*dirs, *files]:
                details = (Path(directory) / name).lstat()
                key = (details.st_dev, details.st_ino)
                if key not in seen:
                    seen.add(key)
                    count += details.st_blocks * 512
    return count


def free_bytes():
    stat = os.statvfs(ROOT)
    return stat.f_bavail * stat.f_frsize


def matches():
    found = []
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) == PID:
            continue
        try:
            cwd = os.readlink(proc / 'cwd')
            arguments = (proc / 'cmdline').read_bytes().split(b'\0')
        except FileNotFoundError:
            continue
        except PermissionError as error:
            raise RuntimeError('Cannot establish process inactivity') from error
        except OSError:
            continue
        if any(cwd == str(p) or cwd.startswith(str(p) + '/') or
               any(str(p).encode() in value for value in arguments) for p in [ROOT, SLOT]):
            found.append(dict(pid=int(proc.name), cwd=cwd,
                              argv0=os.fsdecode(arguments[0]) if arguments else ''))
    return found


def revalidate():
    for path in [ROOT, SLOT, TARGET, CACHE / 'locks']:
        assert path.is_dir() and not path.is_symlink(), str(path)
    for stamp in [SLOT / '.nudox-worktree-root', TARGET / '.nudox-worktree-root',
                  CACHE / 'affinity/slot-5.owner']:
        assert not stamp.is_symlink() and stamp.read_text().strip() == str(ROOT), str(stamp)
    assert subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip() == EXPECTED
    assert not subprocess.check_output(['git', '-C', str(ROOT), 'status', '--porcelain'])
    active = matches()
    assert not active, ('Matching process found', active)


def acquire(path):
    path.mkdir()
    LOCKS.append(path)
    (path / 'pid').write_text(str(PID) + '\n')
    start = subprocess.check_output(['ps', '-p', str(PID), '-o', 'lstart='], text=True)
    (path / 'start').write_text(' '.join(start.split()) + '\n')
    (path / 'workspace').write_text(str(ROOT) + '\n')


def release():
    for path in reversed(LOCKS):
        if (path / 'pid').read_text().strip() == str(PID):
            for name in ['pid', 'start', 'workspace']:
                (path / name).unlink()
            path.rmdir()


def main():
    checksum = subprocess.check_output(
        ['/nix/store/cp7wjv1pl4wapfk48svvizxd089v9h0a-coreutils-9.11/bin/cksum'],
        input=str(ROOT).encode()).decode().split()
    acquire(CACHE / 'locks' / f'worktree-{checksum[0]}-{checksum[1]}.lock')
    acquire(CACHE / 'locks/slot-5.lock')
    revalidate()
    before = dict(allocated_unique_inode_bytes=allocated([TARGET, SLOT]), disk_free_bytes=free_bytes())
    originals = {name: snapshot(TARGET / 'debug' / name) for name in EXPECTED_OUTPUTS}
    assert all(originals[n]['sha256'] == EXPECTED_OUTPUTS[n] for n in originals)
    assert free_bytes() > sum(v['size_bytes'] for v in originals.values()) + 512 * 1024 * 1024
    FROZEN.mkdir(mode=0o700)
    preserved = []
    for name in EXPECTED_OUTPUTS:
        destination = FROZEN / name
        with (TARGET / 'debug' / name).open('rb') as source, destination.open('xb') as output:
            shutil.copyfileobj(source, output, 1024 * 1024)
            output.flush()
            os.fsync(output.fileno())
        destination.chmod(0o555)
        copy = snapshot(destination)
        current = snapshot(TARGET / 'debug' / name)
        assert copy['sha256'] == current['sha256'] == EXPECTED_OUTPUTS[name]
        assert copy['links'] == 1 and copy['inode'] != current['inode']
        preserved.append(dict(before=originals[name], source_after_copy=current, frozen_copy=copy))
    source_receipts = TARGET / '.nudox-provenance'
    archive = EVIDENCE / 'preserved-cargo-provenance'
    shutil.copytree(source_receipts, archive)
    provenance = [snapshot(p) for p in sorted(archive.iterdir()) if p.is_file()]
    retained = [snapshot(EVIDENCE / 'bin/project_report-native-05')]
    receipts = [snapshot(p) for p in sorted(EVIDENCE.iterdir()) if p.is_file() and
                (p.suffix in ['.json', '.strace', '.log', '.bundle'])]
    revalidate()
    removed = []
    for root in [SLOT, TARGET]:
        for child in sorted(root.iterdir()):
            if child.name in ['.nudox-worktree-root', '.nudox-provenance']:
                continue
            removed.append(str(child))
            if child.is_dir() and not child.is_symlink():
                shutil.rmtree(child)
            else:
                child.unlink()
    revalidate()
    for item in preserved:
        assert snapshot(Path(item['frozen_copy']['path'])) == item['frozen_copy']
    for item in [*retained, *provenance, *receipts]:
        assert snapshot(Path(item['path'])) == item
    after = dict(allocated_unique_inode_bytes=allocated([TARGET, SLOT]), disk_free_bytes=free_bytes())
    result = dict(schema='python-native-owned-target-reclaim.v1',
                  captured_at_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  source_commit=EXPECTED, source_clean=True,
                  scope='Root-authorized own slot5 and target compiled contents only; no shared sccache, Nix, source or foreign target changes',
                  held_managed_locks=[str(p) for p in LOCKS], matching_active_processes=matches(),
                  before=before, after=after, removed_paths=removed, retained_outputs=preserved,
                  retained_native_producer=retained, retained_cargo_provenance=provenance,
                  retained_evidence=receipts)
    receipt = EVIDENCE / 'native-owned-target-reclaim-01.receipt.json'
    with receipt.open('x') as output:
        json.dump(result, output, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    print(json.dumps(dict(receipt=str(receipt), receipt_sha256=snapshot(receipt)['sha256'],
                         before=before, after=after, removed_paths=removed,
                         frozen_outputs=[v['frozen_copy'] for v in preserved]), indent=2), flush=True)


try:
    main()
finally:
    release()
