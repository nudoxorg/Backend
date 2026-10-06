#!/usr/bin/env python3
"""Acquire or reuse official immutable archives and stage source-only dossiers.

No package code, dependency scripts, compiler, or Nudox product is executed.
Every dossier remains not_run until an exact matched runtime closes its gates.
"""
import argparse
import ast
import concurrent.futures
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import tarfile
import time
import traceback
import urllib.parse
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("--output", required=True, type=Path)
parser.add_argument("--corpus", required=True, type=Path)
parser.add_argument("--origin-verifier", required=True, type=Path)
parser.add_argument("--finalize-existing", action="store_true", help="Verify retained archives and trees without network or extraction; write missing dossiers and a separate attempt summary.")
parser.add_argument("--case", action="append", help="Explicit ecosystem:package case; omitting this selects the original framework cohort.")
args = parser.parse_args()
if args.finalize_existing:
    assert args.output.is_dir()
else:
    args.output.mkdir(parents=True, exist_ok=False)
spec = importlib.util.spec_from_file_location("registry_origin", args.origin_verifier)
origin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(origin)

def sha(value):
    return hashlib.sha256(value).hexdigest()

def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()

def fetch(url, path, maximum):
    started = time.monotonic()
    with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "nudox-authorized-corpus-source-acceptance/1"}), timeout=45) as reply:
        data = reply.read(maximum + 1)
        assert len(data) <= maximum
        fact = {"url": url, "final_url": reply.url, "status": reply.status, "seconds": time.monotonic() - started, "bytes": len(data), "sha256": sha(data), "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    path.write_bytes(data)
    return data, fact

def resource_sample():
    available = int(next(row.split()[1] for row in Path("/proc/meminfo").read_text().splitlines() if row.startswith("MemAvailable:")))
    free = shutil.disk_usage(args.output).free
    assert available >= 24 * 1024 * 1024 and free >= 24 * 1024 ** 3
    return {"memory_available_kib": available, "free_disk_bytes": free}

def expectations(root, ecosystem, name):
    preferred = {"attrs": {"define", "field"}, "flask": {"Flask", "wsgi_app"}, "fastapi": {"FastAPI", "openapi"}, "pydantic": {"BaseModel", "model_validate"}, "zope-event": {"notify"}, "fastify": {"FastifyInstance", "FastifyRequest"}, "rxjs": {"Observable", "Subscriber"}, "zod": {"ZodType", "ZodObject"}, "vite": {"Plugin", "defineConfig"}, "svelte": {"Component", "mount"}, "astro": {"AstroIntegration", "defineConfig"}, "typing-extensions": {"Protocol", "TypedDict", "ParamSpec", "TypeAliasType"}, "click": {"Command", "Group", "command", "option"}, "blinker": {"Signal", "send", "send_async"}, "itsdangerous": {"Serializer", "URLSafeSerializer", "Signer", "sign", "unsign"}, "packaging": {"Version", "Requirement", "parse"}, "idna": {"encode", "decode", "check_label"}, "sniffio": {"current_async_library", "AsyncLibraryNotFoundError"}, "certifi": {"where", "contents"}, "immer": {"Immer", "Draft", "WritableDraft", "Patch"}, "tslib": {"__awaiter", "__generator", "__importStar", "__importDefault"}}.get(name, {"cachetools": {"Cache", "TTLCache", "cached", "cachedmethod"}, "h11": {"Connection", "Request", "Response", "send", "receive_data", "next_event"}, "toolz": {"compose", "curry", "mapcat", "memoize"}, "decorator": {"decorator", "decorate", "FunctionMaker"}}.get(name, set()))
    found = []
    for file in sorted(root.rglob("*")):
        if not file.is_file() or file.suffix not in {".py", ".ts", ".tsx"}:
            continue
        relative = file.relative_to(root).as_posix()
        if any(part in {"tests", "test", "docs", "benchmarks", "examples"} for part in file.relative_to(root).parts):
            continue
        raw = file.read_bytes()
        if ecosystem == "pypi":
            try:
                tree = ast.parse(raw)
            except (SyntaxError, UnicodeError):
                continue
            starts = [0]
            for line in raw.splitlines(keepends=True):
                starts.append(starts[-1] + len(line))
            for node in ast.walk(tree):
                if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and node.name in preferred:
                    start = starts[node.lineno - 1] + node.col_offset
                    end = starts[node.end_lineno - 1] + node.end_col_offset
                    found.append({"name": node.name, "path": relative, "line": node.lineno, "end_line": node.end_lineno, "kind": type(node).__name__, "file_sha256": sha(raw), "start_byte": start, "end_byte": end, "declaration_sha256": sha(raw[start:end]), "expectation_authority": "actual source AST; compiler binding/reference confidence not yet observed"})
        else:
            for match in re.finditer(rb"(?m)^\s*(?:export\s+)?(?:declare\s+)?(?:abstract\s+)?(?:class|interface|function|type)\s+([A-Za-z_$][A-Za-z0-9_$]*)", raw):
                symbol = match[1].decode()
                if symbol in preferred:
                    found.append({"name": symbol, "path": relative, "line": raw[:match.start(1)].count(b"\n") + 1, "file_sha256": sha(raw), "name_start_byte": match.start(1), "name_end_byte": match.end(1), "name_sha256": sha(match[1]), "expectation_authority": "actual declaration token; compiler binding/reference confidence not yet observed"})
    return found[:32]

def prepare(ecosystem, name):
    sample = resource_sample()
    out = args.output / (ecosystem + "-" + name)
    if args.finalize_existing:
        assert out.is_dir()
    else:
        out.mkdir()
    requests = []
    reused = False
    metadata_path, archive_path = out / "registry-metadata.json", out / "archive.tar.gz"
    if ecosystem == "npm":
        seed = args.corpus / "catalog/npm-pin-seed100-20261006T0457Z"
        for file in (seed / "metadata").glob("*.json"):
            value = json.loads(file.read_bytes())
            if value.get("name") == name:
                candidate = seed / "tarballs" / (file.stem + ".tgz")
                if candidate.is_file():
                    if args.finalize_existing:
                        assert metadata_path.read_bytes() == file.read_bytes()
                        assert archive_path.read_bytes() == candidate.read_bytes()
                    else:
                        metadata_path.write_bytes(file.read_bytes())
                        archive_path.write_bytes(candidate.read_bytes())
                    reused = True
                    requests.append({"reused_metadata": str(file), "reused_archive": str(candidate), "original_acquisition_directory": str(seed)})
                    break
        if not reused and not args.finalize_existing:
            _, fact = fetch("https://registry.npmjs.org/" + urllib.parse.quote(name, safe="") + "/latest", metadata_path, 8 * 1024 ** 2)
            requests.append(fact)
        metadata = json.loads(metadata_path.read_bytes())
        version = metadata["version"]
        metadata_url = "https://registry.npmjs.org/" + urllib.parse.quote(name, safe="") + ("/" + version if reused else "/latest")
        archive_url = metadata["dist"]["tarball"]
        prefix = "package/"
        dependencies = {"dependencies": metadata.get("dependencies", {}), "peerDependencies": metadata.get("peerDependencies", {}), "devDependencies": metadata.get("devDependencies", {})}
    else:
        metadata_url = "https://pypi.org/pypi/" + name + "/json"
        if not args.finalize_existing:
            _, fact = fetch(metadata_url, metadata_path, 16 * 1024 ** 2)
            requests.append(fact)
        metadata = json.loads(metadata_path.read_bytes())
        version = metadata["info"]["version"]
        matches = [item for item in metadata["releases"][version] if item["packagetype"] == "sdist"]
        assert len(matches) == 1
        archive_url = matches[0]["url"]
        dependencies = {"requires_dist": metadata["info"].get("requires_dist") or []}
    if not reused and not args.finalize_existing:
        _, fact = fetch(archive_url, archive_path, 64 * 1024 ** 2)
        requests.append(fact)
    archive = archive_path.read_bytes()
    if ecosystem == "pypi":
        with tarfile.open(archive_path) as tar:
            roots = {member.name.split("/", 1)[0] for member in tar.getmembers()}
        assert len(roots) == 1
        prefix = next(iter(roots)) + "/"
    files, special = origin.archive_members(archive, prefix, 20000, 32 * 1024 ** 2, 256 * 1024 ** 2)
    package = {"ecosystem": ecosystem, "id": name, "version": version}
    origin.verify_registry_metadata(package, metadata, metadata_url, archive, archive_url, special)
    source = out / "source"
    if args.finalize_existing:
        assert source.is_dir()
        requests.append({"finalization": "retained metadata/archive/source bytes; no network, extraction, or package execution", "initial_attempt_summary_sha256": sha((args.output / "summary.json").read_bytes())})
    else:
        source.mkdir()
        with tarfile.open(archive_path) as tar:
            for member in tar:
                if not member.isfile():
                    continue
                relative = member.name[len(prefix):]
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(tar.extractfile(member).read())
    observed = [{"path": path.relative_to(source).as_posix(), "bytes": path.stat().st_size, "sha256": sha(path.read_bytes())} for path in sorted(source.rglob("*")) if path.is_file()]
    # Archive membership orders complete POSIX strings, not Path components.
    observed.sort(key=lambda row: row["path"])
    assert observed == files and all(path.stat().st_nlink == 1 for path in source.rglob("*") if path.is_file())
    if args.finalize_existing and (out / "dossier.json").is_file():
        dossier = json.loads((out / "dossier.json").read_bytes())
        assert dossier["source_tree_sha256"] == sha(canonical(files))
        assert dossier["inventory_sha256"] == sha((out / "source-inventory.json").read_bytes())
        assert dossier["origin_sha256"] == sha((out / "registry-origin.json").read_bytes())
        print(json.dumps({"retained_dossier": str(out / "dossier.json"), "verified_source_files": len(files)}), flush=True)
        return dossier
    inventory = {"schema": "nudox.acquired-package-source-inventory.v1", "package": package, "source_root": str(source), "files": files}
    (out / "source-inventory.json").write_bytes(canonical(inventory) + b"\n")
    binding = {"schema": "nudox.registry-artifact-origin.v1", "package": package, "registry_metadata": {"path": str(metadata_path), "sha256": sha(metadata_path.read_bytes()), "url": metadata_url}, "archive": {"path": str(archive_path), "sha256": sha(archive), "url": archive_url}, "unpack": {"strip_prefix": prefix}}
    (out / "registry-origin.json").write_bytes(canonical(binding) + b"\n")
    dossier = {"schema": "nudox.source-verified-stratified-dossier.v1", "package": package, "source_root": str(source), "source_tree_sha256": sha(canonical(files)), "source_files": len(files), "source_bytes": sum(item["bytes"] for item in files), "inventory_sha256": sha((out / "source-inventory.json").read_bytes()), "origin_sha256": sha((out / "registry-origin.json").read_bytes()), "archive_sha256": sha(archive), "origin_verified": True, "archives_reused": reused, "acquisition_requests": requests, "resource_before": sample, "dependency_requirements": dependencies, "dependency_closure_installed": False, "source_expectations": expectations(source, ecosystem, name), "runtime_acceptance_status": "not_run", "runtime_passes": 0, "package_code_executed": False, "observer_sha256": sha(Path(__file__).read_bytes()), "origin_verifier_sha256": sha(args.origin_verifier.read_bytes())}
    (out / "dossier.json").write_text(json.dumps(dossier, indent=2) + "\n")
    print(json.dumps({"package": package, "source_files": len(files), "source_bytes": dossier["source_bytes"], "expectations": len(dossier["source_expectations"]), "reused": reused, "dossier": str(out / "dossier.json")}), flush=True)
    return dossier

cases = [("npm", name) for name in ["fastify", "rxjs", "zod", "vite", "svelte", "astro"]] + [("pypi", name) for name in ["attrs", "flask", "fastapi", "pydantic", "zope-event"]]
if args.case:
    cases = [tuple(value.split(":", 1)) for value in args.case]
    assert all(len(case) == 2 and case[0] in {"npm", "pypi"} and re.fullmatch(r"[A-Za-z0-9._-]+", case[1]) for case in cases)
    assert len(set(cases)) == len(cases)
results, failures = [], []
with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
    futures = {pool.submit(prepare, *case): case for case in cases}
    for future in concurrent.futures.as_completed(futures):
        try:
            results.append(future.result())
        except Exception as error:
            failures.append({"case": futures[future], "error": type(error).__name__, "detail": str(error), "traceback": traceback.format_exc()})
            print(json.dumps(failures[-1]), flush=True)
summary = {"schema": "nudox.stratified-source-dossiers-summary.v1", "prepared": len(results), "runtime_passes": 0, "failures": failures, "cases": results, "offline_finalization": args.finalize_existing}
summary_path = args.output / ("summary-attempt02.json" if args.finalize_existing else "summary.json")
assert not summary_path.exists()
summary_path.write_text(json.dumps(summary, indent=2) + "\n")
