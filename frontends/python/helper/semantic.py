#!/usr/bin/env python3
"""Hermetic CPython AST/symbol producer for the backend native wire."""

from __future__ import annotations

import ast
import json
import struct
import sys
from dataclasses import dataclass


MAX_INPUT = 512 * 1024


@dataclass(frozen=True)
class Request:
    language: str
    session: bytes
    manifest: bytes
    authority: bytes
    inputs: dict[str, bytes]


def take(data: bytes, at: int, size: int) -> tuple[bytes, int]:
    end = at + size
    if size < 0 or end > len(data):
        raise ValueError("truncated native request")
    return data[at:end], end


def read_request() -> Request:
    data = sys.stdin.buffer.read(MAX_INPUT + 1)
    if len(data) > MAX_INPUT:
        raise ValueError("native request exceeds helper bound")
    magic, at = take(data, 0, 4)
    version, language_size, count = struct.unpack_from(">HHH", data, at)
    at += 6
    if magic != b"BCQ\0" or version != 1:
        raise ValueError("invalid native request")
    session, at = take(data, at, 32)
    manifest, at = take(data, at, 32)
    authority, at = take(data, at, 32)
    language_bytes, at = take(data, at, language_size)
    inputs: dict[str, bytes] = {}
    for _ in range(count):
        name_size = struct.unpack_from(">H", data, at)[0]
        at += 2
        value_size = struct.unpack_from(">I", data, at)[0]
        at += 4
        name_bytes, at = take(data, at, name_size)
        value, at = take(data, at, value_size)
        name = name_bytes.decode("utf-8")
        if name in inputs:
            raise ValueError("duplicate native input")
        inputs[name] = value
    if at != len(data):
        raise ValueError("trailing native request bytes")
    return Request(language_bytes.decode("utf-8"), session, manifest, authority, inputs)


def compact(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


class Facts(ast.NodeVisitor):
    def __init__(self, source: str) -> None:
        self.source = source
        self.lines = [0]
        for line in source.splitlines(keepends=True):
            self.lines.append(self.lines[-1] + len(line.encode()))
        self.owner = ["module"]
        self.declared: dict[str, str] = {}
        self.imports: dict[str, str] = {}
        self.rows: list[tuple[int, str, bytes]] = []

    def span(self, node: ast.AST) -> dict[str, int]:
        start = self.lines[node.lineno - 1] + node.col_offset
        end = self.lines[node.end_lineno - 1] + node.end_col_offset
        return {"start": start, "end": max(start + 1, end)}

    def qualify(self, name: str) -> str:
        return ".".join((*self.owner, name))

    def annotation(self, node: ast.AST | None) -> str | None:
        return None if node is None else ast.unparse(node)

    def emit_declaration(self, node: ast.AST, name: str, kind: str, signature: str,
                         class_form: str | None = None, parameters: list[dict] | None = None,
                         annotations: list[str] | None = None) -> None:
        identity = self.qualify(name)
        self.declared[name] = identity
        payload = {
            "owner": ".".join(self.owner), "name": name, "kind": kind,
            "span": self.span(node), "signature": signature,
            "documentation": ast.get_docstring(node, clean=False)
            if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)) else None,
            "class_form": class_form, "parameters": parameters or [],
            "annotations": annotations or [],
        }
        self.rows.append((1, identity, compact(payload)))

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        bases = [ast.unparse(base) for base in node.bases]
        form = "plain"
        names = {name.rsplit(".", 1)[-1] for name in bases}
        decorators = {ast.unparse(item).rsplit(".", 1)[-1] for item in node.decorator_list}
        if "dataclass" in decorators: form = "dataclass"
        elif "Protocol" in names: form = "protocol"
        elif "TypedDict" in names: form = "typed_dict"
        elif "Enum" in names: form = "enum"
        self.emit_declaration(node, node.name, "class", f"class {node.name}({', '.join(bases)})", form)
        self.owner.append(node.name)
        self.generic_visit(node)
        self.owner.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        parameters = []
        positional_only = set(node.args.posonlyargs)
        keyword_only = set(node.args.kwonlyargs)
        for argument in (*node.args.posonlyargs, *node.args.args, *node.args.kwonlyargs):
            kind = "positional_only" if argument in positional_only else "keyword_only" if argument in keyword_only else "positional_or_keyword"
            parameters.append({"name": argument.arg, "kind": kind, "span": self.span(argument),
                               "annotation": self.annotation(argument.annotation), "has_default": False})
        if node.args.vararg:
            parameters.append({"name": node.args.vararg.arg, "kind": "var_args", "span": self.span(node.args.vararg),
                               "annotation": self.annotation(node.args.vararg.annotation), "has_default": False})
        if node.args.kwarg:
            parameters.append({"name": node.args.kwarg.arg, "kind": "kw_args", "span": self.span(node.args.kwarg),
                               "annotation": self.annotation(node.args.kwarg.annotation), "has_default": False})
        returns = self.annotation(node.returns)
        signature = f"def {node.name}({', '.join(item['name'] for item in parameters)})"
        if returns: signature += f" -> {returns}"
        self.emit_declaration(node, node.name, "function", signature, parameters=parameters,
                              annotations=[] if returns is None else [returns])
        self.owner.append(node.name)
        self.generic_visit(node)
        self.owner.pop()

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_AnnAssign(self, node: ast.AnnAssign) -> None:
        if isinstance(node.target, ast.Name):
            annotation = ast.unparse(node.annotation)
            identity = self.qualify(node.target.id)
            self.emit_declaration(node, node.target.id, "field", f"{node.target.id}: {annotation}", annotations=[annotation])
            self.rows.append((2, identity, compact({"owner": identity, "site": "field", "span": self.span(node), "inferred": annotation})))
        self.generic_visit(node)

    def visit_Import(self, node: ast.Import) -> None:
        for alias in node.names:
            binding = alias.asname or alias.name.split(".")[0]
            self.imports[binding] = alias.name
            self.rows.append((5, f"python/{alias.name}", compact({"binding": binding, "module": alias.name, "span": self.span(node)})))

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        module = "." * node.level + (node.module or "")
        for alias in node.names:
            binding = alias.asname or alias.name
            self.imports[binding] = module
            self.rows.append((5, f"python/{module}", compact({"binding": binding, "module": module, "span": self.span(node)})))

    def visit_Name(self, node: ast.Name) -> None:
        if not isinstance(node.ctx, ast.Load): return
        target = self.declared.get(node.id, node.id)
        module = self.imports.get(node.id)
        resolution = "local" if node.id in self.declared else "foreign" if module else "unresolved"
        owner = ".".join(self.owner)
        self.rows.append((3, f"{owner}->{target}@{self.span(node)['start']}", compact({
            "owner": owner, "target": target, "span": self.span(node),
            "resolution": resolution, "module": module,
        })))


def analyze(source: bytes) -> list[tuple[int, str, bytes]]:
    text = source.decode("utf-8")
    try:
        tree = ast.parse(text, filename="module.py", type_comments=True)
    except SyntaxError as error:
        start = max(0, (error.offset or 1) - 1)
        return [(4, f"syntax@{start}", compact({"severity": "error", "code": "SyntaxError",
                "span": {"start": start, "end": start + 1}, "message": error.msg}))]
    facts = Facts(text)
    facts.visit(tree)
    facts.rows.append((6, "python/imports/optional", compact({"binding": "optional", "module": "imports/optional", "span": {"start": 0, "end": 1}})))
    rows = sorted(set(facts.rows), key=lambda row: (row[0], row[1], row[2]))
    return [row for index, row in enumerate(rows)
            if index == 0 or row[:2] != rows[index - 1][:2]]


def write_envelope(request: Request, rows: list[tuple[int, str, bytes]]) -> None:
    output = sys.stdout.buffer
    output.write(b"BCN\0" + struct.pack(">HBBI", 1, 0, 0, len(rows)))
    output.write(request.session + request.manifest + request.authority + struct.pack(">Q", 0))
    language = request.language.encode()
    output.write(struct.pack(">H", len(language)) + language)
    for kind, key_text, value in rows:
        key = key_text.encode()
        output.write(struct.pack(">BBII", kind, 0, len(key), len(value)) + key + value)


if __name__ == "__main__":
    native_request = read_request()
    if native_request.language != "python":
        raise ValueError("request language is not python")
    write_envelope(native_request, analyze(native_request.inputs["module.py"]))
