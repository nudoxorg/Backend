"""Inventory tracked Rust ignore attributes without treating literals as code."""
from collections import Counter
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
CHAR = re.compile(r"'(?:\\(?:u\{[^}\n]+\}|x[0-9A-Fa-f]{2}|[^\n])|[^'\\\n])'")
IGNORE_PREFIX = re.compile(rb'#\s*!?\s*\[\s*(?:(?:r#)?(?:ignore|cfg_attr)\b|/)')
RAW = re.compile(r'(?:br|cr|r)(#{0,255})"')
TOKEN_START = re.compile(r"//|/\*|(?:br|cr|r)#{0,255}\"|\"|'")
MODULE = re.compile(r'\bmod\s+((?:r#)?[A-Za-z_]\w*)\s*([;{])|[{}]')


def mask_literals(source: str) -> str:
    """Preserve offsets/newlines; mask comments, strings and character literals."""
    result = list(source)
    length = len(source)
    cursor = 0
    while cursor < length:
        token = TOKEN_START.search(source, cursor)
        if token is None:
            break
        cursor = token.start()
        start = cursor
        end = None
        if source.startswith('//', cursor):
            end = source.find('\n', cursor)
            if end < 0:
                end = length
        elif source.startswith('/*', cursor):
            depth, walk = 1, cursor + 2
            while walk < length and depth:
                if source.startswith('/*', walk):
                    depth += 1
                    walk += 2
                elif source.startswith('*/', walk):
                    depth -= 1
                    walk += 2
                else:
                    walk += 1
            end = walk
        else:
            raw = RAW.match(source, cursor) if cursor == 0 or not (source[cursor-1].isalnum() or source[cursor-1] == '_') else None
            if raw:
                closing = '"' + raw.group(1)
                found = source.find(closing, raw.end())
                end = length if found < 0 else found + len(closing)
            elif source[cursor] == '"':
                walk = cursor + 1
                while walk < length:
                    if source[walk] == '\\':
                        walk += 2
                    elif source[walk] == '"':
                        walk += 1
                        break
                    else:
                        walk += 1
                end = min(walk, length)
            elif source[cursor] == "'":
                char = CHAR.match(source, cursor)
                if char:
                    end = char.end()
        if end is None:
            cursor += 1
            continue
        for position in range(start, end):
            if result[position] not in '\r\n':
                result[position] = ' '
        cursor = end
    return ''.join(result)


def attribute_end(masked: str, opening: int) -> int:
    depth = 1
    cursor = opening + 1
    while cursor < len(masked):
        if masked[cursor] == '[':
            depth += 1
        elif masked[cursor] == ']':
            depth -= 1
            if depth == 0:
                return cursor + 1
        cursor += 1
    raise ValueError('unterminated Rust attribute')


def contains_ignore(body: str) -> bool:
    body = body.strip()
    if re.match(r'(?:r#)?ignore(?:\s*=|\s*$)', body):
        return True
    wrapped = re.fullmatch(r'(?:r#)?cfg_attr\s*\((.*)\)', body, re.DOTALL)
    if not wrapped:
        return False
    arguments, start, depth = [], 0, 0
    inner = wrapped.group(1)
    for index, char in enumerate(inner):
        if char in '([{':
            depth += 1
        elif char in ')]}':
            depth -= 1
        elif char == ',' and depth == 0:
            arguments.append(inner[start:index])
            start = index + 1
    arguments.append(inner[start:])
    return any(contains_ignore(argument) for argument in arguments[1:])


def source_entries(path: str, source: str) -> list[dict]:
    masked = mask_literals(source)
    modules = list(MODULE.finditer(masked))
    module_cursor, depth, stack = 0, 0, []
    entries = []
    occurrences = Counter()
    cursor = 0
    while True:
        attribute = re.search(r'#!?\s*\[', masked[cursor:])
        if attribute is None:
            break
        start = cursor + attribute.start()
        opening = cursor + attribute.end() - 1
        end = attribute_end(masked, opening)
        cursor = end
        if not contains_ignore(masked[opening+1:end-1]):
            continue
        while module_cursor < len(modules) and modules[module_cursor].start() < start:
            token = modules[module_cursor]
            module_cursor += 1
            if token.group(1) is not None:
                if token.group(2) == '{':
                    depth += 1
                    stack.append((depth, token.group(1)))
            elif token.group() == '{':
                depth += 1
            else:
                while stack and stack[-1][0] == depth:
                    stack.pop()
                depth -= 1
        following = end
        while True:
            next_attribute = re.match(r'\s*#!?\s*\[', masked[following:])
            if next_attribute is None:
                break
            following = attribute_end(masked, following + next_attribute.end() - 1)
        declaration = re.match(r'\s*(?:(?:pub(?:\s*\([^)]*\))?|async|unsafe|const|extern|default)\s+)*fn\s+((?:r#)?[A-Za-z_]\w*)', masked[following:])
        # Retain unsupported/generated attribute sites too. This is an ownership
        # inventory, not an assertion that every site runs as a nextest test.
        name = declaration.group(1) if declaration else '<attribute>'
        scope = '::'.join([item[1] for item in stack] + [name])
        key = f'source:{path}::{scope}'
        occurrences[key] += 1
        if occurrences[key] > 1:
            key += f'#{occurrences[key]}'
        entries.append({'filter': key, 'attribute': source[start:end],
                        'line': source.count('\n', 0, start) + 1})
    return entries


def discover(root: Path) -> list[dict]:
    files = subprocess.check_output(['git', '-C', str(root), 'ls-files', '-z', '--', '*.rs'], timeout=30)
    entries = []
    for raw in files.split(b'\0'):
        if not raw:
            continue
        name = raw.decode('utf-8')
        # Documentation Rust samples and the retired workspace prototype do
        # not belong to the current product/test source tree.
        if name.startswith(('docs/', 'workspace/')):
            continue
        path = root / name
        if path.is_symlink():
            raise ValueError(f'refusing to follow tracked Rust symlink: {name}')
        data = path.read_bytes()
        if b'ignore' not in data or not IGNORE_PREFIX.search(data):
            continue
        entries.extend(source_entries(name, data.decode('utf-8')))
    return sorted(entries, key=lambda entry: entry['filter'])


def validate_inventory(root: Path, document: dict) -> list[str]:
    if document.get('schema') != 1 or isinstance(document.get('schema'), bool):
        return ['source ignore registry requires schema 1']
    actual = {entry['filter']: entry['attribute'] for entry in discover(root)}
    registered = {entry['filter']: entry for entry in document['exclude']}
    errors = [f'unowned Rust ignore: {key}' for key in actual.keys() - registered.keys()]
    errors += [f'stale Rust ignore registry entry: {key}' for key in registered.keys() - actual.keys()]
    errors += [f'Rust ignore attribute changed; review {key}' for key in actual.keys() & registered.keys()
               if actual[key] != registered[key].get('attribute')]
    return sorted(errors)


if __name__ == '__main__':
    print(json.dumps(discover(ROOT), indent=2, ensure_ascii=False))
