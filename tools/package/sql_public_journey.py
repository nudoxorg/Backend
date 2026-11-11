#!/usr/bin/env python3
"""SQL journey through the public binaries: owner, CLI and MCP, cold restart.

Drives real `backend-locald`, `backend-cli` and `backend-mcp` processes from a
bundle directory that also holds the SQL worker as `backend-sqlite-worker`
beside the owner. The owner binary must have been built with
`NUDOX_BUILD_SQLITE_WORKER_ID` set to that worker's executable identity (see
`crates/compile/examples/sql_worker_identity.rs`); without it the owner admits
no worker and every SQL host is a recorded gap, which this script then fails on.

Development mode: debug builds skip the account gate. A gate-on run would stop
at the typed account refusal and is not a pass.

No real accounts, no network, a private temporary state and project directory.
"""
import argparse, atexit, json, os, pathlib, re, select, shutil, signal, subprocess, sys, tempfile, time

p = argparse.ArgumentParser()
p.add_argument("--bundle", required=True)
p.add_argument("--output", required=True)
p.add_argument("--sql-only", action="store_true", help="leave out the TypeScript files")
p.add_argument("--python", action="store_true", help="index Python beside SQL instead of TypeScript")
p.add_argument("--flat", action="store_true", help="place migrations and queries in one directory")
p.add_argument("--expect-no-worker", action="store_true", help="require named unavailable-host gaps and successful cold publication")
a = p.parse_args()
bundle = pathlib.Path(a.bundle).resolve(strict=True)
out = pathlib.Path(a.output).resolve()
out.mkdir(parents=True, exist_ok=False)

FILES = {
    "db/migrations/0001_init.sql": (
        "CREATE TABLE accounts (\n  id INTEGER PRIMARY KEY,\n  email TEXT NOT NULL UNIQUE,\n"
        "  balance INTEGER NOT NULL DEFAULT 0\n);\n"
        "CREATE TABLE orders (\n  id INTEGER PRIMARY KEY,\n"
        "  account_id INTEGER NOT NULL REFERENCES accounts(id),\n  total INTEGER NOT NULL\n);\n"
        "CREATE INDEX orders_by_account ON orders(account_id);\n"
    ),
    "db/migrations/0002_tier.sql": "ALTER TABLE accounts ADD COLUMN tier TEXT NOT NULL DEFAULT 'basic';\n",
    "db/migrations/0003_views.sql": (
        "CREATE VIEW account_spend AS\n  SELECT a.id AS account_id, a.email, SUM(o.total) AS spent\n"
        "  FROM accounts a JOIN orders o ON o.account_id = a.id\n  GROUP BY a.id, a.email;\n"
    ),
    "db/queries/accounts.sql": (
        "SELECT email, balance FROM accounts WHERE id = ?1;\n"
        "UPDATE accounts SET balance = balance - ?2 WHERE id = ?1;\n"
        "SELECT s.email, s.spent FROM account_spend s WHERE s.spent > 100;\n"
    ),
    # Application code beside the SQL: indexing must not trip over it.
    "app/server.ts": 'export const query = "SELECT email FROM accounts";\n',
    "package.json": '{"name":"sql-public-fixture","version":"1.0.0","private":true}\n',
    "tsconfig.json": '{"compilerOptions":{"target":"ES2020","module":"ESNext","strict":true,"noEmit":true},"include":["app"]}\n',
}

if a.python:
    FILES = {path: text for path, text in FILES.items() if path.startswith("db/")}
    FILES["app/repository.py"] = "def load_query():\n    return 1\n"
# This valid SQLite construct previously lost an entire file in a source build.
FILES["db/migrations/0001_init.sql"] += "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT, country TEXT);\n"
FILES["db/queries/upsert.sql"] = (
    "INSERT INTO authors (id, name, country) VALUES (?1, ?2, ?3)\n"
    "ON CONFLICT (id) DO UPDATE SET name = excluded.name, country = excluded.country;\n"
)

# Report-specific migration history and positive/negative impact controls.
FILES.update({
    "db/migrations/0010_bookshop.sql": (
        "CREATE TABLE books (id INTEGER PRIMARY KEY, price_cents INTEGER, title TEXT);\n"
        "CREATE TABLE order_items (book_id INTEGER REFERENCES books(id));\n"
        "CREATE TABLE price_audit (book_id INTEGER REFERENCES books(id));\n"
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, legacy_code TEXT);\n"
        "CREATE INDEX books_by_title ON books(title);\n"
        "CREATE VIEW book_sales AS SELECT title FROM books;\n"
    ),
    "db/migrations/0011_bookshop_rename.sql": (
        "ALTER TABLE books RENAME COLUMN price_cents TO list_price_cents;\n"
        "ALTER TABLE order_items RENAME TO order_lines;\n"
        "ALTER TABLE customers DROP COLUMN legacy_code;\n"
        "CREATE TRIGGER reprice_book AFTER UPDATE OF list_price_cents ON books "
        "BEGIN SELECT old.list_price_cents, new.list_price_cents; END;\n"
    ),
    "db/queries/bookshop.sql": (
        "SELECT price_cents FROM books;\n"
        "SELECT * FROM order_items;\n"
        "SELECT legacy_code FROM customers;\n"
    ),
    "db/queries/book_by_isbn.sql": "SELECT list_price_cents FROM books WHERE id = ?1;\n",
    "db/queries/reprice_book.sql": "UPDATE books SET list_price_cents = ?1 WHERE id = ?2;\n",
    "db/queries/place_order_line.sql": "SELECT list_price_cents FROM books WHERE id = ?1;\n",
    "db/queries/unrelated_title.sql": "SELECT title FROM books;\n",
})

def sql_path(path):
    return "db/" + pathlib.PurePosixPath(path).name if a.flat else path

if a.flat:
    FILES = {sql_path(path) if path.startswith("db/") else path: text for path, text in FILES.items()}

records = []
owner = None
mcp = None


def fail(message):
    records.append({"failure": message})
    (out / "journey.json").write_text(json.dumps(records, indent=1))
    print("FAIL:", message)
    cleanup()
    sys.exit(1)


def cleanup():
    global owner
    for process in (owner, mcp):
        if process and process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=10)
            except Exception:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=10)
                except Exception:
                    pass


# Also retire children if a malformed response raises outside fail().
atexit.register(cleanup)

root = pathlib.Path(tempfile.mkdtemp(prefix="ndx-sql-public-"))  # under TMPDIR
os.chmod(root, 0o700)
env = {k: os.environ[k] for k in ("PATH", "LANG", "LC_ALL", "TZ", "NUDOX_ACCOUNT_GATE", "NUDOX_TRACE") if k in os.environ}
for k in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR", "CARGO_HOME", "TMPDIR"):
    env[k] = str(root / k)
    pathlib.Path(env[k]).mkdir(mode=0o700)
project = root / "project"
for rel, text in FILES.items():
    if a.sql_only and not rel.startswith("db/"):
        continue
    path = project / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
project = project.resolve()
workspace = root / "state"
workspace.mkdir(mode=0o700)
secret = workspace / "authority.secret"
secret.write_bytes(os.urandom(32))
os.chmod(secret, 0o600)
endpoint = root / "owner.sock"
env["BACKEND_LOCALD_AUTHORITY_SECRET_FILE"] = str(secret)


def start_owner():
    global owner
    if endpoint.exists():
        endpoint.unlink()
    err = (out / f"owner-{len(records)}.stderr").open("wb")
    owner = subprocess.Popen(
        [str(bundle / "backend-locald"), "--endpoint", str(endpoint), "--workspace", str(workspace),
         "--authority-secret-file", str(secret), "--registry-offline", "--registry-discovery-offline",
         "--advisory-offline", "--forge-offline"],
        cwd=project, env=env, stdout=subprocess.DEVNULL, stderr=err, start_new_session=True)
    deadline = time.monotonic() + 600
    while not endpoint.exists():
        if owner.poll() is not None:
            fail(f"owner exited {owner.returncode} before its endpoint existed")
        if time.monotonic() > deadline:
            fail("owner startup timed out")
        time.sleep(0.05)


def cli(label, args, timeout=900):
    cmd = [str(bundle / "backend-cli"), "--format", "json", "--detail", "full", "--project", str(project), "--workspace", str(workspace),
           "--endpoint", str(endpoint), *args]
    try:
        done = subprocess.run(cmd, cwd=project, env=env, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        tail = ""
        for f in sorted(out.glob("owner-*.stderr")):
            tail += f.read_text(errors="replace")[-1500:]
        fail(f"{label}: CLI did not finish in {timeout}s; owner stderr tail:\n{tail}")
    raw = done.stdout.decode("utf-8", "replace")
    records.append({"label": label, "argv": cmd[1:], "exit": done.returncode, "stdout": raw, "stderr": done.stderr.decode("utf-8", "replace")})
    return done.returncode, raw


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for item in value.values():
            yield from strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)


def query(label, *args):
    code, raw = cli(label, ["sql-query", str(project), *args])
    if code != 0:
        fail(f"{label}: sql-query exited {code}: {raw[:400]}")
    try:
        body = json.loads(raw)
    except ValueError:
        fail(f"{label}: sql-query output is not JSON")
    return body, "\n".join(strings(body))


def expect(text, needles, label):
    for needle in needles:
        if needle not in text:
            fail(f"{label}: missing {needle!r} in:\n{text[:1500]}")


start_owner()
# One accepted operation. A caller wait expiry is a failure of this oracle;
# retain its operation key and state rather than submitting another add.
code, raw = cli("index", ["add", str(project), "--wait", "--wait-timeout", "900"], timeout=960)
if code != 0:
    fail(f"index exited {code}: {raw[:600]} (a typed account refusal means this is a gate-on build)")

if a.expect_no_worker:
    def unavailable_gaps(label):
        body, _ = query(label, "--role", "gap", "--limit", "128")
        data = body.get("semantic_data", {})
        if data.get("kind") != "sql-query":
            fail(f"{label}: missing full typed SQL page")
        page = data["value"]
        if page.get("terminal", {}).get("state") != "complete":
            fail(f"{label}: incomplete gap page")
        expected = {path for path in FILES if path.endswith(".sql")}
        gaps = page.get("host_gaps", [])
        if {gap.get("path") for gap in gaps} != expected or len(gaps) != len(expected):
            fail(f"{label}: selected SQL file gaps differ: {gaps}")
        if any(gap.get("reason") != {"state": "unavailable", "reason": "engine_not_admitted"} for gap in gaps):
            fail(f"{label}: worker terminal reasons differ: {gaps}")
        return gaps
    def mcp_unavailable_gaps(expected, label):
        global mcp
        mcp = subprocess.Popen([str(bundle / "backend-mcp"), "--project", str(project), "--workspace", str(workspace), "--endpoint", str(endpoint)], cwd=project, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=(out / f"{label}.stderr").open("wb"), start_new_session=True)
        def exchange(seq, method, params):
            mcp.stdin.write((json.dumps({"jsonrpc": "2.0", "id": seq, "method": method, "params": params}) + "\n").encode())
            mcp.stdin.flush()
            if not select.select([mcp.stdout], [], [], 120)[0]:
                fail(f"{label}: MCP {method} response timed out")
            reply = json.loads(mcp.stdout.readline())
            records.append({"label": label + ":" + method, "reply": reply})
            return reply
        exchange(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "sql-public-journey", "version": "1"}})
        mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        mcp.stdin.flush()
        reply = exchange(2, "tools/call", {"name": "backend.sql_query", "arguments": {"package": str(project), "role": "gap", "limit": 128, "detail": "full"}})
        result = reply.get("result", {})
        data = result.get("structuredContent", {}).get("semantic_data", {})
        if result.get("isError") or data.get("kind") != "sql-query" or data.get("value", {}).get("host_gaps") != expected:
            fail(f"{label}: MCP gap facts differ from CLI: {reply}")
        mcp.stdin.close()
        mcp.wait(timeout=10)

    before = unavailable_gaps("no-worker-gaps")
    mcp_unavailable_gaps(before, "no-worker-mcp-gaps")
    os.killpg(owner.pid, signal.SIGKILL)
    owner.wait(timeout=10)
    start_owner()
    after = unavailable_gaps("no-worker-cold-gaps")
    if before != after:
        fail("unavailable SQL host gaps changed after cold reopen")
    mcp_unavailable_gaps(after, "no-worker-cold-mcp-gaps")
    cleanup()
    (out / "journey.json").write_text(json.dumps(records, indent=1))
    print("PASS: SQL without worker publishes with named gaps and survives cold restart")
    sys.exit(0)

# 1. Definitions across files, through the CLI.
_, text = query("tables", "--role", "definition", "--kind", "table")
expect(text, ["table accounts", "table orders"], "tables")
_, text = query("columns", "--role", "definition", "--kind", "column", "--parent", "accounts")
expect(text, ["column accounts.balance", "column accounts.tier", "column accounts.email"], "columns")
_, text = query("views", "--role", "definition", "--kind", "view")
expect(text, ["view account_spend"], "views")
# Ordinary discovery must expose native SQL declarations.
code, raw = cli("search-sql", ["search", "account_spend"])
if code != 0 or "account_spend" not in raw or "SQL view" not in raw:
    fail("normal search did not discover the SQL view: " + raw[:1200])
code, raw = cli("search-sql-column", ["search", "balance"])
if code != 0 or "SQL column accounts.balance" not in raw:
    fail("normal search did not discover the SQL column by its leaf name: " + raw[:1200])
code, raw = cli("outline-sql", ["outline", str(project)])
if code != 0 or "account_spend" not in raw:
    fail("normal outline did not discover the SQL view: " + raw[:1200])
# 2. Honest coverage: nothing partial, files all selected.
_, text = query("gaps", "--role", "gap")
expect(text, ["no known gaps in the SQL files"], "gaps")
# The worker and owner both accepted the excluded upsert and bound its reads.
_, text = query("upsert", "--role", "reference", "--uses", "authors.name")
expect(text, [sql_path("db/queries/upsert.sql"), "excluded.name"], "upsert")
# 3. Who uses accounts.balance: three mentions in another file, bound exactly.
body, text = query("uses-balance", "--role", "reference", "--kind", "column", "--uses", "accounts.balance")
expect(text, [sql_path("db/queries/accounts.sql")], "uses")
mentions = re.findall(r"Column (?:\w+\.)?balance in", text)
if len(mentions) != 3:
    fail("expected exactly three mentions of accounts.balance:\n" + text[:1500])
reference = None
for s in strings(body):
    if s.startswith("read: sql-read-source "):
        reference = s.split()[-1]
if not reference:
    fail("no source action on the first hit")
code, raw = cli("read-source", ["sql-read-source", reference])
if code != 0 or '"text":"balance"' not in raw.replace(" ", "") and "balance" not in raw:
    fail("sql-read-source did not return the exact written name: " + raw[:400])
def typed(body, kind, label):
    semantic = body.get("semantic_data")
    if not isinstance(semantic, dict) or semantic.get("kind") != kind or not isinstance(semantic.get("value"), dict):
        fail(f"{label}: expected full typed {kind} payload: {body}")
    return semantic["value"]


def sql_page(label, *args):
    body, _ = query(label, *args, "--limit", "128")
    page = typed(body, "sql-query", label)
    if page.get("terminal", {}).get("state") != "complete":
        fail(f"{label}: incomplete typed SQL page: {page.get('terminal')}")
    return page


def impact_page(label, option, name):
    code, raw = cli(label, ["impact", str(project), option, name, "--limit", "200"])
    if code != 0:
        fail(f"{label}: impact failed: {raw[:1200]}")
    page = typed(json.loads(raw), "impact", label)
    if page.get("seed_state", {}).get("state") != "resolved" or page.get("terminal", {}).get("state") != "complete":
        fail(f"{label}: unresolved or incomplete impact: {page}")
    return page


def bookshop_claims(label):
    definitions = sql_page(label + "-definitions", "--role", "definition", "--kind", "table")
    names = [h["fact"] for h in definitions["hits"] if h["fact"].get("name") in ("order_items", "order_lines")]
    if len(names) < 2 or any(n.get("head_name") != "order_lines" or n.get("entity") is None for n in names):
        fail(f"{label}: missing renamed table claims: {names}")
    if any(n["entity"] != names[0]["entity"] for n in names):
        fail(f"{label}: rename split the exact table entity: {names}")
    columns = sql_page(label + "-price-definitions", "--role", "definition", "--kind", "column", "--parent", "books")
    price_defs = [h["fact"] for h in columns["hits"] if h["fact"].get("head_name") == "list_price_cents"]
    if not price_defs or price_defs[0].get("entity") is None or any(d.get("entity") != price_defs[0]["entity"] for d in price_defs):
        fail(f"{label}: renamed price column has no single entity: {price_defs}")
    price_refs = sql_page(label + "-price-references", "--role", "reference", "--kind", "column", "--uses", "books.list_price_cents")
    for path in ("db/queries/book_by_isbn.sql", "db/queries/reprice_book.sql", "db/queries/place_order_line.sql"):
        if not any(h["path"] == sql_path(path) and h["fact"].get("resolution") == {"Resolved": {"target": price_defs[0]["entity"], "live": True}} for h in price_refs["hits"]):
            fail(f"{label}: {path} does not bind the exact price entity: {price_refs['hits']}")
    trigger_refs = [h["fact"] for h in price_refs["hits"] if h["path"] == sql_path("db/migrations/0011_bookshop_rename.sql") and (h["fact"].get("in_object") or {}).get("name") == "reprice_book"]
    if not {"old", "new"}.issubset({r.get("qualifier") for r in trigger_refs}) or len(trigger_refs) < 3:
        fail(f"{label}: trigger UPDATE OF / old / new price references missing: {trigger_refs}")
    diagnostics = sql_page(label + "-diagnostics", "--role", "diagnostic")
    refs = [h for h in diagnostics["hits"] if h["path"] == sql_path("db/queries/bookshop.sql") and h["fact"].get("fact") == "Reference"]
    expected = {"price_cents": "NoSuchColumn", "order_items": "NoSuchObject"}
    for name, reason in expected.items():
        if not any(h["fact"].get("name") == name and h["fact"].get("resolution") == {"Unresolved": {"reason": reason}} for h in refs):
            fail(f"{label}: missing typed unbound {name}/{reason}: {refs}")
    dropped = [h["fact"] for h in refs if h["fact"].get("name") == "legacy_code"]
    if not any(isinstance(f.get("resolution"), dict) and f["resolution"].get("Resolved", {}).get("live") is False for f in dropped):
        fail(f"{label}: dropped legacy_code was not reported with a dead entity: {dropped}")
    column = impact_page(label + "-column-impact", "--sql-column", "books.list_price_cents")
    nodes = [r["node"] for r in column["rows"]]
    for path in ("db/queries/book_by_isbn.sql", "db/queries/reprice_book.sql", "db/queries/place_order_line.sql"):
        if not any(r["node"].get("kind") == "query" and r["node"].get("path") == sql_path(path) and r.get("strength") == "exact" for r in column["rows"]):
            fail(f"{label}: {path} missing from price impact: {nodes}")
    if not any(n.get("kind") == "trigger" and n.get("name") == "reprice_book" for n in nodes):
        fail(f"{label}: price trigger missing from impact: {nodes}")
    if any(n.get("path") in (sql_path("db/queries/unrelated_title.sql"), sql_path("db/queries/bookshop.sql")) or n.get("kind") in ("table", "index") or n.get("name") in ("book_sales", "books_by_title", "order_items", "order_lines", "price_audit") for n in nodes):
        fail(f"{label}: column impact spread to unrelated objects: {nodes}")
    table = impact_page(label + "-table-impact", "--sql-table", "order_lines")
    if len(table["seeds"]) != 1 or table["seeds"][0].get("name") != "order_lines":
        fail(f"{label}: renamed table did not resolve to exactly one canonical seed: {table['seeds']}")
    if any(n.get("name") == "order_items" for n in table["seeds"] + [r["node"] for r in table["rows"]]):
        fail(f"{label}: old table name retained as a separate impact node: {table}")
    return definitions, diagnostics, column, table


bookshop_before = bookshop_claims("bookshop")
def bookshop_search(label, name, signature, path):
    code, raw = cli(label, ["search", name])
    if code != 0:
        fail(f"{label}: search failed: {raw[:800]}")
    body = json.loads(raw)
    rows = [r for r in body.get("records", []) if r.get("signature") == signature]
    if len(rows) != 1 or rows[0].get("identity", {}).get("path") != sql_path(path):
        fail(f"{label}: expected one SQL declaration with its selected path: {rows}")
    return rows[0]


def bookshop_discovery(label):
    return [
        bookshop_search(label + "-price", "list_price_cents", "SQL column books.list_price_cents", "db/migrations/0010_bookshop.sql"),
        bookshop_search(label + "-view", "book_sales", "SQL view book_sales", "db/migrations/0010_bookshop.sql"),
        bookshop_search(label + "-table", "order_lines", "SQL table order_lines", "db/migrations/0010_bookshop.sql"),
    ]


bookshop_discovery_before = bookshop_discovery("bookshop-search")

# 4. The same through MCP over stdio.
mcp = subprocess.Popen([str(bundle / "backend-mcp"), "--project", str(project), "--workspace", str(workspace),
                        "--endpoint", str(endpoint)], cwd=project, env=env, stdin=subprocess.PIPE,
                       stdout=subprocess.PIPE, stderr=(out / "mcp.stderr").open("wb"), start_new_session=True)


def rpc(seq, method, params):
    mcp.stdin.write((json.dumps({"jsonrpc": "2.0", "id": seq, "method": method, "params": params}) + "\n").encode())
    mcp.stdin.flush()
    if not select.select([mcp.stdout], [], [], 120)[0]:
        fail(f"MCP {method} response exceeded the harness deadline")
    line = mcp.stdout.readline()
    reply = json.loads(line)
    records.append({"label": "mcp:" + method, "reply": reply})
    return reply


rpc(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "sql-public-journey", "version": "1"}})
mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
mcp.stdin.flush()
reply = rpc(2, "tools/call", {"name": "backend.sql_query", "arguments": {
    "package": str(project), "role": "definition", "kind": "column", "parent": "accounts"}})
mtext = "\n".join(strings(reply))
if reply.get("result", {}).get("isError") or "column accounts.balance" not in mtext:
    fail("MCP sql_query did not return the column definitions: " + mtext[:600])
# Full typed MCP responses must carry the same facts and impact nodes as CLI.
def mcp_typed(seq, tool, arguments, kind):
    reply = rpc(seq, "tools/call", {"name": tool, "arguments": {"package": str(project), "detail": "full", **arguments}})
    result = reply.get("result", {})
    if result.get("isError") or not isinstance(result.get("structuredContent"), dict):
        fail(f"{tool}: missing typed MCP response: {reply}")
    return typed(result["structuredContent"], kind, tool)

def mcp_search_matches(seq, name, expected):
    reply = rpc(seq, "tools/call", {"name": "backend.search", "arguments": {"query": name, "detail": "full"}})
    result = reply.get("result", {})
    records = result.get("structuredContent", {}).get("records", [])
    if result.get("isError") or not any(r.get("signature") == expected.get("signature") and r.get("identity") == expected.get("identity") for r in records):
        fail(f"MCP search {name} differs from CLI: {reply}")

for seq, name, expected in zip((11, 12, 13), ("list_price_cents", "book_sales", "order_lines"), bookshop_discovery_before):
    mcp_search_matches(seq, name, expected)
mcp_defs = mcp_typed(3, "backend.sql_query", {"role": "definition", "kind": "table", "limit": 128}, "sql-query")
mcp_diag = mcp_typed(4, "backend.sql_query", {"role": "diagnostic", "limit": 128}, "sql-query")
mcp_impact = mcp_typed(5, "backend.impact", {"sql_column": "books.list_price_cents", "limit": 200}, "impact")
mcp_table = mcp_typed(9, "backend.impact", {"sql_table": "order_lines", "limit": 200}, "impact")
for cli_page, mcp_page in ((bookshop_before[0], mcp_defs), (bookshop_before[1], mcp_diag)):
    if cli_page["hits"] != mcp_page["hits"] or cli_page["terminal"] != mcp_page["terminal"]:
        fail("CLI and MCP typed SQL facts differ")
if [(r["node"], r["strength"], r["relation"]) for r in bookshop_before[2]["rows"]] != [(r["node"], r["strength"], r["relation"]) for r in mcp_impact["rows"]]:
    fail("CLI and MCP typed column impact differs")
if [(r["node"], r["strength"], r["relation"]) for r in bookshop_before[3]["rows"]] != [(r["node"], r["strength"], r["relation"]) for r in mcp_table["rows"]]:
    fail("CLI and MCP typed renamed table impact differs")
mcp.stdin.close()
mcp.wait(timeout=10)

# 5. Edit: drop the column in a new migration, reindex, the old queries point at a dropped column.
(project / sql_path("db/migrations/0004_drop_balance.sql")).write_text("ALTER TABLE accounts DROP COLUMN balance;\n")
code, raw = cli("reindex-1", ["add", str(project), "--wait", "--wait-timeout", "600"], timeout=720)
if code != 0:
    fail("reindex after edit failed: " + raw[:400])
_, text = query("columns-after", "--role", "definition", "--kind", "column", "--parent", "accounts")
expect(text, ["no longer exists at the end of the migrations"], "columns-after")
_, text = query("uses-after", "--role", "reference", "--kind", "column", "--uses", "accounts.balance")
expect(text, ["(DROPPED)"], "uses-after")
code, raw = cli("outline-after-drop", ["outline", str(project)])
if code != 0 or "SQL column accounts.balance" in raw:
    fail("ordinary outline retained the dropped SQL column: " + raw[:1200])
# 6. An unparseable file is a gap, never an empty answer.
(project / "db/broken.sql").write_text("SELECT FROM WHERE;\n")
code, raw = cli("reindex-2", ["add", str(project), "--wait", "--wait-timeout", "600"], timeout=720)
if code != 0:
    fail("reindex with a broken file failed: " + raw[:400])
_, text = query("gap-after", "--role", "gap")
expect(text, ["rejected by the parser", "db/broken.sql"], "gap-after")
_, text = query("uses-nothing", "--role", "reference", "--kind", "column", "--uses", "accounts.tier")
expect(text, ["NOT proof of absence"], "uses-nothing")

# 7. Cold restart: kill the owner without ceremony and ask again.
bookshop_pre_cold = bookshop_claims("bookshop-pre-cold")
bookshop_discovery_pre_cold = bookshop_discovery("bookshop-search-pre-cold")
before = query("pre-kill", "--role", "definition", "--kind", "column", "--parent", "accounts")[1]
os.killpg(owner.pid, signal.SIGKILL)
owner.wait(timeout=10)
start_owner()
after = query("post-kill", "--role", "definition", "--kind", "column", "--parent", "accounts")[1]
if before != after:
    fail("definitions differ after a cold restart:\nBEFORE\n%s\nAFTER\n%s" % (before[:800], after[:800]))
# Compare the same selected generation across the cold restart; prior edits may
# legitimately change its fact identities or context.
bookshop_after = bookshop_claims("bookshop-cold")
bookshop_discovery_after = bookshop_discovery("bookshop-search-cold")
for before_row, after_row in zip(bookshop_discovery_pre_cold, bookshop_discovery_after):
    if before_row.get("identity") != after_row.get("identity"):
        fail("SQL discovery identity changed after cold reopen")
for before_page, after_page in zip(bookshop_pre_cold[:2], bookshop_after[:2]):
    before_hits = {tuple(h["id"]): h["fact"] for h in before_page["hits"] if "bookshop" in h["path"]}
    after_hits = {tuple(h["id"]): h["fact"] for h in after_page["hits"] if "bookshop" in h["path"]}
    if before_hits != after_hits:
        fail("Bookshop typed fact identities changed after cold reopen")
mcp = subprocess.Popen([str(bundle / "backend-mcp"), "--project", str(project), "--workspace", str(workspace),
                        "--endpoint", str(endpoint)], cwd=project, env=env, stdin=subprocess.PIPE,
                       stdout=subprocess.PIPE, stderr=(out / "mcp-cold.stderr").open("wb"), start_new_session=True)
rpc(6, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "sql-public-journey", "version": "1"}})
mcp.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
mcp.stdin.flush()
for seq, name, expected in zip((14, 15, 16), ("list_price_cents", "book_sales", "order_lines"), bookshop_discovery_after):
    mcp_search_matches(seq, name, expected)
cold_diag = mcp_typed(7, "backend.sql_query", {"role": "diagnostic", "limit": 128}, "sql-query")
cold_impact = mcp_typed(8, "backend.impact", {"sql_column": "books.list_price_cents", "limit": 200}, "impact")
cold_table = mcp_typed(10, "backend.impact", {"sql_table": "order_lines", "limit": 200}, "impact")
if cold_diag["hits"] != bookshop_after[1]["hits"]:
    fail("cold MCP typed diagnostics differ from CLI")
if [(r["node"], r["strength"], r["relation"]) for r in cold_impact["rows"]] != [(r["node"], r["strength"], r["relation"]) for r in bookshop_after[2]["rows"]]:
    fail("cold MCP typed column impact differs from CLI")
if [(r["node"], r["strength"], r["relation"]) for r in cold_table["rows"]] != [(r["node"], r["strength"], r["relation"]) for r in bookshop_after[3]["rows"]]:
    fail("cold MCP typed renamed table impact differs from CLI")
mcp.stdin.close()
mcp.wait(timeout=10)
code, raw = cli("outline-cold", ["outline", str(project)])
if code != 0 or "account_spend" not in raw or "SQL column accounts.balance" in raw:
    fail("cold ordinary outline lost SQL or retained a dropped column: " + raw[:1200])
cleanup()
(out / "journey.json").write_text(json.dumps(records, indent=1))
print("PASS: sql public journey (CLI + MCP + cold restart)")
