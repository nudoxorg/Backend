"""Rebind a finite SDK assembly after inner signing, before the outer app seal."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path

from linux_release_package import parse_json_bytes, read_regular_bytes

spec = importlib.util.spec_from_file_location("signed_sdk_bundle", Path(__file__).with_name("macos-investor-bundle.py"))
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)

MANIFEST = "Contents/Resources/build-manifest.json"
RECEIPT = "Contents/Resources/Build Evidence/compiler-helpers-receipt.json"
ORIGIN = "Contents/Resources/Build Evidence/compiler-helpers-source-receipt.json"


def read_json(path, label):
    return parse_json_bytes(read_regular_bytes(path, 16 * 1024**2, label), label)


def admit(app, evidence, output):
    """Reject changed assembly inputs and retain their exact precursor bytes."""
    manifest_raw = read_regular_bytes(app / MANIFEST, 16 * 1024**2, "SDK assembly manifest")
    if parse_json_bytes(manifest_raw, "SDK assembly manifest") != evidence:
        raise ValueError("SDK assembly manifest changed before signing")
    observed = bundle.file_inventory(app)
    observed.pop(MANIFEST, None)
    if observed != evidence["files"]:
        raise ValueError("SDK assembly file inventory changed before signing")
    receipt_raw = read_regular_bytes(app / RECEIPT, 16 * 1024**2, "SDK assembly helper receipt")
    receipt = parse_json_bytes(receipt_raw, "SDK assembly helper receipt")
    helpers = evidence["compiler_helpers"]
    if (helpers.get("assembly_mode") != "typescript-sdk-only"
            or hashlib.sha256(receipt_raw).hexdigest() != helpers["receipt_sha256"]
            or receipt.get("files") != helpers["files"] or receipt.get("tools") != helpers["tools"]
            or receipt.get("source_receipt_sha256") != bundle.sha256(app / ORIGIN)
            or helpers.get("source_receipt_sha256") != bundle.sha256(app / ORIGIN)):
        raise ValueError("SDK assembly receipt differs from its manifest/origin binding")
    bundle.validate_sdk_helper_payload(receipt, app / "Contents/Resources/Helpers", evidence["source"], evidence["target"]["triple"])
    precursor = output / "sdk-assembly-evidence"
    precursor.mkdir()
    (precursor / "build-manifest.json").write_bytes(manifest_raw)
    (precursor / "compiler-helpers-receipt.json").write_bytes(receipt_raw)
    return {"manifest_sha256": hashlib.sha256(manifest_raw).hexdigest(),
            "receipt_sha256": hashlib.sha256(receipt_raw).hexdigest(), "receipt": receipt}


def refresh(app, evidence, precursor):
    """Authenticate signed bytes and probes; caller seals the outer app next."""
    receipt = copy.deepcopy(precursor["receipt"])
    payload = app / "Contents/Resources/Helpers"
    receipt["files"] = {name: bundle.admit_file_digest(payload / name, 512 * 1024**2)[1]
                        for name in receipt["files"]}
    if any(digest != precursor["receipt"]["files"][name]
           for name, digest in receipt["files"].items() if name != "typescript/node/bin/node"):
        raise ValueError("SDK package/notices changed during signing")
    receipt["tools"]["node"]["sha256"] = receipt["files"]["typescript/node/bin/node"]
    receipt["pre_sign_receipt_sha256"] = precursor["receipt_sha256"]
    receipt["runtime_probe_status"] = "pending post-sign native probes"
    bundle.validate_sdk_helper_payload(receipt, payload, evidence["source"], evidence["target"]["triple"])
    required = {f"Contents/MacOS/{name}" for name in bundle.EXECUTABLES}
    required.add("Contents/Resources/Helpers/typescript/node/bin/node")
    images = bundle.inspect_macho_tree(app, evidence["target"]["architecture"],
                                       evidence["bundle"]["minimum_macos"], required)
    original_images = {record["path"] for record in evidence["macho_images"]}
    if {record["path"] for record in images} != original_images or any(record["signature"] != "signed" for record in images):
        raise ValueError("post-sign SDK Mach-O set/signatures differ from admitted assembly")
    observed = bundle.file_inventory(app)
    observed.pop(MANIFEST, None)
    # Signing may change admitted Mach-O bytes and create signature sidecars.
    # Everything else, including the original SDK receipt, must remain exact.
    def signature_record(name):
        return "_CodeSignature" in Path(name).parts
    allowed = original_images | {RECEIPT}
    before = {name: value for name, value in evidence["files"].items()
              if name not in allowed and not signature_record(name)}
    after = {name: value for name, value in observed.items()
             if name not in allowed and not signature_record(name)}
    if before != after:
        raise ValueError("non-code SDK assembly bytes changed during signing/probes")
    probes = bundle.verify_sdk_runtime(payload, receipt)
    receipt["runtime_probe_status"] = "passed post-sign Node and Compiler API probes"
    (app / RECEIPT).write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    observed = bundle.file_inventory(app)
    observed.pop(MANIFEST, None)
    after = {name: value for name, value in observed.items()
             if name not in allowed and not signature_record(name)}
    if before != after:
        raise ValueError("non-code SDK assembly bytes changed during native probes")
    result = copy.deepcopy(evidence)
    result["compiler_helpers"].update(receipt_sha256=bundle.sha256(app / RECEIPT),
                                      files=receipt["files"], tools=receipt["tools"],
                                      runtime_probes=probes,
                                      runtime_probe_status=receipt["runtime_probe_status"])
    result["macho_images"] = images
    result["files"] = {name: value for name, value in observed.items()
                       if not name.startswith("Contents/_CodeSignature/")}
    result["file_inventory_scope"] = "all bundle files except this manifest and the subsequent outer Contents/_CodeSignature seal"
    result["sdk_finalization"] = {"assembly_build_manifest_sha256": precursor["manifest_sha256"],
                                  "assembly_helper_receipt_sha256": precursor["receipt_sha256"],
                                  "native_probes_stage": "after inner signing, before outer app seal"}
    result["bundle"]["signature"] = "outer seal is recorded by the release manifest"
    result["distribution_status"] = "signed inner images and SDK probes admitted; outer app signing/notarization and native installed QA pending"
    (app / MANIFEST).write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result
