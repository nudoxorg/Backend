"""Portable binary fixtures, not native signing or installed SDK evidence."""
import pathlib
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import macho_signing_identity as identity


def fixture(signature=b'fixture signature', *, code=b'original code', vm_extent=None):
    content = bytearray(4096)
    content[1024:1024 + len(code)] = code
    payload = b'linkedit payload' + bytes(112)
    size = len(payload) + len(signature)
    vm_extent = vm_extent or ((size + 16383) // 16384) * 16384
    struct.pack_into('<IiiIIIII', content, 0, 0xfeedfacf, 0x100000c, 0, 2, 2, 88, 0, 0)
    struct.pack_into('<II16sQQQQiiII', content, 32, 0x19, 72, b'__LINKEDIT', 8192, vm_extent, 4096, size, 7, 1, 0, 0)
    struct.pack_into('<IIII', content, 104, 0x1d, 16, 4096 + len(payload), len(signature))
    return bytes(content) + payload + signature


def remove_signature(command, cwd, environment):
    path = pathlib.Path(command[-1])
    data = bytearray(path.read_bytes())
    endian, _, fileoff, _, signature, _ = identity.layout(path)
    assert signature is not None
    data = data[:signature[0]]
    struct.pack_into(endian + 'II', data, 16, 1, 72)
    data[104:120] = bytes(16)
    struct.pack_into(endian + 'Q', data, 32 + 48, len(data) - fileoff)
    path.write_bytes(data)
    return subprocess.CompletedProcess(command, 0, b'', b'')


class MachoSigningIdentity(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.tool = self.root / 'codesign'
        self.tool.write_bytes(b'unit selected static tool')
        self.patch = patch.object(identity, 'CODESIGN', self.tool)
        self.patch.start(); self.addCleanup(self.patch.stop)

    def capture(self, data):
        image = self.root / 'image'
        image.write_bytes(data)
        return identity.code_identity(image, remove_signature)

    def test_only_signature_and_explained_linkedit_vm_extent_can_change(self):
        before = self.capture(fixture())
        after = self.capture(fixture(b'changed signature' * 512))
        self.assertNotEqual(before['raw_sha256'], after['raw_sha256'])
        self.assertEqual(before['signature_independent_sha256'], after['signature_independent_sha256'])
        with self.assertRaisesRegex(ValueError, 'VM extent'):
            self.capture(fixture(vm_extent=4096))

    def test_code_and_adjacent_segment_field_are_never_masked(self):
        before = self.capture(fixture())['signature_independent_sha256']
        self.assertNotEqual(self.capture(fixture(code=b'substitute code'))['signature_independent_sha256'], before)
        changed = bytearray(fixture())
        struct.pack_into('<Q', changed, 32 + 24, 8193)
        self.assertNotEqual(self.capture(changed)['signature_independent_sha256'], before)

    def test_malformed_duplicate_and_truncated_tables_fail_before_tool(self):
        candidates = [b'\xcf\xfa\xed\xfe', bytearray(fixture()), bytearray(fixture()), bytearray(fixture()), bytearray(fixture())]
        struct.pack_into('<I', candidates[1], 20, identity.MAX_COMMANDS + 1)
        struct.pack_into('<I', candidates[2], 36, 8)
        struct.pack_into('<Q', candidates[3], 32 + 32, 123)
        struct.pack_into('<II', candidates[4], 104, 0x19, 72)
        duplicate_segment = bytearray(fixture())
        struct.pack_into('<II', duplicate_segment, 16, 3, 160)
        duplicate_segment[120:192] = duplicate_segment[32:104]
        duplicate_signature = bytearray(fixture())
        struct.pack_into('<II', duplicate_signature, 16, 3, 104)
        duplicate_signature[120:136] = duplicate_signature[104:120]
        missing_command = bytearray(fixture())
        struct.pack_into('<I', missing_command, 16, 3)
        nonterminal_signature = bytearray(fixture())
        struct.pack_into('<I', nonterminal_signature, 112, 4096)
        candidates.extend((duplicate_segment, duplicate_signature, missing_command, nonterminal_signature))
        for data in candidates:
            image = self.root / 'bad'; image.write_bytes(data)
            with self.subTest(data=data[:40]), patch('test_macho_signing_identity.remove_signature') as probe:
                with self.assertRaises(ValueError): identity.code_identity(image, probe)
                probe.assert_not_called()

    def test_tool_identity_and_private_copy_are_retained(self):
        result = self.capture(fixture())
        self.assertEqual(result['codesign_path'], str(self.tool))
        self.assertEqual((self.root / 'image').read_bytes(), fixture())
        def mutate_tool(*args):
            result = remove_signature(*args)
            self.tool.write_bytes(b'changed tool')
            return result
        with self.assertRaisesRegex(ValueError, 'codesign changed'):
            identity.code_identity(self.root / 'image', mutate_tool)


if __name__ == '__main__': unittest.main()
