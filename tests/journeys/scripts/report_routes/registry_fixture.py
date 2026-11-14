"""A hermetic loopback package registry for the public-route probes.

It answers the native npm and PyPI metadata protocols the owner speaks when it
is started with `--registry-discovery-source` and `--registry-source`, for a
small fixed set of packages. It binds 127.0.0.1 only and never fetches
anything: no byte of it comes from a real registry. Every request path is
logged so a probe can prove what reached the network.
"""
import http.server, json, threading

FASTAPI_VERSIONS = ["0.160.0", "0.161.0", "0.162.0"]
VITE_VERSIONS = ["5.4.0", "6.0.0"]


def pypi_json(name, version):
    return {
        "info": {
            "name": name, "version": version, "summary": f"{name} fixture",
            "description": f"README witness for {name}", "requires_python": ">=3.9",
            "requires_dist": ["starlette>=0.40", "pydantic>=2"],
        },
        "releases": {v: [{"yanked": False}] for v in FASTAPI_VERSIONS},
    }


def npm_packument(name, versions):
    return {
        "name": name, "_rev": "1-a", "description": f"{name} fixture", "readme": f"README witness for {name}",
        "versions": {v: {"license": "MIT", "engines": {"node": ">=18"}, "dependencies": {"dep": "^1"}}
                     for v in versions},
    }


class Registry:
    def __init__(self):
        self.requests = []
        self.clients = []
        registry = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                registry.requests.append(self.path)
                registry.clients.append(self.client_address[0])
                body = registry.route(self.path)
                if body is None:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                data = json.dumps(body).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def route(self, path):
        empty_feed = {"schema": 1, "next": "00" * 32, "items": []}
        if path in ("/npm", "/npm/"):
            return {"update_seq": 1}
        if path.startswith("/npm/_changes?"):
            if "since=0" in path:
                docs = [{"seq": i + 1, "id": n, "changes": [{"rev": "1-a"}], "doc": npm_packument(n, v)}
                        for i, (n, v) in enumerate([("vite", VITE_VERSIONS)])]
                return {"results": docs, "last_seq": len(docs), "pending": 0}
            return {"results": [], "last_seq": 1, "pending": 0}
        if path == "/npm/vite":
            return npm_packument("vite", VITE_VERSIONS)
        if path == "/pypi/simple/":
            return {"meta": {}, "projects": [{"name": "fastapi"}]}
        if path.startswith("/pypi/pypi/fastapi/") and path.endswith("/json"):
            parts = path.split("/")
            version = parts[4] if len(parts) == 6 and parts[4] in FASTAPI_VERSIONS else FASTAPI_VERSIONS[-1]
            return pypi_json("fastapi", version)
        if path == "/pypi/pypi/fastapi/json":
            return pypi_json("fastapi", FASTAPI_VERSIONS[-1])
        if path.startswith("/npm/feed?") or path.startswith("/pypi/feed?"):
            return empty_feed
        return None

    def source(self, ecosystem):
        return f"{ecosystem}={self.base}/{ecosystem}"

    def stop(self):
        self.server.shutdown()
        self.server.server_close()
