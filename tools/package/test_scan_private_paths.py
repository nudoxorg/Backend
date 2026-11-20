import tempfile
import unittest
from pathlib import Path

import scan_private_paths as gate


class ScanPrivatePathsTest(unittest.TestCase):
    def write(self, data):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "binary"
        path.write_bytes(data)
        return path

    def test_reports_a_prefix_inside_a_binary(self):
        path = self.write(b"\x00\x01/root/builder/cargo-home/registry/src/x/lib.rs\x00")
        self.assertEqual(gate.scan(path, ["/root/builder"]), {"/root/builder": 1})

    def test_finds_a_prefix_that_straddles_a_read_boundary(self):
        old = gate.CHUNK
        try:
            gate.CHUNK = 8
            path = self.write(b"aaaaa/root/builder/b")
            self.assertEqual(gate.scan(path, ["/root/builder"]), {"/root/builder": 1})
        finally:
            gate.CHUNK = old

    def test_counts_each_occurrence_once_across_chunks(self):
        old = gate.CHUNK
        try:
            gate.CHUNK = 5
            path = self.write(b"/ab/ab/ab--/ab")
            self.assertEqual(gate.scan(path, ["/ab"]), {"/ab": 4})
        finally:
            gate.CHUNK = old

    def test_clean_binary_passes(self):
        path = self.write(b"/nudox/source/lib.rs /nudox/cargo-home/x.rs")
        self.assertEqual(gate.scan(path, ["/root/builder"]), {})
        self.assertEqual(gate.main(["--forbid", "/root/builder", str(path)]), 0)

    def test_leaky_binary_fails_the_gate(self):
        path = self.write(b"panic at /root/builder/src/lib.rs")
        self.assertEqual(gate.main(["--forbid", "/root/builder", str(path)]), 1)


if __name__ == "__main__":
    unittest.main()
