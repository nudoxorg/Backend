#!/usr/bin/env python3
"""A local stand-in for the OpenAI-compatible embeddings endpoint
`index::server::search::semantic::HttpEmbedder` (via the vendored `embedrs`
crate, see `embedrs-0.4.0/src/provider/openai.rs`) talks to.

Why this exists, and why it isn't just "point tests at ollama": the
compiled-in test embedding brand is `JinaCodeV2`
(`jinaai/jina-embeddings-v2-base-code`, see
`workspace/registry/vector/core/model.rs`) -- `server_common::TestModel`.
Its canonical weights are a 641 MB ONNX artifact pinned by sha256 and not
fetchable/servable in this environment. `workspace/registry/vector/core/model.rs`
also defines `NomicEmbedText` ("the local-dev-only Ollama brand"; same 768
dimensions) specifically for hosts that run Ollama instead of the ONNX
weights -- but `server_common::TestModel` is hardcoded to `JinaCodeV2`, so
`HttpEmbedder` always requests model `jinaai/jina-embeddings-v2-base-code`,
a name Ollama's registry does not have.

This server bridges that gap without touching product code (out of this
task's scope) or the compiled-in brand (a real design decision, not a
test-harness bug -- reported, not silently patched around): it speaks the
exact wire contract embedrs's OpenAI provider expects --

    POST {base}/embeddings
    body: {"model": str, "input": [str, ...], "encoding_format": str, "dimensions"?: int}
    -> 200 {"data": [{"embedding": [f32; N]}, ...], "model": str, "usage": {"total_tokens": int}}

-- and forwards each request to a REAL local Ollama instance
(`UPSTREAM_URL`, default `http://127.0.0.1:11434/v1/embeddings`) under a
model Ollama actually has pulled (`UPSTREAM_MODEL`, default
`nomic-embed-text`), rewriting only the `model` field. When an upstream is
reachable, every embedding returned is a genuine, semantically meaningful
vector from a real local model -- not synthetic. `local-backends.nu` never
starts or stops the upstream Ollama itself (it may be a long-lived host
service unrelated to this test run); it only checks whether one is already
reachable.

If no upstream is reachable, this falls back to a deterministic
SHA-256-derived pseudo-vector per input, clearly logged as a fallback. That
degrades any test asserting retrieval *quality* against real semantics, but
keeps plumbing-only assertions (a request completes; an empty corpus answers
without error; a package that was embedded is findable by its own literal
tag) meaningful.
"""

import hashlib
import http.server
import json
import os
import struct
import sys
import urllib.error
import urllib.request

DIMENSIONS = 768
UPSTREAM_URL = os.environ.get("UPSTREAM_URL", "http://127.0.0.1:11434/v1/embeddings")
UPSTREAM_MODEL = os.environ.get("UPSTREAM_MODEL", "nomic-embed-text")
UPSTREAM_TIMEOUT = float(os.environ.get("UPSTREAM_TIMEOUT", "10"))


def deterministic_vector(seed_text: str, dimensions: int = DIMENSIONS) -> list[float]:
    """A stable, unit-ish pseudo-vector derived from the input text's SHA-256.
    Deterministic so repeated runs against the same input are reproducible;
    not trained, not semantic. Fallback path only -- see module docstring.
    """
    digest = hashlib.sha256(seed_text.encode("utf-8")).digest()
    values = []
    counter = 0
    while len(values) < dimensions:
        block = hashlib.sha256(digest + counter.to_bytes(4, "little")).digest()
        for i in range(0, len(block) - 3, 4):
            if len(values) >= dimensions:
                break
            (u,) = struct.unpack_from("<I", block, i)
            values.append((u / 0xFFFFFFFF) * 2.0 - 1.0)
        counter += 1
    norm = sum(v * v for v in values) ** 0.5 or 1.0
    return [v / norm for v in values]


def try_upstream(model: str, inputs: list[str], dimensions) -> dict | None:
    body = {"model": UPSTREAM_MODEL, "input": inputs, "encoding_format": "float"}
    if dimensions:
        body["dimensions"] = dimensions
    req = urllib.request.Request(
        UPSTREAM_URL,
        data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=UPSTREAM_TIMEOUT) as resp:
            parsed = json.loads(resp.read())
    except (urllib.error.URLError, OSError, json.JSONDecodeError, TimeoutError) as error:
        sys.stderr.write(f"stub-embeddings: upstream unreachable ({error}); falling back to synthetic vectors\n")
        return None
    # Normalize both OpenAI-shape ({"data":[{"embedding":...}]}) and any
    # variant that nests differently -- Ollama's /v1/embeddings is OpenAI-shape.
    data = parsed.get("data")
    if not isinstance(data, list) or len(data) != len(inputs):
        sys.stderr.write("stub-embeddings: upstream response shape unexpected; falling back to synthetic vectors\n")
        return None
    return {
        "data": data,
        "model": model,
        "usage": parsed.get("usage", {"total_tokens": sum(max(1, len(t.split())) for t in inputs)}),
    }


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):  # noqa: A003 - stdlib override
        sys.stderr.write("stub-embeddings: " + (fmt % args) + "\n")

    def _json(self, status: int, payload: dict):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path in ("/", "/healthz", "/health"):
            self._json(200, {"status": "ok", "stub": True, "upstream": UPSTREAM_URL})
            return
        self._json(404, {"error": "not found"})

    def do_POST(self):
        if not self.path.endswith("/embeddings"):
            self._json(404, {"error": "not found"})
            return
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            request = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            self._json(400, {"error": "invalid json"})
            return
        model = request.get("model", "stub-model")
        inputs = request.get("input", [])
        if isinstance(inputs, str):
            inputs = [inputs]
        dimensions = request.get("dimensions")

        upstream_result = try_upstream(model, inputs, dimensions) if inputs else None
        if upstream_result is not None:
            self._json(200, upstream_result)
            return

        # Synthetic fallback -- see module docstring.
        want_dims = dimensions or DIMENSIONS
        data = [{"embedding": deterministic_vector(text, want_dims)} for text in inputs]
        total_tokens = sum(max(1, len(text.split())) for text in inputs)
        self._json(200, {"data": data, "model": model, "usage": {"total_tokens": total_tokens}})


def main():
    host = os.environ.get("STUB_EMBEDDINGS_HOST", "127.0.0.1")
    port = int(os.environ.get("STUB_EMBEDDINGS_PORT", "21434"))
    server = http.server.ThreadingHTTPServer((host, port), Handler)
    sys.stderr.write(f"stub-embeddings: listening on http://{host}:{port}, upstream={UPSTREAM_URL} model={UPSTREAM_MODEL}\n")
    server.serve_forever()


if __name__ == "__main__":
    main()
