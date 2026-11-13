#!/usr/bin/env python3
"""Verify an extracted local-development app against an admitted receipt.

This checks bytes and ad hoc signatures only; it does not confer release trust
or launch the app. The caller supplies the separately trusted receipt SHA-256.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import time

import macos_stylo_companion as companion
import record_macos_stylo_development_bundle as actor


def verify(app: Path, receipt_path: Path, expected_receipt: str) -> dict:
    if re.fullmatch(r'[0-9a-f]{64}', expected_receipt) is None:
        raise ValueError('invalid expected receipt hash')
    app = app.resolve(strict=True)
    if not app.is_dir() or app.suffix != '.app':
        raise ValueError('expected an application bundle directory')
    size, checksum = companion.digest_regular(receipt_path, 1024 * 1024)
    fd = os.open(receipt_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        raw = stream.read(1024 * 1024 + 1)
    if len(raw) != size or hashlib.sha256(raw).hexdigest() != checksum or checksum != expected_receipt:
        raise ValueError('receipt differs from expected hash')
    receipt = json.loads(raw)
    if receipt.get('schema') != actor.SCHEMA or receipt.get('exit') != 0:
        raise ValueError('unexpected development receipt')
    deadline = time.monotonic() + 120
    expected_bundle = receipt.get('bundle_digest')
    if actor.bundle_digest(app, deadline) != expected_bundle:
        raise ValueError('bundle bytes or modes differ from receipt')
    macos = app / 'Contents/MacOS'
    for image in (app, macos/'Nudox', macos/'backend-locald', macos/'backend-mcp', macos/'backend-cli', macos/'backend-stylo-worker', macos/'backend-sqlite-worker'):
        companion.run_tool(['/usr/bin/codesign', '--verify', '--strict', str(image)], deadline=deadline)
    if actor.bundle_digest(app, deadline) != expected_bundle:
        raise ValueError('bundle changed during verification')
    return {'scope':'ad hoc development bundle byte and signature verification; no distribution trust',
            'source':receipt['source'], 'app':str(app), 'receipt_sha256':checksum,
            'bundle_sha256':expected_bundle['sha256'], 'worker_anchor':receipt['worker_anchor'],
            'sqlite_worker_identity':receipt['sqlite_worker']['artifact_identity']}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    parser.add_argument('--receipt-sha256', required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args.app, args.receipt, args.receipt_sha256), indent=2))
        return 0
    except (ValueError, OSError) as error:
        print(f'development bundle refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
