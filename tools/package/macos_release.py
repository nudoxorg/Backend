#!/usr/bin/env python3
"""Native Mac release driver: preflight, receipted build/package, sign/notarize."""
import argparse
import json
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

from release_contract import ASSET, sha256

HERE = Path(__file__).resolve().parent
MACHO = {b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca"}
INPUTS = {"cargo_runner", "cargo_bundle", "icon", "dotnet_root", "dotnet_receipt", "dotnet_pin", "roslyn_dir", "roslyn_receipt", "helpers_dir", "helpers_receipt"}


def run(command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


def preflight(config, stage="build"):
    problems = []
    if platform.system() != "Darwin" or platform.machine().lower() not in {"arm64", "aarch64"}:
        problems.append("Apple Silicon macOS build host required")
    source = Path(config["source_root"]).resolve()
    output = Path(config["output_dir"]).resolve()
    if source == output or source in output.parents:
        problems.append("release output must be outside the source checkout")
    if stage == "build" and output.exists():
        problems.append("release output directory must be new")
    parent = output.parent
    while not parent.exists():
        parent = parent.parent
    free = shutil.disk_usage(parent).free / 1024**3
    floor = max(20, config.get("disk_floor_gib", 40))
    if stage == "sign":
        app = output / "package/Nudox.app"
        total = sum(path.stat().st_size for path in app.rglob("*") if path.is_file() and not path.is_symlink())
        floor = max(2, 3 * total / 1024**3)
    if free < floor:
        problems.append(f"build volume has {free:.1f} GiB free; configured admission floor is {floor} GiB")
    for key in sorted(INPUTS):
        if not config.get(key) or not Path(config[key]).is_absolute() or not Path(config[key]).exists():
            problems.append(f"missing absolute release input: {key}")
    for key in ("expected_revision", "expected_tree", "expected_runner_sha256", "expected_icon_sha256", "minimum_os", "signing_identity", "notary_profile"):
        if not config.get(key):
            problems.append(f"missing release configuration: {key}")
    for tool in ("xcrun", "codesign", "security", "ditto", "otool", "spctl"):
        if shutil.which(tool) is None:
            problems.append(f"missing native distribution tool: {tool}")
    if not source.is_dir():
        problems.append("source checkout is missing")
    else:
        status = subprocess.check_output(["git", "-C", str(source), "status", "--porcelain"], text=True)
        if status:
            problems.append("source checkout must be clean and committed before release")
        actual = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
        tree = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD^{tree}"], text=True).strip()
        if actual != config.get("expected_revision") or tree != config.get("expected_tree"):
            problems.append("source checkout differs from selected revision/tree")
    if shutil.which("security"):
        identities = subprocess.check_output(["security", "find-identity", "-v", "-p", "codesigning"], text=True)
        identity = config.get("signing_identity", "")
        if not identity or not any(identity in line and "Developer ID Application:" in line for line in identities.splitlines()):
            problems.append("selected Developer ID Application identity is unavailable")
    if not problems:
        try:
            credentials = subprocess.run(["xcrun", "notarytool", "history", "--keychain-profile", config["notary_profile"], "--output-format", "json"], capture_output=True, timeout=30)
            if credentials.returncode != 0:
                problems.append("notarization profile could not authenticate; configure the keychain profile before building")
        except subprocess.TimeoutExpired:
            problems.append("notarization credential check timed out; retry before building")
    return {"ready": not problems, "free_gib": round(free, 1), "blockers": problems}


def prepare(config):
    admission = preflight(config)
    if not admission["ready"]:
        raise ValueError("release preflight failed: " + "; ".join(admission["blockers"]))
    source = Path(config["source_root"]).resolve()
    output = Path(config["output_dir"]).resolve()
    output.mkdir(parents=True)
    build = output / "build"
    package = output / "package"
    common = ["--source-root", str(source), "--expected-revision", config["expected_revision"], "--expected-tree", config["expected_tree"], "--expected-runner-sha256", config["expected_runner_sha256"]]
    run([sys.executable, str(HERE / "build-macos-investor-app.py"), *common, "--cargo-runner", config["cargo_runner"], "--output-dir", str(build)])
    arguments = [sys.executable, str(HERE / "macos-investor-bundle.py"), *common, "--artifact-dir", str(build / "artifacts"), "--build-receipt", str(build / "application-build-receipt.json"), "--output-dir", str(package), "--minimum-os", config["minimum_os"], "--expected-icon-sha256", config["expected_icon_sha256"]]
    for key in INPUTS - {"cargo_runner"}:
        arguments.extend(["--" + key.replace("_", "-"), config[key]])
    for key in ("dotnet_source_archive", "relocation_plan"):
        if config.get(key):
            arguments.extend(["--" + key.replace("_", "-"), config[key]])
    for value in config.get("relocation_package_roots", []):
        arguments.extend(["--relocation-package-root", value])
    run(arguments)
    finalize(config)


def finalize(config):
    admission = preflight(config, stage="sign")
    if not admission["ready"]:
        raise ValueError("signing preflight failed: " + "; ".join(admission["blockers"]))
    source = Path(config["source_root"]).resolve()
    output = Path(config["output_dir"]).resolve()
    package = output / "package"
    candidate = output / "candidate"
    if candidate.exists():
        raise ValueError("completed/partial candidate already exists; inspect it before retrying")
    app = package / "Nudox.app"
    evidence = json.loads((app / "Contents/Resources/build-manifest.json").read_text())
    if evidence["source"]["git_revision"] != config["expected_revision"] or evidence["source"]["git_tree"] != config["expected_tree"]:
        raise ValueError("packaged source differs from selected release")
    version = tomllib.loads((source / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is None:
        raise ValueError("source workspace version must be stable semver")
    entitlements = output / "runtime-entitlements.plist"
    entitlements.write_bytes(plistlib.dumps({"com.apple.security.cs.allow-jit": True}))
    for path in sorted(app.rglob("*"), key=lambda p: len(p.parts), reverse=True):
        if path.is_symlink() or not path.is_file():
            continue
        with path.open("rb") as stream:
            magic = stream.read(4)
        if magic not in MACHO:
            continue
        header = subprocess.check_output(["otool", "-hv", str(path)], text=True)
        command = ["codesign", "--force", "--timestamp", "--options", "runtime", "--sign", config["signing_identity"]]
        if "EXECUTE" in header:
            command += ["--entitlements", str(entitlements)]
        run(command + [str(path)])
    for nested in sorted(app.rglob("*"), key=lambda p: len(p.parts), reverse=True):
        if nested.is_dir() and not nested.is_symlink() and nested.suffix in {".framework", ".bundle", ".xpc", ".app"}:
            run(["codesign", "--force", "--timestamp", "--options", "runtime", "--sign", config["signing_identity"], str(nested)])
    run(["codesign", "--force", "--timestamp", "--options", "runtime", "--sign", config["signing_identity"], str(app)])
    run(["codesign", "--verify", "--deep", "--strict", str(app)])
    submission = output / "notarization.zip"
    run(["ditto", "-c", "-k", "--keepParent", str(app), str(submission)])
    result = run(["xcrun", "notarytool", "submit", str(submission), "--keychain-profile", config["notary_profile"], "--wait", "--output-format", "json"], stdout=subprocess.PIPE, text=True)
    notarization = json.loads(result.stdout)
    (output / "notarization.json").write_text(json.dumps(notarization, indent=2) + "\n")
    if notarization.get("status") != "Accepted":
        raise ValueError("notarization was not accepted; inspect notarization.json")
    run(["xcrun", "stapler", "staple", str(app)])
    run(["xcrun", "stapler", "validate", str(app)])
    run(["codesign", "--verify", "--deep", "--strict", str(app)])
    run(["spctl", "--assess", "--type", "execute", str(app)])
    candidate.mkdir()
    archive = candidate / ASSET
    run(["ditto", "-c", "-k", "--keepParent", str(app), str(archive)])
    build_manifest = app / "Contents/Resources/build-manifest.json"
    manifest = {"schema": 1, "version": version, "source_sha": config["expected_revision"], "source_tree": config["expected_tree"], "cargo_lock_sha256": sha256(source / "Cargo.lock"), "platform": "macos", "target": "aarch64-apple-darwin", "asset": ASSET, "sha256": sha256(archive), "size_bytes": archive.stat().st_size, "minimum_os": config["minimum_os"], "build_manifest_sha256": sha256(build_manifest), "signed": True, "notarized": True}
    (candidate / "release-manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    (candidate / (ASSET + ".sha256")).write_text(f'{manifest["sha256"]}  {ASSET}\n')
    print(json.dumps({"candidate": str(candidate), "version": version, "sha256": manifest["sha256"], "next": "test this archive natively and record native-qa.json before staging"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("preflight", "prepare", "finalize"))
    parser.add_argument("--config", required=True, type=Path)
    args = parser.parse_args()
    config = json.loads(args.config.read_text())
    if args.action == "preflight":
        result = preflight(config)
        print(json.dumps(result, indent=2))
        return 0 if result["ready"] else 2
    if args.action == "finalize":
        finalize(config)
    else:
        prepare(config)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Mac release stopped: {error}", file=sys.stderr)
        sys.exit(1)
