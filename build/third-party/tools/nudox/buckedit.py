"""Deterministic, line-oriented mutation of Starlark / BUCK files.

These helpers patch BUCK and ``.bzl`` files in place while preserving the
surrounding indentation and quote style.  Everything is written to be
*idempotent* — running the same mutation twice is a no-op — so the tooling can
re-run safely.

Public API
----------
- :func:`insert_into_crates_list` — pure: add ``"crate"`` to a ``crates=[...]``
  list in ``deps(...)``, alpha-ordered.
- :func:`insert_before_anchor` — pure: insert a line before the first line
  containing an anchor marker.
- :func:`ensure_member_in_rust_bzl` — file wrapper: register a workspace member
  in ``build/rust.bzl``'s ``_MEMBERS`` dict.
- :func:`ensure_alias_in_root_buck` — file wrapper: register a top-level alias in
  the root ``BUCK`` file.

Design note
-----------
We prefer simple line scanning over heavy regexes.  The BUCK files this tool
targets are hand-written but regular, so scanning for the ``crates`` argument
and its bracketed body is both robust and easy to reason about.
"""

from __future__ import annotations

import re
from pathlib import Path

from . import paths

# Matches a single double-quoted Starlark string item, capturing leading
# indentation and the unquoted value.  Used to read the existing items in a
# ``crates = [ ... ]`` block so a new item can be slotted in alphabetically
# while matching the block's indentation.
_STR_ITEM_RE = re.compile(r'^(?P<indent>\s*)"(?P<value>[^"]*)"\s*,?\s*$')


# ── crates=[...] insertion ────────────────────────────────────────────────────

def insert_into_deps_list(buck_text: str, crate: str) -> str:
    """Insert ``"crate"`` alphabetically into the first ``deps([...])`` list.

    Prefers the cargo.toml-style ``deps([...])`` form introduced in
    ``build/rust.bzl``.  Falls back to legacy ``crates=[...]`` when no list
    form is found.
    """
    lines = buck_text.splitlines(keepends=True)
    open_idx = _find_deps_open(lines)
    if open_idx is not None:
        close_idx = _find_list_close_from(lines, open_idx, r"deps\s*=\s*\[")
        if close_idx is not None:
            return _insert_string_into_list(lines, open_idx, close_idx, crate, r"deps\s*=\s*\[")

    return insert_into_crates_list(buck_text, crate)


def _find_deps_open(lines: list[str]) -> int | None:
    pat = re.compile(r"(^|\W)deps\s*=\s*\[")
    for i, line in enumerate(lines):
        if pat.search(line):
            return i
    return None


def _find_list_close_from(lines: list[str], open_idx: int, open_pat: str) -> int | None:
    depth = 0
    started = False
    for i in range(open_idx, len(lines)):
        line = lines[i]
        start_col = 0
        if i == open_idx:
            m = re.search(open_pat, line)
            if m:
                start_col = m.end() - 1
        for ch in line[start_col:]:
            if ch == "[":
                depth += 1
                started = True
            elif ch == "]":
                depth -= 1
                if started and depth == 0:
                    return i
    return None


def _insert_string_into_list(
    lines: list[str], open_idx: int, close_idx: int, value: str, open_pat: str
) -> str:
    body_start = open_idx + 1
    item_lines: list[tuple[int, str, str]] = []
    for i in range(body_start, close_idx):
        m = _STR_ITEM_RE.match(lines[i])
        if m:
            item_lines.append((i, m.group("indent"), m.group("value")))

    if close_idx == open_idx or not item_lines:
        return _insert_inline_with_pat(lines, open_idx, close_idx, value, open_pat) or "".join(lines)

    if any(v == value for _, _, v in item_lines):
        return "".join(lines)

    indent = item_lines[0][1]
    insert_at = close_idx
    for line_index, _, existing in item_lines:
        if value < existing:
            insert_at = line_index
            break

    newline_char = "\r\n" if lines[item_lines[0][0]].endswith("\r\n") else "\n"
    lines.insert(insert_at, f'{indent}"{value}",{newline_char}')
    return "".join(lines)


def _insert_inline_with_pat(
    lines: list[str], open_idx: int, close_idx: int, value: str, open_pat: str
) -> str | None:
    if close_idx != open_idx:
        return None
    line = lines[open_idx]
    m = re.search(rf"({open_pat})(?P<body>.*?)(\])", line)
    if not m:
        return None
    body = m.group("body")
    items = [tok.strip() for tok in body.split(",")]
    values = [tok[1:-1] for tok in items if len(tok) >= 2 and tok[0] == '"']
    if value in values:
        return "".join(lines)
    values.append(value)
    values.sort()
    new_body = ", ".join(f'"{v}"' for v in values)
    new_line = line[: m.start(2)] + new_body + line[m.end(2) :]
    lines[open_idx] = new_line
    return "".join(lines)


def insert_into_crates_list(buck_text: str, crate: str) -> str:
    """Insert ``"crate"`` alphabetically into the first ``crates=[...]`` list.

    Finds the ``crates`` keyword argument (as it appears inside a ``deps(...)``
    call), then inserts ``crate`` in alphabetical order among the existing
    ``"..."`` string items, matching their indentation.

    Idempotent: if ``crate`` is already present in that list, the text is
    returned unchanged.

    Limitation: when a file contains multiple ``crates=[...]`` occurrences
    (e.g. ``workspace/server/BUCK`` has one in ``rust_crate`` and one in
    ``rust_tests``), only the FIRST occurrence — assumed to belong to the
    primary target — is modified.  Callers wanting the secondary lists updated
    must do so explicitly.
    """
    lines = buck_text.splitlines(keepends=True)

    # 1. Locate the line that opens the first `crates = [` argument.
    open_idx = _find_crates_open(lines)
    if open_idx is None:
        # No crates list to patch; leave the text untouched.
        return buck_text

    # 2. Find the matching close bracket for the list that opens on `open_idx`.
    close_idx = _find_list_close_from(lines, open_idx, r"crates\s*=\s*\[")
    if close_idx is None:
        return buck_text  # malformed / unterminated; refuse to guess.

    # 3. Scan the interior for existing quoted items (idempotency + ordering).
    #    We only treat lines that are *solely* a quoted item as list members so
    #    we don't trip over an inline `crates = ["a", "b"]` — handled below.
    body_start = open_idx + 1
    item_lines: list[tuple[int, str, str]] = []  # (line_index, indent, value)
    for i in range(body_start, close_idx):
        m = _STR_ITEM_RE.match(lines[i])
        if m:
            item_lines.append((i, m.group("indent"), m.group("value")))

    # Inline single-line form: `crates = ["a", "b"]` (open and close on one row,
    # or several items packed on the opening line).  Fall back to a regex-based
    # rewrite of that single line.
    if close_idx == open_idx or not item_lines:
        return _insert_inline(lines, open_idx, close_idx, crate) or buck_text

    # Idempotency: already present?
    if any(value == crate for _, _, value in item_lines):
        return buck_text

    indent = item_lines[0][1]

    # 4. Find the insertion point that keeps the list alphabetically ordered.
    #    We insert before the first existing item that sorts after `crate`.
    insert_at = close_idx  # default: just before the closing bracket line
    for line_index, _, value in item_lines:
        if crate < value:
            insert_at = line_index
            break

    new_line = f'{indent}"{crate}",\n'
    # Preserve whatever newline convention the surrounding lines use by copying
    # from a neighbouring item line when the default "\n" would mismatch.
    if item_lines and not item_lines[0][0] == insert_at:
        sample = lines[item_lines[0][0]]
        if sample.endswith("\r\n"):
            new_line = f'{indent}"{crate}",\r\n'

    lines.insert(insert_at, new_line)
    return "".join(lines)


def _find_crates_open(lines: list[str]) -> int | None:
    """Return the index of the first line containing a ``crates =`` argument."""
    # Match `crates = [` or `crates=[` possibly with the bracket on the same or
    # a following line.  We anchor on the keyword to avoid matching a crate
    # literally named "crates" inside some other list.
    pat = re.compile(r"(^|\W)crates\s*=\s*\[")
    for i, line in enumerate(lines):
        if pat.search(line):
            return i
    return None


def _find_list_close(lines: list[str], open_idx: int) -> int | None:
    """Return the index of the line closing the list opened on ``open_idx``.

    Tracks bracket depth starting from the first ``[`` on the opening line so
    nested lists inside the block don't confuse the matcher.  If the list opens
    and closes on the same line, returns ``open_idx``.
    """
    depth = 0
    started = False
    for i in range(open_idx, len(lines)):
        line = lines[i]
        # On the opening line, only start counting from the `crates = [` bracket.
        start_col = 0
        if i == open_idx:
            m = re.search(r"crates\s*=\s*\[", line)
            if m:
                start_col = m.end() - 1  # index of the `[`
        for ch in line[start_col:]:
            if ch == "[":
                depth += 1
                started = True
            elif ch == "]":
                depth -= 1
                if started and depth == 0:
                    return i
    return None


def _insert_inline(
    lines: list[str], open_idx: int, close_idx: int, crate: str
) -> str | None:
    """Handle a single-line ``crates = [ "a", "b" ]`` list.

    Returns the rewritten full text, or ``None`` if the inline shape could not
    be parsed (caller then falls back to leaving the text unchanged).
    Idempotent.
    """
    if close_idx != open_idx:
        return None  # not actually inline
    line = lines[open_idx]
    m = re.search(r"(crates\s*=\s*\[)(?P<body>.*?)(\])", line)
    if not m:
        return None
    body = m.group("body")
    items = [tok.strip() for tok in body.split(",")]
    values = [tok[1:-1] for tok in items if len(tok) >= 2 and tok[0] == '"']
    if crate in values:
        return "".join(lines)  # idempotent
    values.append(crate)
    values.sort()
    new_body = ", ".join(f'"{v}"' for v in values)
    new_line = line[: m.start(2)] + new_body + line[m.end(2) :]
    lines[open_idx] = new_line
    return "".join(lines)


# ── anchor insertion ──────────────────────────────────────────────────────────

def insert_before_anchor(text: str, anchor: str, new_line: str) -> str:
    """Insert ``new_line`` immediately before the first line containing ``anchor``.

    ``new_line`` is re-indented to match the anchor line's leading whitespace
    (its own leading whitespace is stripped first).  A trailing newline is added
    if missing.  Idempotent: if a line equal to the target (ignoring surrounding
    whitespace) already exists anywhere in the text, the text is returned
    unchanged.
    """
    lines = text.splitlines(keepends=True)

    stripped_new = new_line.strip()
    # Idempotency: bail if the exact content already exists.
    for line in lines:
        if line.strip() == stripped_new:
            return text

    for i, line in enumerate(lines):
        if anchor in line:
            indent = line[: len(line) - len(line.lstrip())]
            newline_char = "\r\n" if line.endswith("\r\n") else "\n"
            rendered = f"{indent}{stripped_new}{newline_char}"
            lines.insert(i, rendered)
            return "".join(lines)

    # Anchor not found: caller decides on a fallback.
    return text


# ── file-level convenience wrappers ───────────────────────────────────────────

def ensure_member_in_rust_bzl(member: str, target_label: str) -> None:
    """Register ``member -> target_label`` in ``build/rust.bzl``'s ``_MEMBERS``.

    Inserts ``    "{member}":  "{target_label}",`` before the ``# nudox:members``
    anchor.  If the anchor is missing, falls back to inserting before the closing
    brace of the ``_MEMBERS = {`` block.  Idempotent — a no-op if ``member`` is
    already a key in the dict.
    """
    path: Path = paths.RUST_BZL
    text = path.read_text(encoding="utf-8")

    # Idempotency: already a key? (`"member":` appears in the _MEMBERS block.)
    if re.search(rf'^\s*"{re.escape(member)}"\s*:', text, flags=re.MULTILINE):
        return

    entry = f'    "{member}":  "{target_label}",'
    anchor = "# nudox:members"

    if anchor in text:
        new_text = insert_before_anchor(text, anchor, entry)
    else:
        new_text = _insert_before_members_close(text, entry)

    if new_text != text:
        path.write_text(new_text, encoding="utf-8")


def _insert_before_members_close(text: str, entry: str) -> str:
    """Insert ``entry`` before the closing ``}`` of the ``_MEMBERS = {`` block.

    Fallback used only when the ``# nudox:members`` anchor is absent.  Scans from
    the ``_MEMBERS = {`` line, tracking brace depth, and inserts on the line that
    closes the dict.
    """
    lines = text.splitlines(keepends=True)
    open_idx = None
    for i, line in enumerate(lines):
        if re.search(r"_MEMBERS\s*=\s*\{", line):
            open_idx = i
            break
    if open_idx is None:
        return text

    depth = 0
    started = False
    for i in range(open_idx, len(lines)):
        for ch in lines[i]:
            if ch == "{":
                depth += 1
                started = True
            elif ch == "}":
                depth -= 1
                if started and depth == 0:
                    newline_char = "\r\n" if lines[i].endswith("\r\n") else "\n"
                    rendered = entry.rstrip("\n") + newline_char
                    lines.insert(i, rendered)
                    return "".join(lines)
    return text


def ensure_alias_in_root_buck(name: str, actual: str) -> None:
    """Register an ``alias(name=..., actual=..., visibility=["PUBLIC"])`` line.

    Writes to ``paths.ROOT_BUCK``.  The line is inserted before a
    ``# nudox:aliases`` anchor when present, otherwise appended at end of file.
    The root BUCK file is created if it does not yet exist.  Idempotent — a
    no-op if an alias with the same ``name`` already exists.
    """
    path: Path = paths.ROOT_BUCK
    text = path.read_text(encoding="utf-8") if path.exists() else ""

    # Idempotency: alias(name = "<name>" already present?
    if re.search(rf'alias\(\s*name\s*=\s*"{re.escape(name)}"', text):
        return

    alias_line = f'alias(name = "{name}", actual = "{actual}", visibility = ["PUBLIC"])'
    anchor = "# nudox:aliases"

    if anchor in text:
        new_text = insert_before_anchor(text, anchor, alias_line)
    else:
        if text and not text.endswith("\n"):
            text += "\n"
        new_text = text + alias_line + "\n"

    path.write_text(new_text, encoding="utf-8")
