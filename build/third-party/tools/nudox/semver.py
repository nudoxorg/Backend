"""Cargo-compatible semver parsing and requirement matching.

Public API
----------
- :func:`parse` — split a version into ``(major, minor, patch)``.
- :func:`satisfies` — does a version satisfy a Cargo-style requirement string?
"""

from __future__ import annotations

import re


def parse(version: str) -> tuple[int, int, int]:
    """Return ``(major, minor, patch)`` ignoring pre-release/build metadata."""
    clean = re.split(r"[.\-+]", version.lstrip("^~=<> "))
    parts: list[int] = []
    for p in clean:
        if not p:
            continue
        try:
            parts.append(int(p))
        except ValueError:
            parts.append(0)
        if len(parts) == 3:
            break
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts)  # type: ignore[return-value]


def satisfies(version: str, requirement: str) -> bool:
    """Does ``version`` satisfy the Cargo-style ``requirement`` string?

    Implements the most common forms used on crates.io:
        ^1.2.3 (or bare 1.2.3), ~1.2.3, >=1.2, <2.0, =1.0, and comma-AND lists.
    See https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html
    """
    if not requirement or not requirement.strip():
        return True

    requirement = requirement.strip()
    if "," in requirement:
        return all(
            satisfies(version, part.strip())
            for part in requirement.split(",")
        )

    match = re.match(r"^([<>=^~!]*)\s*(.+)$", requirement)
    if not match:
        return True

    operator, spec = match.group(1), match.group(2).strip()
    v_parts = parse(version)
    s_parts = parse(spec)
    spec_component_count = len([p for p in spec.split(".") if p])

    if operator in ("", "^"):
        # Caret semantics (default)
        if s_parts[0] != 0:
            return v_parts[0] == s_parts[0] and v_parts >= s_parts
        if s_parts[1] != 0:
            return (
                v_parts[0] == s_parts[0]
                and v_parts[1] == s_parts[1]
                and v_parts >= s_parts
            )
        return v_parts == s_parts

    if operator == "~":
        if spec_component_count >= 2:
            return (
                v_parts[0] == s_parts[0]
                and v_parts[1] == s_parts[1]
                and v_parts >= s_parts
            )
        return v_parts[0] == s_parts[0] and v_parts >= s_parts

    if operator == ">=":
        return v_parts >= s_parts
    if operator == ">":
        return v_parts > s_parts
    if operator == "<=":
        return v_parts <= s_parts
    if operator == "<":
        return v_parts < s_parts
    if operator in ("=", "=="):
        return all(v_parts[i] == s_parts[i] for i in range(spec_component_count))

    return True  # unknown operator → be permissive
