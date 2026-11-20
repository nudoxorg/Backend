"""Check Roslyn source-bound foreign method coordinates; no user code runs.

Usage: python3 captured_foreign_target.py /absolute/dotnet /absolute/oracle.dll
"""
import hashlib
import json
import pathlib
import struct
import subprocess
import sys
import tempfile


def main():
    dotnet, helper = sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="nudox-csharp-foreign-") as tmp:
        root = pathlib.Path(tmp)
        out = root / "image"
        target = '\ufeff// π\npublic static class Target { public static void Go() {} }\n'
        source = 'Target.Go(); System.Console.WriteLine("ok");\npublic partial class Program;\n'
        members = [{"Path":"A.cs","Text":source},{"Path":"B.cs","Text":target}]
        cmd = [dotnet,"exec",helper,"--mode","source","--root",tmp,"--captured-sources",
               "--authority-image","--source-binding",str(root / "A.cs"),"--out",str(out)]
        subprocess.run(cmd, input=json.dumps(members), check=True, capture_output=True, text=True, timeout=30)
        image = out.read_bytes()
        assert struct.unpack_from("<H",image,4)[0] == 5
        _, atom_count, atom_offset, _ = struct.unpack_from("<IIII",image,48)
        _, _, string_offset, _ = struct.unpack_from("<IIII",image,64)
        _, ref_count, ref_offset, _ = struct.unpack_from("<IIII",image,48+10*16)
        targets = []
        for index in range(ref_count):
            target_word = struct.unpack_from("<I",image,ref_offset+index*28+4)[0]
            if target_word != 0xffffffff and target_word & 0x80000000:
                atom = target_word & 0x7fffffff
                assert atom < atom_count
                offset, length = struct.unpack_from("<II",image,atom_offset+atom*8)
                targets.append(image[string_offset+offset:string_offset+offset+length].decode())
        assert len(targets) == 1, targets
        tag, path_hex, sha, start, end, name_start, name_end, doc_hex = targets[0].split("|")
        assert tag == "csharp-source-v1" and bytes.fromhex(path_hex).decode() == "B.cs"
        target_bytes = target.encode()
        assert sha == hashlib.sha256(target_bytes).hexdigest()
        start, end, name_start, name_end = map(int,(start,end,name_start,name_end))
        assert target_bytes[name_start:name_end] == b"Go"
        assert b"void Go()" in target_bytes[start:end]
        assert bytes.fromhex(doc_hex).decode() == "M:Target.Go"
        # The sidecar must retain the exact native row used by the image.
        manifest = root / "config.json"
        manifest.write_text(json.dumps([
            {"Path": "App.csproj", "Text": "<Project/>"},
            {"Path": "Directory.Build.props", "Text": None},
            {"Path": "Directory.Build.targets", "Text": None},
        ]))
        receipt_path = root / "evidence.json"
        evidence_cmd = cmd + ["--project-file", str(root / "App.csproj"),
                              "--captured-config-file", str(manifest),
                              "--capture-evidence-file", str(receipt_path),
                              "--capture-evidence-nonce", "0123456789abcdef0123456789abcdef"]
        subprocess.run(evidence_cmd, input=json.dumps(members), check=True, capture_output=True, text=True, timeout=30)
        receipt = json.loads(receipt_path.read_text())
        assert receipt["format"] == 2 and receipt["imageSha256"] == hashlib.sha256(out.read_bytes()).hexdigest()
        assert len(receipt["occurrences"]) == 1
        occurrence = receipt["occurrences"][0]
        assert occurrence["target"] == targets[0] and occurrence["owner"] == 0xffffffff
        assert source.encode()[occurrence["start"]:occurrence["end"]] == b"Go"
        # An ordinary authored caller retains its native declaration row, not the file sentinel.
        ordinary = "public class Caller { public void Run() { Target.Go(); } }\n"
        members[0]["Text"] = ordinary
        receipt_path.unlink()
        subprocess.run(evidence_cmd, input=json.dumps(members), check=True, capture_output=True, text=True, timeout=30)
        ordinary_receipt = json.loads(receipt_path.read_text())
        ordinary_refs = ordinary_receipt["occurrences"]
        assert len(ordinary_refs) == 1 and ordinary_refs[0]["owner"] != 0xffffffff
        assert ordinary.encode()[ordinary_refs[0]["start"]:ordinary_refs[0]["end"]] == b"Go"
        ordinary_image = out.read_bytes()
        _, count, offset, _ = struct.unpack_from("<IIII", ordinary_image, 48+10*16)
        actual = [struct.unpack_from("<IIIIII", ordinary_image, offset+i*28)
                  for i in range(count) if struct.unpack_from("<I", ordinary_image, offset+i*28+4)[0] & 0x80000000
                  and struct.unpack_from("<I", ordinary_image, offset+i*28+4)[0] != 0xffffffff]
        assert len(actual) == 1 and actual[0][0] == ordinary_refs[0]["owner"]
        assert actual[0][4:6] == (ordinary_refs[0]["start"], ordinary_refs[0]["end"])
        # An unbound method must not gain a coordinate from the other source's name.
        members[0]["Text"] = "Target.Missing();\npublic partial class Program;\n"
        subprocess.run(cmd, input=json.dumps(members), check=True, capture_output=True, text=True, timeout=30)
        unresolved = out.read_bytes()
        _, count, offset, _ = struct.unpack_from("<IIII", unresolved, 48+10*16)
        for index in range(count):
            word = struct.unpack_from("<I", unresolved, offset+index*28+4)[0]
            assert word == 0xffffffff or not word & 0x80000000, hex(word)
        print("compiler-resolved captured cross-file target and unbound negative passed")


if __name__ == "__main__":
    main()
