"""Fail early when Nix cannot vendor a locked Git package.

This checks coverage and digest syntax; Nix verifies source contents on fetch.
"""

import base64
import json
import sys
import tomllib
from pathlib import Path


def validate(lock, hashes):
    errors = []
    for package in lock["package"]:
        if not package.get("source", "").startswith("git+"):
            continue
        key = f'{package["name"]}-{package["version"]}'
        digest = hashes.get(key, "")
        try:
            valid = digest.startswith("sha256-") and len(
                base64.b64decode(digest[7:], validate=True)
            ) == 32
        except (ValueError, TypeError):
            valid = False
        if not valid:
            errors.append(f"{key}: missing or invalid Nix Git source hash")
    return errors


if __name__ == "__main__":
    errors = validate(
        tomllib.loads(Path(sys.argv[1]).read_text()),
        json.loads(Path(sys.argv[2]).read_text()),
    )
    for error in errors:
        print(error, file=sys.stderr)
    sys.exit(bool(errors))
