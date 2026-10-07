#!/usr/bin/env python3
"""Admit and launch one private actual phase; never supplies acceptance credit."""
import argparse
import base64
import json
from pathlib import Path
import shlex
import subprocess
import sys

HERE = Path(__file__).resolve().parent
PYTHON = "/nix/store/llk2h8rxqzv7zh53bi413ffibjrxskxw-python3-3.14.6/bin/python3"
B = "/Users/rmccrar6/nudox-functional-corpus-20261006/sol-fresh-remote-runtime"
R = B + "/runs/sol61-fe26-native-cachetools-20261007T0016Z"
SSH = ["ssh", "-F", "/Users/mileswirht/.ssh/config_external", "-o", "BatchMode=yes", "h16001mac"]
SCP = ["scp", "-F", "/Users/mileswirht/.ssh/config_external", "-o", "BatchMode=yes"]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("phase")
    args = parser.parse_args()
    plan = json.loads((HERE / "fe26-acceptance-plan.json").read_text())
    phases = plan["phases"]
    index = next(i for i, row in enumerate(phases) if row["phase"] == args.phase)
    row = phases[index]
    assert index > 0, "initial owner was already launched; never repeat first publication"
    previous = phases[index - 1]["phase"]
    check = "import json,pathlib; p=pathlib.Path(" + repr(R + "/" + previous + "/receipt.json") + "); v=json.loads(p.read_text()); "
    if args.phase == "removed-verify":
        # The original failed observer is retained. Its removal request was
        # accepted, then it wrongly demanded readable removed-project history.
        check += ("assert v['all_phase_checks_pass'] is False; "
                  "assert v['lifecycle_change']['heading']=='remove'; "
                  "assert v['lifecycle_change']['records'][0]['title']=='remove request accepted'; "
                  "assert v['history_after_change']=={'slug':'invalid-query','operand':'','cause':'malformed',"
                  "'detail':'semantic publication unavailable: project authority','answer':'fault'}")
    else:
        check += "assert v['all_phase_checks_pass'] is True"
    subprocess.run([*SSH, PYTHON + " -B -c " + shlex.quote(check)], check=True)
    evidence = HERE / "fe26-native-acceptance"
    proof = evidence / (args.phase + "-fleet.json")
    assert not proof.exists(), "retain original admission samples; choose another attempt explicitly"
    with (evidence / (args.phase + "-fleet.stdout")).open("wb") as output:
        subprocess.run([PYTHON, "-B", "/Users/mileswirht/Downloads/nudox-active-20261005/corpus-control/.config/scripts/fleet-capacity-census.py",
                        "--destination", "h16001mac", "--jobs", "1", "--allow-local-over-cap-for-remote",
                        "--output", str(proof)], stdout=output, check=True)
    census = json.loads(proof.read_text())
    assert census["advisory_allowed"] is True, census["reasons"]
    remote_proof = B + "/artifacts/sol61-fe26-" + args.phase + "-fleet.json"
    subprocess.run([*SCP, str(proof), "h16001mac:" + remote_proof], check=True)
    argv = [PYTHON, "-B", B + "/artifacts/mac_public_phase.py", "--root", R,
            "--manifest", B + "/receipts/canonical-release-fe26d8326a-attempt01/runtime-build-manifest.json",
            "--portable-root", B + "/artifacts/cli-canonical-fe26-macos-arm64-v1/nudox-macos-arm64",
            "--expected-source", plan["expected_source"], "--fleet-proof", remote_proof,
            "--phase", args.phase, *plan["common_flags"], *row["flags"]]
    encoded = base64.b64encode(json.dumps(argv).encode()).decode()
    launch = ("import subprocess,json,base64; argv=json.loads(base64.b64decode(" + repr(encoded) + ")); "
              "p=subprocess.Popen(argv,stdin=subprocess.DEVNULL,stdout=open(" + repr(B + "/artifacts/sol61-fe26-" + args.phase + ".stdout") + ", 'wb'),"
              "stderr=open(" + repr(B + "/artifacts/sol61-fe26-" + args.phase + ".stderr") + ", 'wb'),start_new_session=True); "
              "print(json.dumps({'observer_pid':p.pid}))")
    completed = subprocess.run([*SSH, PYTHON + " -B -c " + shlex.quote(launch)], capture_output=True, check=True)
    result = json.loads(completed.stdout)
    result.update(phase=args.phase, root=R, argv=argv, fleet_sampled_at_utc=census["sampled_at_utc"])
    (evidence / (args.phase + "-launch.json")).write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
