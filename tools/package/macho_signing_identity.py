"""Bind thin 64-bit Mach-O content across Apple's code-signature replacement."""
from pathlib import Path
import struct
import tempfile

from linux_release_package import admit_file_digest, copy_admitted_file

CODESIGN = Path('/usr/bin/codesign')
MAX_IMAGE = 512 * 1024**2
MAX_COMMANDS = 1024**2


def layout(path, *, signed_input=True):
    size = path.stat().st_size
    with path.open('rb') as stream:
        header = stream.read(32)
        endian = {b'\xcf\xfa\xed\xfe': '<', b'\xfe\xed\xfa\xcf': '>'}.get(header[:4])
        if endian is None or len(header) != 32:
            raise ValueError('code identity requires a thin 64-bit Mach-O header')
        count, length = struct.unpack_from(endian + 'II', header, 16)
        if count > 4096 or length > MAX_COMMANDS or 32 + length > size:
            raise ValueError('Mach-O load-command table exceeds its bounded extent')
        commands = stream.read(length)
    offset = 0
    linkedit = signature = None
    segments = []
    for _ in range(count):
        if offset + 8 > length:
            raise ValueError('truncated Mach-O load command')
        kind, width = struct.unpack_from(endian + 'II', commands, offset)
        if width < 8 or width % 8 or offset + width > length:
            raise ValueError('invalid Mach-O load-command extent')
        if kind == 0x19:  # LC_SEGMENT_64
            if width < 72:
                raise ValueError('truncated Mach-O segment')
            nsects = struct.unpack_from(endian + 'I', commands, offset + 64)[0]
            if width != 72 + 80 * nsects:
                raise ValueError('Mach-O segment section table has an invalid extent')
            vmaddr, vmsize, fileoff, filesize = struct.unpack_from(endian + 'QQQQ', commands, offset + 24)
            if fileoff + filesize > size:
                raise ValueError('Mach-O segment exceeds the file extent')
            name = commands[offset + 8:offset + 24].rstrip(b'\0')
            segments.append((name, vmaddr, vmsize, fileoff, filesize))
            if name == b'__LINKEDIT':
                if linkedit is not None or nsects:
                    raise ValueError('duplicate or section-bearing __LINKEDIT segment')
                linkedit = (32 + offset + 32, vmaddr, vmsize, fileoff, filesize)
        elif kind == 0x1d:  # LC_CODE_SIGNATURE
            if signature is not None or width != 16:
                raise ValueError('duplicate or malformed LC_CODE_SIGNATURE')
            signature = struct.unpack_from(endian + 'II', commands, offset + 8)
        offset += width
    if offset != length or linkedit is None:
        raise ValueError('Mach-O table has trailing bytes or no unique __LINKEDIT')
    field, vmaddr, vmsize, fileoff, filesize = linkedit
    if not filesize or fileoff < 32 + length or fileoff + filesize != size:
        raise ValueError('__LINKEDIT must have a nonempty terminal file extent')
    if signature is not None:
        start, extent = signature
        if not extent or start < fileoff or start + extent != size:
            raise ValueError('LC_CODE_SIGNATURE must occupy the terminal __LINKEDIT extent')
    if signed_input:
        # Apple's signer extends this allocation for its signature. Require the
        # actual signed file extent to explain it; never mask an arbitrary VM size.
        page_extents = {((filesize + page - 1) // page) * page for page in (4096, 16384)}
        if vmsize not in page_extents:
            raise ValueError('__LINKEDIT VM extent does not match its signed file extent')
        for name, start, extent, _, _ in segments:
            if name != b'__LINKEDIT' and extent and start < vmaddr + vmsize and vmaddr < start + extent:
                raise ValueError('__LINKEDIT VM extent overlaps another segment')
    return endian, field, fileoff, filesize, signature


def code_identity(image, probe):
    """Hash every byte after signature removal and one extent-derived VM field."""
    size, raw_sha = admit_file_digest(image, MAX_IMAGE)
    tool_size, tool_sha = admit_file_digest(CODESIGN, 16 * 1024**2)
    with tempfile.TemporaryDirectory(prefix='nudox-signing-identity-') as temporary:
        root = Path(temporary)
        copy = root / 'image'
        copy_admitted_file(image, copy, raw_sha, size, MAX_IMAGE)
        copy.chmod(0o600)
        _, _, _, _, signature = layout(copy)
        if signature is not None:
            completed = probe([str(CODESIGN), '--remove-signature', str(copy)], str(root),
                              {'PATH': '/usr/bin:/bin', 'HOME': str(root), 'TMPDIR': str(root)})
            if completed.returncode:
                raise ValueError('Apple code-signature removal failed on private image copy')
        endian, field, _, unsigned_size, remaining = layout(copy, signed_input=False)
        if remaining is not None:
            raise ValueError('Apple removal left an LC_CODE_SIGNATURE command')
        # Removal retains the former signature's __LINKEDIT.vmsize. The entire
        # unsigned file is otherwise hashed, including all code/load commands.
        canonical_vm_extent = ((unsigned_size + 16383) // 16384) * 16384
        with copy.open('r+b') as stream:
            stream.seek(field)
            stream.write(struct.pack(endian + 'Q', canonical_vm_extent))
        _, normalized_sha = admit_file_digest(copy, MAX_IMAGE)
    if admit_file_digest(image, MAX_IMAGE) != (size, raw_sha) or admit_file_digest(CODESIGN, 16 * 1024**2) != (tool_size, tool_sha):
        raise ValueError('Mach-O image or selected Apple codesign changed during identity admission')
    return {'raw_sha256': raw_sha, 'signature_independent_sha256': normalized_sha,
            'codesign_path': str(CODESIGN), 'codesign_sha256': tool_sha,
            'normalization': 'Apple signature removal; unsigned terminal __LINKEDIT.vmsize rounded to 16KiB; every other byte retained'}
