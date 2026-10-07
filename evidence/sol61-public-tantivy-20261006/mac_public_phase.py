#!/usr/bin/env python3
"""One admitted native Mac owner per phase, with exact public wire retention.

The coordinator supplies a fresh whole-fleet screen for every invocation.
Publication uses a new private official source copy and ordinary public vars.
Subsequent invocations preserve that same absolute project and state paths.
"""
import argparse
import base64
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import uuid
import zlib

from deep_public_pages import identity, run_matrix, traverse


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def inventory(path):
    rows = []
    for entry in sorted(path.rglob("*")):
        relative = entry.relative_to(path).as_posix()
        if entry.is_symlink():
            rows.append((relative, "symlink", os.readlink(entry)))
        elif entry.is_file():
            rows.append((relative, "file", entry.stat().st_size, digest(entry)))
        elif entry.is_dir():
            rows.append((relative, "directory"))
        else:
            raise AssertionError("unsupported private projection entry")
    return rows


def available_memory():
    raw = subprocess.check_output(["/usr/bin/vm_stat"], text=True)
    page = int(re.search(r"page size of (\d+) bytes", raw)[1])
    fields = {key.strip(): int(value.strip().rstrip(".")) for key, value in
              (line.split(":", 1) for line in raw.splitlines()[1:] if ":" in line)}
    return page * sum(fields[key] for key in
                      ("Pages free", "Pages inactive", "Pages speculative"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--expected-source")
    parser.add_argument("--portable-root", type=Path)
    parser.add_argument("--fleet-proof", type=Path, required=True)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--phase", required=True)
    parser.add_argument("--publish", action="store_true")
    parser.add_argument("--matrix", action="store_true")
    parser.add_argument("--replay", action="store_true")
    parser.add_argument("--refresh", action="store_true")
    parser.add_argument("--remove", action="store_true")
    parser.add_argument("--expect-removed", action="store_true")
    parser.add_argument("--stale-tokens", action="store_true")
    parser.add_argument("--inactive-donor", type=Path)
    parser.add_argument("--inactive-target", type=Path)
    parser.add_argument("--hold-reader", action="store_true")
    parser.add_argument("--verify-selected", action="store_true")
    parser.add_argument("--selected-fault", action="store_true")
    parser.add_argument("--unicode-edit", action="store_true")
    parser.add_argument("--restore-source", action="store_true")
    parser.add_argument("--require-unicode-hit", action="store_true")
    parser.add_argument("--native-default", action="store_true")
    parser.add_argument("--surface-checks", action="store_true")
    parser.add_argument("--tokens", default="replay.json")
    parser.add_argument("--capture-to")
    parser.add_argument("--recapture-to")
    parser.add_argument("--offline-acquisition", action="store_true")
    parser.add_argument("--contract-negatives", action="store_true")
    parser.add_argument("--catalog-probe", action="store_true")
    args = parser.parse_args()
    proof = json.loads(args.fleet_proof.read_text())
    sampled = datetime.datetime.fromisoformat(proof["sampled_at_utc"]).timestamp()
    assert proof["advisory_allowed"] is True and proof["destination"] == "h16001mac"
    assert 0 <= time.time() - sampled <= 60, "fresh entire fleet admission required"
    args.root.mkdir(mode=0o700, parents=True, exist_ok=True)
    permit = (args.root / "allocated-runtime.lock").open("a+")
    fcntl.flock(permit, fcntl.LOCK_EX | fcntl.LOCK_NB)
    project, state = args.root / "project", args.root / "state"
    if args.publish:
        assert args.source and not project.exists() and not state.exists(), "fresh publication only"
        shutil.copytree(args.source, project)
    assert project.is_dir()
    phase = args.root / args.phase
    phase.mkdir(mode=0o700)
    shutil.copyfile(args.fleet_proof, phase / "fleet-admission.json")
    manifest = json.loads(args.manifest.read_text())
    assert manifest["schema"] == "nudox.runtime-build-manifest.v1"
    if args.expected_source:
        assert manifest["source"]["commit"] == args.expected_source, "exact reviewed canonical cohort required"
    images = {}
    for name in ("backend-cli", "backend-locald", "backend-mcp"):
        row = manifest["executables"][name]
        image = Path(row["path"])
        assert image.is_file() and not image.is_symlink()
        assert image.stat().st_size == row["bytes"] and digest(image) == row["sha256"]
        images[name] = str(image)
    assert digest(manifest["root_receipt"]["path"]) == manifest["root_receipt"]["sha256"]
    package = None
    if args.portable_root:
        package = json.loads((args.portable_root / "manifest.json").read_text())
        assert package["source"] == manifest["source"]["commit"]
        assert package["runtime_manifest_sha256"] == digest(args.manifest)
        for row in package["files"]:
            image = args.portable_root / row["packaged"]
            assert image.is_relative_to(args.portable_root) and image.is_file() and not image.is_symlink()
            assert digest(image) == row["packaged_sha256"]
            name = image.name
            if name in images:
                assert row["original_sha256"] == manifest["executables"][name]["sha256"]
                images[name] = str(image)
        assert all(Path(path).is_relative_to(args.portable_root) for path in images.values())
    home, tmp = phase / "home", phase / "tmp"
    home.mkdir(); tmp.mkdir()
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "TMPDIR": str(tmp),
           "USER": "rmccrar6", "LANG": "C.UTF-8",
           "NUDOX_PYTHON": "/nix/store/llk2h8rxqzv7zh53bi413ffibjrxskxw-python3-3.14.6/bin/python3.14",
           "NUDOX_PYREFLY": "/nix/store/pri5s6i4j71w929h18l4hvdvvkm875gd-pyrefly-1.2.0-dev.1/bin/pyrefly"}
    if args.native_default:
        env.pop("NUDOX_PYTHON"); env.pop("NUDOX_PYREFLY")
        env["PATH"] = os.environ["PATH"]
    endpoint = "/tmp/nudox-sol61-" + uuid.uuid4().hex[:18] + ".sock"
    common = ["--project", str(project), "--workspace", str(state), "--endpoint", endpoint]
    wire = (phase / "wire.jsonl").open("w")
    started = time.time()
    receipt = {"schema": "nudox.native-public-phase.v1", "phase": args.phase,
               "started_at_unix": started, "runtime_source": manifest["source"],
               "observer": {"path": str(Path(__file__).resolve()), "sha256": digest(__file__),
                            "paging_sha256": digest(Path(__file__).with_name("deep_public_pages.py")),
                            "python": sys.executable, "python_sha256": digest(sys.executable),
                            "python_version": sys.version, "host": tuple(os.uname())},
               "source": str(project), "state": str(state), "manifest": str(args.manifest),
               "manifest_sha256": digest(args.manifest), "environment": env,
               "allocation": "Root allocated sol61-public-tantivy one private Mac runtime owner",
               "whole_project_runtime_pass": False, "all_phase_checks_pass": False}
    receipt["portable_package"] = package
    receipt["used_images"] = images
    if args.unicode_edit or args.restore_source:
        assert args.unicode_edit != args.restore_source and args.refresh
        source = project / "src/cachetools/__init__.py"
        backup = args.root / "original-cachetools-init.py"
        before = source.read_bytes()
        if args.unicode_edit:
            assert not backup.exists(), "preserve first original source backup"
            backup.write_bytes(before)
            text = before.decode()
            declaration = text.index("\ndef cached(")
            opening = text.index('"""', declaration) + 3
            text = text[:opening] + " λ路径 sol61 genuine documentation refresh. " + text[opening:]
            source.write_text(text)
        else:
            assert backup.is_file()
            source.write_bytes(backup.read_bytes())
        receipt["source_change"] = {"path": str(source), "before_sha256": hashlib.sha256(before).hexdigest(),
                                    "after_sha256": digest(source), "preserved_backup_sha256": digest(backup)}
    inactive, held_reader, inactive_before = args.inactive_target, None, None
    selected_binding = selected_stamp = None
    if args.selected_fault:
        replay = json.loads((args.root / args.tokens).read_text())
        selected = Path(replay["selected_projection"])
        assert selected.is_relative_to(state / "search-index-v2" / "v4")
        selected_binding = selected / "backend-binding-v3"
        selected_stamp = selected_binding.read_bytes()
        assert selected_stamp.hex() == selected.name and len(selected_stamp) == 32
        selected_binding.write_bytes(selected_stamp[:-1] + bytes([selected_stamp[-1] ^ 1]))
        receipt["selected_fault"] = {"target": str(selected_binding), "original_binding": selected_stamp.hex()}
    if args.inactive_donor:
        donor = args.inactive_donor
        stamp = (donor / "backend-binding-v3").read_bytes()
        assert len(stamp) == 32 and stamp.hex() == donor.name, "authentic pre-injection binding required"
        donor_before = inventory(donor)
        inactive = state / "search-index-v2" / "v4" / donor.name
        selected = json.loads((args.root / args.tokens).read_text())["selected_projection"]
        assert str(inactive) != selected, "authentic prior root must be inactive in current selection"
        if inactive.exists():
            assert (inactive / "backend-binding-v3").read_bytes() == stamp, "existing genuine private prior-root binding"
            mode = "existing-actual-private-prior-root"
        else:
            shutil.copytree(donor, inactive)
            assert inventory(inactive) == donor_before
            mode = "new-exact-private-donor-copy"
        (inactive / "backend-binding-v3").write_bytes(stamp[:-1] + bytes([stamp[-1] ^ 1]))
        assert inventory(donor) == donor_before, "original donor must remain immutable"
        receipt["authentic_inactive_injection"] = {"donor": str(donor), "target": str(inactive),
                                                  "mode": mode, "original_binding": stamp.hex(), "donor_inventory": donor_before}
    if inactive:
        assert inactive.is_relative_to(state / "search-index-v2" / "v4"), "owned private projection only"
        inactive_before = inventory(inactive)
        receipt["inactive_before"] = inactive_before
        if args.hold_reader:
            # Open an existing lease only; never create ownership evidence.
            held_reader = (inactive / ".backend-root-reader.lock").open("rb")
            fcntl.flock(held_reader, fcntl.LOCK_SH)
            receipt["independent_reader_holder_pid"] = os.getpid()
    def record(kind, **fields):
        wire.write(json.dumps({"kind": kind, "at_unix": time.time(), **fields}, ensure_ascii=False) + "\n")
        wire.flush()
    def cli(label, words):
        argv = [images["backend-cli"], "--json", "--detail", "full", *common,
                *(["--passive"] if words == ["health"] else []), *words]
        child = subprocess.Popen(argv, env=env, cwd=project, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, start_new_session=True)
        try:
            out, err = child.communicate(timeout=120)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGTERM)
            out, err = child.communicate(timeout=10)
        (phase / (label + ".stdout")).write_bytes(out)
        (phase / (label + ".stderr")).write_bytes(err)
        assert len(out) <= 4 * 1024**2, "bounded retained diagnostic"
        value = json.loads(out) if out else {}
        record("cli-reply", label=label, argv=argv, exit=child.returncode,
               bytes=len(out), payload=value)
        return child.returncode, value
    owner = client = None
    done = threading.Event()
    memory = []
    files = []
    try:
        assert 0 <= time.time() - sampled <= 60, "fleet screen must remain fresh at owner launch"
        assert available_memory() >= 8 * 1024**3
        assert shutil.disk_usage(args.root).free >= 16 * 1024**3
        files = [(phase / name).open("wb") for name in ("owner.stdout", "owner.stderr", "mcp.stderr")]
        argv = [images["backend-locald"], "--workspace", str(state), "--endpoint", endpoint,
                "--idle-timeout-ms", "0"]
        if args.offline_acquisition:
            argv += ["--registry-offline", "--registry-discovery-offline",
                     "--advisory-offline", "--forge-offline"]
        owner = subprocess.Popen(argv, env=env, cwd=project, stdout=files[0], stderr=files[1], start_new_session=True)
        receipt["owner_pid"] = owner.pid
        record("owner-start", pid=owner.pid, argv=argv)
        def guard():
            while not done.wait(2):
                value = available_memory(); memory.append(value)
                if value < 8 * 1024**3 and owner.poll() is None:
                    receipt["guard_stopped_owned_owner"] = True
                    os.killpg(owner.pid, signal.SIGTERM)
                    return
        watcher = threading.Thread(target=guard, daemon=True); watcher.start()
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            assert owner.poll() is None, "owned locald exited before readiness"
            probe = socket.socket(socket.AF_UNIX)
            try:
                probe.connect(endpoint); break
            except (FileNotFoundError, ConnectionRefusedError):
                time.sleep(.05)
            finally:
                probe.close()
        else:
            raise TimeoutError("owned locald readiness")
        code, health = cli("health", ["health"])
        assert code == 0, health
        if args.publish:
            code, added = cli("add", ["add", str(project)])
            receipt["publication"] = added
            assert code == 0, added
        expected_files = sum(path.suffix in (".py", ".pyi", ".pyw")
                             for path in project.rglob("*") if path.is_file())
        receipt["eligible_python_inputs"] = expected_files
        if not args.remove and not args.refresh and not args.expect_removed:
            # Check the selected current frontier after every restart and
            # ordinary retry/edit, not only the first successful add.
            code, history = cli("history", ["semantic-versions", str(project)])
            receipt["history"] = history
            assert code == 0, history
            selected = [row for row in history.get("semantic_data", {}).get("value", [])
                        if row.get("selected") is True and row.get("complete") is True
                        and row.get("freshness", {}).get("state") == "current"]
            assert len(selected) == 1 and selected[0]["artifacts"] == expected_files, "actual CurrentSelectedComplete must cover every eligible Python input"
            assert selected[0]["selected_source_frontier"]["file_count"] == expected_files
            receipt["native_selection_full_eligible_frontier"] = True
        if args.refresh or args.remove or args.expect_removed:
            if not args.expect_removed:
                words = ["remove" if args.remove else "add", str(project)]
                code, changed = cli("lifecycle-change", words)
                receipt["lifecycle_change"] = changed
                assert code == 0, changed
            code, history = cli("history-after-change", ["semantic-versions", str(project)])
            receipt["history_after_change"] = history
            selected = [row for row in history.get("semantic_data", {}).get("value", [])
                        if row.get("selected") is True and row.get("complete") is True
                        and row.get("freshness", {}).get("state") == "current"]
            if args.remove or args.expect_removed:
                assert not selected, "removed project must have no current selected publication"
                if code != 0:
                    assert history == {"slug": "invalid-query", "operand": "", "cause": "malformed",
                                       "detail": "semantic publication unavailable: project authority",
                                       "answer": "fault"}, history
                    receipt["removed_history_exact_project_authority_refusal"] = True
            else:
                assert code == 0, history
                assert len(selected) == 1 and selected[0]["artifacts"] == expected_files
                assert selected[0]["selected_source_frontier"]["file_count"] == expected_files
                receipt["native_selection_full_eligible_frontier_after_change"] = True
        client = subprocess.Popen([images["backend-mcp"], *common], env=env, cwd=project,
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=files[2], start_new_session=True)
        selector = selectors.DefaultSelector(); selector.register(client.stdout, selectors.EVENT_READ)
        sequence, buffer = 0, b""
        def rpc(method, params, notification=False):
            nonlocal sequence, buffer
            sequence += 1
            request = {"jsonrpc": "2.0", "method": method, "params": params}
            if not notification: request["id"] = sequence
            encoded = (json.dumps(request, ensure_ascii=False) + "\n").encode()
            record("mcp-request", bytes=len(encoded), payload=request)
            client.stdin.write(encoded); client.stdin.flush()
            if notification: return
            deadline = time.monotonic() + 120
            while b"\n" not in buffer:
                assert selector.select(max(0, deadline - time.monotonic())), "MCP deadline"
                block = os.read(client.stdout.fileno(), 65536)
                assert block, "MCP EOF"
                buffer += block
                assert len(buffer) <= 4 * 1024**2
            line, buffer = buffer.split(b"\n", 1)
            value = json.loads(line)
            record("mcp-reply", bytes=len(line) + 1, payload=value)
            assert value.get("id") == sequence, "MCP response identity"
            return value
        rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                           "clientInfo": {"name": "sol61-actual-pages", "version": "1"}})
        rpc("notifications/initialized", {}, True)
        counter = 0
        def query(surface, route, text, credit, token):
            nonlocal counter
            counter += 1
            if surface == "cli":
                words = [route, text, "--limit", str(credit)]
                if token: words += ["--cursor", token]
                code, value = cli("query-%06d" % counter, words)
                if value.get("answer") == "records":
                    assert code == 0, "successful public records require successful CLI exit"
                return value
            arguments = {("coordinate" if route == "graph" else "query"): text,
                         "limit": credit, "detail": "full"}
            if token: arguments["cursor"] = token
            reply = rpc("tools/call", {"name": "backend." + route, "arguments": arguments})
            return reply.get("result", {}).get("structuredContent", reply)
        def typed_refusal(value):
            return ((value.get("answer") == "fault" and isinstance(value.get("slug"), str))
                    or isinstance(value.get("error", {}).get("code"), int)
                    or (value.get("result", {}).get("isError") is True
                        and value["result"].get("content")))
        def reference(surface, route, text):
            # Retain large-credit refusals, then obtain the complete real
            # stream with an ordinary smaller public credit.
            value = query(surface, route, text, 200, None)
            credit, refusals = 200, []
            for candidate in (7, 3, 1):
                if value.get("answer") == "records":
                    break
                assert (value.get("answer") == "fault" and value.get("slug") == "transport"
                        and "byte budget" in value.get("detail", "")), value
                refusals.append({"credit": credit, "fault": value})
                credit = candidate
                value = query(surface, route, text, credit, None)
            rows, tokens = [], set()
            for page in range(4097):
                assert value.get("answer") == "records", value
                current = value.get("records", [])
                assert len(current) <= credit
                rows.extend(current)
                token = value.get("nextCursor")
                assert bool(value.get("more")) == bool(token)
                if not token:
                    break
                assert current and token not in tokens
                tokens.add(token)
                value = query(surface, route, text, credit, token)
            else:
                raise AssertionError("complete reference exceeded 4096 pages")
            receipt.setdefault("complete_reference_streams", []).append(
                {"surface": surface, "route": route, "query": text, "credit": credit,
                 "pages": page + 1, "rows": len(rows), "typed_budget_refusals": refusals})
            return {"answer": "records", "records": rows, "more": False}
        def capture_tokens(filename):
            replay = {"owner_pid": owner.pid, "issued_at_unix": time.time(),
                      "manifest_sha256": receipt["manifest_sha256"], "cases": []}
            for route in ("resolve", "search"):
                for text in ("cached", "__getitem__"):
                    for surface in ("cli", "mcp"):
                        full = reference(surface, route, text)
                        first = query(surface, route, text, 3, None)
                        assert full.get("answer") == "records" and not full.get("more"), full
                        assert first.get("answer") == "records" and first.get("nextCursor"), first
                        assert list(map(identity, first["records"])) == list(map(identity, full["records"][:3]))
                        replay["cases"].append({"surface": surface, "route": route, "text": text,
                                                "credit": 3, "first": first, "expected": full["records"]})
            projections = [path for path in (state / "search-index-v2" / "v4").iterdir()
                           if path.is_dir() and len(path.name) == 64
                           and (path / "backend-binding-v3").read_bytes().hex() == path.name]
            if len(projections) > 1:
                leased = []
                for path in projections:
                    # This private state has exactly one owned daemon. Its
                    # current reader lease identifies the actively queried
                    # projection; do not infer it from modification time.
                    with (path / ".backend-root-reader.lock").open("rb") as lease:
                        try:
                            fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        except BlockingIOError:
                            leased.append(path)
                        else:
                            fcntl.flock(lease, fcntl.LOCK_UN)
                projections = leased
            assert len(projections) == 1, "fresh selected projection must be unambiguous"
            replay["selected_projection"] = str(projections[0])
            target = args.root / filename
            assert not target.exists(), "preserve every originally issued token set"
            target.write_text(json.dumps(replay, ensure_ascii=False, indent=2) + "\n")
            receipt.setdefault("issued_token_sets", []).append(
                {"path": str(target), "sha256": digest(target), "cases": len(replay["cases"]),
                 "issued_at_unix": replay["issued_at_unix"]})
        if args.publish or args.capture_to:
            capture_tokens(args.capture_to or args.tokens)
        if args.replay:
            replay = json.loads((args.root / args.tokens).read_text())
            assert replay["manifest_sha256"] == receipt["manifest_sha256"], "matched cohort replay"
            assert replay["owner_pid"] != owner.pid, "genuine new owner process required"
            replay_results = []
            for case in replay["cases"]:
                def resumed(surface, route, text, credit, token):
                    # Feed the retained first page only to the comparison;
                    # every successor request carries the original old token.
                    return case["first"] if token is None else query(surface, route, text, credit, token)
                replay_results.append(traverse(resumed, case["surface"], case["route"], case["text"],
                                               case["credit"], case["expected"]))
            receipt["old_token_new_owner_replays"] = replay_results
        if args.stale_tokens:
            replay = json.loads((args.root / args.tokens).read_text())
            negatives = []
            for case in replay["cases"]:
                value = query(case["surface"], case["route"], case["text"], case["credit"],
                              case["first"]["nextCursor"])
                assert value.get("answer") != "records", "obsolete selected root token admitted"
                assert typed_refusal(value), "typed public refusal required"
                negatives.append({"surface": case["surface"], "route": case["route"],
                                  "query": case["text"], "refusal": value})
            receipt["stale_selected_token_refusals"] = negatives
        if args.contract_negatives:
            replay = json.loads((args.root / args.tokens).read_text())
            negatives = []
            for case in replay["cases"]:
                token = case["first"]["nextCursor"]
                if case["surface"] == "mcp":
                    assert int(token.split("-", 2)[1]) > time.time() + 30, "do not confuse expiry with contract refusal"
                variants = [("family", "search" if case["route"] == "resolve" else "resolve", case["text"], 3, token),
                            ("text", case["route"], case["text"] + "-wrong", 3, token),
                            ("credit", case["route"], case["text"], 4, token),
                            ("truncated", case["route"], case["text"], 3, token[:-1]),
                            ("padding_alias", case["route"], case["text"], 3, token + "="),
                            ("token_budget", case["route"], case["text"], 3, token + "A" * (48 * 1024 + 1))]
                if case["surface"] == "cli":
                    assert token.startswith("pc3-")
                    encoded = token[4:]
                    decoder = zlib.decompressobj()
                    body = decoder.decompress(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)), 262145)
                    assert len(body) <= 262144 and decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail
                    def packed(raw):
                        return "pc3-" + base64.urlsafe_b64encode(zlib.compress(raw)).decode().rstrip("=")
                    changed = json.loads(body)
                    changed["command"]["data"]["cursor"]["root"] = "00" * 32
                    variants += [("extra_json", case["route"], case["text"], 3, packed(body + b"{}")),
                                 ("wrong_root", case["route"], case["text"], 3, packed(json.dumps(changed).encode())),
                                 ("expanded_budget", case["route"], case["text"], 3, packed(b"0" * 262145))]
                for label, route, text, credit, altered in variants:
                    value = query(case["surface"], route, text, credit, altered)
                    assert typed_refusal(value), (label, value)
                    negatives.append({"surface": case["surface"], "route": case["route"],
                                      "query": case["text"], "negative": label, "typed_refusal": value})
            receipt["public_contract_negatives"] = negatives
        if args.verify_selected:
            replay = json.loads((args.root / args.tokens).read_text())
            for case in replay["cases"]:
                value = reference(case["surface"], case["route"], case["text"])
                assert value.get("answer") == "records" and not value.get("more"), value
                assert list(map(identity, value["records"])) == list(map(identity, case["expected"])), "exact valid selected rows and order"
            receipt["selected_full_rows_equal"] = True
        if args.require_unicode_hit:
            answers = {surface: reference(surface, "search", "λ路径")
                       for surface in ("cli", "mcp")}
            for value in answers.values():
                assert value.get("answer") == "records" and value.get("records"), "positive Unicode search witness"
            assert list(map(identity, answers["cli"]["records"])) == list(map(identity, answers["mcp"]["records"]))
            receipt["positive_unicode_cli_mcp_answers"] = answers
        if inactive:
            assert inventory(inactive) == inactive_before, "unproven inactive bytes and entries retained exactly"
            receipt["inactive_inventory_retained_exactly"] = True
        if selected_binding:
            assert selected_binding.read_bytes() == selected_stamp, "selected corruption must rebuild exact original binding"
            receipt["selected_binding_rebuilt_exactly"] = True
        if args.matrix:
            matrix = run_matrix(query)
            (phase / "matrix.json").write_text(json.dumps(matrix, ensure_ascii=False, indent=2) + "\n")
            receipt["matrix_all_pass"] = matrix["all_pass"]
            receipt["matrix_cases"] = len(matrix["cases"])
            assert matrix["all_pass"], "full public page matrix failed; retained every case"
        if args.surface_checks:
            checks = []
            for text in ("cached", "__getitem__", "λ路径", "nohitλ路径"):
                code, default = cli("default-search-" + hashlib.sha256(text.encode()).hexdigest()[:8], ["search", text])
                assert code == 0 and default.get("answer") == "records", default
                # Public DEFAULT_LIMIT is 25; check both surfaces explicitly.
                explicit = query("cli", "search", text, 25, None)
                assert list(map(identity, default["records"])) == list(map(identity, explicit["records"]))
                mcp_default_reply = rpc("tools/call", {"name": "backend.search", "arguments":
                                        {"query": text, "detail": "full"}})
                mcp_default = mcp_default_reply.get("result", {}).get("structuredContent", mcp_default_reply)
                mcp_explicit = query("mcp", "search", text, 25, None)
                assert mcp_default.get("answer") == "records", mcp_default
                assert list(map(identity, mcp_default["records"])) == list(map(identity, mcp_explicit["records"]))
                assert list(map(identity, default["records"])) == list(map(identity, mcp_default["records"]))
                checks.append({"default_search": text, "rows": len(default["records"]),
                               "surfaces": ["cli", "mcp"], "pass": True})
            replay = json.loads((args.root / args.tokens).read_text())
            coordinates = list(dict.fromkeys(row["identity"]["coordinate"]
                                            for case in replay["cases"] for row in case["expected"]
                                            if row.get("identity", {}).get("path", "").startswith("src/")))[:5]
            for number, coordinate in enumerate(coordinates):
                code, plain = cli("plain-graph-%02d" % number, ["graph", coordinate])
                assert code == 0 and plain.get("answer") == "records", plain
                full = reference("mcp", "graph", coordinate)
                assert full.get("answer") == "records" and not full.get("more"), full
                assert list(map(identity, plain["records"])) == list(map(identity, full["records"])), "plain CLI/MCP graph full rows"
                checks.append({"coordinate": coordinate, "plain_rows": len(plain["records"]),
                               "paged": [traverse(query, "mcp", "graph", coordinate, credit, plain["records"])
                                         for credit in (1, 3, 7)], "pass": True})
            receipt["default_search_and_graph_checks"] = checks
        if args.catalog_probe:
            checks = []
            for text in ("cachetools", "cached", "__getitem__", "pkg:pypi/cachetools@7.2.1", "λ路径"):
                code, value = cli("catalog-" + hashlib.sha256(text.encode()).hexdigest()[:8],
                                  ["index-search", text, "--limit", "3"])
                reply = rpc("tools/call", {"name": "backend.index_search", "arguments":
                                           {"query": text, "limit": 3, "detail": "full"}})
                structured = reply.get("result", {}).get("structuredContent", reply)
                checks.append({"query": text, "cli_exit": code, "cli": value, "mcp": structured})
            receipt["actual_catalog_probe"] = checks
        if args.recapture_to:
            # Preserve early issued tokens, but issue a separate fresh set
            # after long matrices so the next cold test can distinguish
            # restart continuity from public token expiry.
            capture_tokens(args.recapture_to)
        receipt["all_phase_checks_pass"] = True
    except BaseException as error:
        receipt["error"] = repr(error)
        raise
    finally:
        done.set()
        for child in (client, owner):
            if child and child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try: child.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL); child.wait(timeout=10)
            if child: record("owned-child-retired", pid=child.pid, exit=child.returncode)
        receipt["available_memory_samples"] = memory
        receipt["finished_at_unix"] = time.time()
        receipt["elapsed_seconds"] = receipt["finished_at_unix"] - started
        if held_reader:
            fcntl.flock(held_reader, fcntl.LOCK_UN); held_reader.close()
        if inactive:
            receipt["inactive_after"] = inventory(inactive) if inactive.exists() else None
        (phase / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n")
        wire.close()
        for file in files: file.close()
        permit.close()
        print(json.dumps({"receipt": str(phase / "receipt.json"), "pass": receipt["all_phase_checks_pass"]}), flush=True)


if __name__ == "__main__":
    main()
