#!/usr/bin/env python3
"""Convert Buck2 BXL resolve_targets output to rust-project.json.

Called by gen-rust-project.sh — don't invoke directly.

Input:  path to the ExpandedAndResolved JSON written by
        prelude//rust/rust-analyzer:resolve_deps.bxl:resolve_targets
Output: rust-project.json written to stdout
"""

import sys
import json
import subprocess
from pathlib import Path


def sysroot_src() -> str | None:
    try:
        out = subprocess.run(
            ["rustc", "--print", "sysroot"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        # Nightly toolchains put library sources here
        candidate = Path(out) / "lib" / "rustlib" / "src" / "rust" / "library"
        return str(candidate) if candidate.exists() else None
    except Exception:
        return None


def convert(data: dict) -> dict:
    resolved: dict = data.get("resolved_deps", {})
    proc_macros: dict = data.get("queried_proc_macros", {})

    # Stable ordering: workspace members first, then deps alphabetically
    labels = sorted(
        resolved.keys(),
        key=lambda l: (0 if resolved[l].get("in_workspace") else 1, l),
    )
    idx: dict[str, int] = {l: i for i, l in enumerate(labels)}

    crates = []
    for label in labels:
        info = resolved[label]

        crate_root = info.get("crate_root")
        if not crate_root:
            continue

        deps = []
        for dep_label in info.get("deps", []):
            if dep_label not in idx:
                continue
            dep_info = resolved[dep_label]
            dep_name = dep_info.get("crate") or dep_info.get("name") or dep_label.split(":")[-1]
            deps.append({"crate": idx[dep_label], "name": dep_name})

        is_proc = bool(info.get("proc_macro"))
        dylib_path = None
        if is_proc and label in proc_macros:
            mo = proc_macros[label]
            dylib_path = mo.get("dylib") if isinstance(mo, dict) else None

        display = info.get("crate") or info.get("name") or label.split(":")[-1]

        crates.append({
            "display_name": display,
            "root_module": str(crate_root),
            "edition": info.get("edition") or "2021",
            "is_workspace_member": bool(info.get("in_workspace")),
            "deps": deps,
            "cfg": [],
            "env": info.get("env") or {},
            "is_proc_macro": is_proc,
            "proc_macro_dylib_path": dylib_path,
        })

    return {"sysroot_src": sysroot_src(), "crates": crates}


def main() -> None:
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <bxl-output-path>", file=sys.stderr)
        sys.exit(1)

    with open(sys.argv[1]) as f:
        data = json.load(f)

    json.dump(convert(data), sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
