#!/usr/bin/env python3
"""Validate time-bounded emulation exclusions before test selection."""
from __future__ import annotations

import argparse
from datetime import date, datetime, timezone
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parent
MAX_DAYS = 30


def validate(document: dict, today: date) -> list[str]:
    entries = document.get("exclude")
    if not isinstance(entries, list) or not entries:
        return ["expected a nonempty exclude list"]
    errors = []
    seen = set()
    for number, entry in enumerate(entries, 1):
        prefix = f"exclusion {number}"
        if not isinstance(entry, dict):
            errors.append(f"{prefix}: expected a record")
            continue
        for field in ("filter", "category", "reason", "owner", "review"):
            if not isinstance(entry.get(field), str) or not entry[field].strip():
                errors.append(f"{prefix}: missing nonempty {field}")
        selection = entry.get("filter")
        if isinstance(selection, str):
            if selection in seen:
                errors.append(f"{prefix}: duplicate filter {selection}")
            seen.add(selection)
        try:
            reviewed = date.fromisoformat(entry["reviewed"])
            expires = date.fromisoformat(entry["expires"])
        except (KeyError, TypeError, ValueError):
            errors.append(f"{prefix}: reviewed/expires must be ISO YYYY-MM-DD strings")
            continue
        if reviewed > today:
            errors.append(f"{prefix}: reviewed date is in the future")
        if not 0 < (expires - reviewed).days <= MAX_DAYS:
            errors.append(f"{prefix}: expiry must be within {MAX_DAYS} days of review")
        if expires <= today:
            errors.append(f"{prefix}: expired {expires}; owner {entry.get('owner', '?')} must review {selection}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", nargs="?", choices=["windows", "arm64"])
    args = parser.parse_args()
    today = datetime.now(timezone.utc).date()
    failed = False
    for platform in [args.platform] if args.platform else ["windows", "arm64"]:
        path = ROOT / f"emulated-{platform}-exclusions.toml"
        try:
            with path.open("rb") as source:
                document = tomllib.load(source)
            errors = validate(document, today)
        except (OSError, tomllib.TOMLDecodeError) as error:
            errors = [str(error)]
        if errors:
            failed = True
            for error in errors:
                print(f"quarantine {platform}: {error}", file=sys.stderr)
        else:
            print(f"quarantine {platform}: {len(document['exclude'])} owned exclusions valid on {today}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
