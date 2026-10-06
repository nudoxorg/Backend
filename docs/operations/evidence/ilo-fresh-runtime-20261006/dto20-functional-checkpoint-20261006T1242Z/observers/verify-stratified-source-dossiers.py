#!/usr/bin/env python3
"""Admit retained source dossiers through the frozen shared origin observer.

This does not execute package code, a compiler, or a product runtime.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import sys

parser = argparse.ArgumentParser()
parser.add_argument("--dossiers", required=True, type=Path)
parser.add_argument("--observer", required=True, type=Path)
parser.add_argument("--output", required=True, type=Path)
args = parser.parse_args()
assert not args.output.exists()
sys.path.insert(0, str(args.observer.parent))
spec = importlib.util.spec_from_file_location("frozen_corpus_observer", args.observer)
observer = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = observer
spec.loader.exec_module(observer)
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
summary = args.dossiers / "summary-attempt02.json"
proofs = []
for dossier_path in sorted(args.dossiers.glob("*/dossier.json")):
    dossier = json.loads(dossier_path.read_bytes())
    package = {**dossier["package"], "provenance": {"kind": "source-tree-sha256", "sha256": dossier["source_tree_sha256"]}}
    inventory, origin = dossier_path.parent / "source-inventory.json", dossier_path.parent / "registry-origin.json"
    acquisition = {"inventory_path": str(inventory), "inventory_sha256": sha(inventory), "target_subdir": ".", "origin": {"receipt_path": str(origin), "receipt_sha256": sha(origin)}}
    case = observer.ProjectCase(dossier_path.parent.name, Path(dossier["source_root"]), False, 1, (), package, acquisition)
    proof = observer.verify_acquired_source_inventory(case, sha(summary), observer.Deadline(60))
    assert proof["origin_verification"] == "verified-registry-artifact-v1"
    proofs.append({"dossier": str(dossier_path), "dossier_sha256": sha(dossier_path), "proof": proof})
    print(json.dumps({"package": dossier["package"], "source_inventory": proof["verification"], "origin": proof["origin_verification"]}), flush=True)
receipt = {"schema": "nudox.shared-observer-stratified-source-preflight.v1", "observer_path": str(args.observer), "observer_sha256": sha(args.observer), "dossier_summary_sha256": sha(summary), "cases": proofs, "package_code_executed": False, "runtime_passes": 0}
args.output.write_text(json.dumps(receipt, indent=2) + "\n")
