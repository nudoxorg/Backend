#!/usr/bin/env python3
"""Build the artifact-backed VM test on a Nix builder with KVM support."""

import argparse
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys
import tempfile

FLAKE = "github:nudoxorg/keel/c1ec0936f7473319992d5227c6b23d7cc836a45d"
FLAKE_NAR = "sha256-tuetFsHeyVP2NvnEkM14AuA8hvYwcNh39fFh1RjHI6U="


def output(*command, **kwargs):
    return subprocess.check_output(command, text=True, **kwargs).strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--verifier", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        raise ValueError("output path already exists")
    args.output.mkdir(parents=True)
    try:
        return run(args)
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        (args.output / "infrastructure-error.json").write_text(json.dumps({"status": "infrastructure-error", "error": str(error)}) + "\n")
        print(f"keel-vm: {error}", file=sys.stderr)
        return 125


def run(args):
    source = args.source.resolve(strict=True)
    bundle = args.bundle.resolve(strict=True)
    verifier = args.verifier.resolve(strict=True)
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_sha):
        raise ValueError("source SHA must be a full lowercase Git SHA")
    if output("git", "-C", str(source), "rev-parse", "HEAD") != args.source_sha:
        raise ValueError("checkout HEAD does not match source SHA")
    if output("git", "-C", str(source), "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("source checkout is not clean")
    source_ref = "git+" + source.as_uri() + "?rev=" + args.source_sha
    source_metadata = json.loads(output("nix", "flake", "metadata", "--json", source_ref))
    if source_metadata.get("locked", {}).get("rev") != args.source_sha:
        raise ValueError("source flake did not resolve to the requested Git revision")
    source_store = source_metadata["path"]
    checked_verifier = Path(source_store, ".config/ci/t1_artifacts.py")
    if verifier.read_bytes() != checked_verifier.read_bytes():
        raise ValueError("published T1 verifier differs from exact-source verifier")
    subprocess.run([sys.executable, str(checked_verifier), "verify", str(bundle), "--source-sha", args.source_sha], check=True)
    t1 = json.loads((bundle / "manifest.json").read_bytes())
    metadata = json.loads(output("nix", "flake", "metadata", "--json", FLAKE))
    if (metadata.get("locked", {}).get("narHash") != FLAKE_NAR
            or metadata.get("locked", {}).get("rev") != "c1ec0936f7473319992d5227c6b23d7cc836a45d"):
        raise ValueError("public Keel flake narHash differs from the reviewed pin")

    with tempfile.TemporaryDirectory(prefix="keel-vm-") as work:
        work = Path(work)
        inventory = work / "manifest.json"
        with inventory.open("wb") as stream:
            subprocess.run(["nix", "eval", "--json", "--file", source_store + "/tests/keel/default.nix"], stdout=stream, check=True)
        inventory_store = output("nix", "store", "add-file", str(inventory))
        bundle_store = output("nix", "store", "add-path", str(bundle))
        subprocess.run([sys.executable, str(checked_verifier), "verify", bundle_store, "--source-sha", args.source_sha, "--import-runtime"], check=True)
        stored_t1 = json.loads(Path(bundle_store, "manifest.json").read_bytes())
        if stored_t1 != t1:
            raise ValueError("T1 manifest changed while staging")
        roots = t1["runtime_roots"]
        if not roots or any(not isinstance(root, str) or not root.startswith("/nix/store/") for root in roots):
            raise ValueError("invalid T1 runtime roots")
        closure = output("nix", "path-info", "--recursive", *roots).splitlines()
        if not closure:
            raise ValueError("T1 runtime closure is empty")
        for start in range(0, len(closure), 100):
            subprocess.run(["nix-store", "--verify-path", *closure[start:start + 100]], check=True)
        env = os.environ.copy()
        env.update({"KEEL_VM_SOURCE": source_store, "KEEL_VM_BUNDLE": bundle_store,
                    "KEEL_VM_INVENTORY": inventory_store, "KEEL_VM_SHA": args.source_sha,
                    "KEEL_VM_FLAKE": source_ref,
                    "KEEL_VM_ROOTS": json.dumps(roots), "KEEL_VM_NONCE": secrets.token_hex(16)})
        expression = '''
          let
            source = builtins.storePath (builtins.getEnv "KEEL_VM_SOURCE");
            flake = builtins.getFlake (builtins.getEnv "KEEL_VM_FLAKE");
            runtime = flake.packages.x86_64-linux.backend-service-runtime;
            pkgs = import flake.inputs.nixpkgs { system = "x86_64-linux"; };
            keel = builtins.getFlake "github:nudoxorg/keel/c1ec0936f7473319992d5227c6b23d7cc836a45d";
          in assert flake.rev == builtins.getEnv "KEEL_VM_SHA";
          assert toString flake.outPath == toString source;
          import (source + "/.config/ci/keel-vm-test.nix") {
            inherit pkgs;
            bundle = builtins.storePath (builtins.getEnv "KEEL_VM_BUNDLE");
            sourceSha = builtins.getEnv "KEEL_VM_SHA";
            inventory = builtins.storePath (builtins.getEnv "KEEL_VM_INVENTORY");
            sourceTree = source;
            driver = keel.packages.x86_64-linux.keel-nudox-harness;
            compilerEnvironment = runtime.compilerEnvironment;
            compilerPath = runtime.compilerPath;
            python = pkgs.python3;
            runtimeRoots = map builtins.storePath (builtins.fromJSON (builtins.getEnv "KEEL_VM_ROOTS"));
            runNonce = builtins.getEnv "KEEL_VM_NONCE";
          }
        '''
        result = output("nix", "build", "--option", "max-jobs", "0", "--impure", "--no-link", "--print-out-paths", "--expr", expression, env=env)
        if "\n" in result or not result.startswith("/nix/store/"):
            raise ValueError("unexpected Nix test output")
        shutil.copytree(result, args.output, dirs_exist_ok=True)
    if output("git", "-C", str(source), "rev-parse", "HEAD") != args.source_sha or output("git", "-C", str(source), "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("source checkout changed during the VM run")
    if (args.output / "guest-driver-error").exists():
        return 125
    status = args.output / "keel-results" / "guest-exit-status"
    if not status.is_file():
        return 125
    code = status.read_text().strip()
    return 0 if code == "0" else (1 if code == "1" else 125)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"keel-vm: {error}", file=sys.stderr)
        sys.exit(125)
