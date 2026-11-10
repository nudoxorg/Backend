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
import argparse, json, os, pathlib, re, shutil, signal, subprocess, sys, tempfile, time

p = argparse.ArgumentParser()
p.add_argument("--bundle", required=True)
p.add_argument("--output", required=True)
p.add_argument("--sql-only", action="store_true", help="leave out the TypeScript files")
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

records = []
owner = None


def fail(message):
    records.append({"failure": message})
    (out / "journey.json").write_text(json.dumps(records, indent=1))
    print("FAIL:", message)
    cleanup()
    sys.exit(1)


def cleanup():
    global owner
    if owner and owner.poll() is None:
        try:
            os.killpg(owner.pid, signal.SIGTERM)
            owner.wait(timeout=10)
        except Exception:
            try:
                os.killpg(owner.pid, signal.SIGKILL)
            except Exception:
                pass


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
    cmd = [str(bundle / "backend-cli"), "--format", "json", "--project", str(project), "--workspace", str(workspace),
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
# The CLI wait is capped at 900 s; exit 75 means the operation is still active, so ask again.
for attempt in range(8):
    code, raw = cli("index" if attempt == 0 else f"index-wait-{attempt}", ["add", str(project), "--wait", "--wait-timeout", "900"], timeout=960)
    if code != 75:
        break
if code != 0:
    fail(f"index exited {code}: {raw[:600]} (a typed account refusal means this is a gate-on build)")

# 1. Definitions across files, through the CLI.
_, text = query("tables", "--role", "definition", "--kind", "table")
expect(text, ["table accounts", "table orders"], "tables")
_, text = query("columns", "--role", "definition", "--kind", "column", "--parent", "accounts")
expect(text, ["column accounts.balance", "column accounts.tier", "column accounts.email"], "columns")
_, text = query("views", "--role", "definition", "--kind", "view")
expect(text, ["view account_spend"], "views")
# 2. Honest coverage: nothing partial, files all selected.
_, text = query("gaps", "--role", "gap")
expect(text, ["no known gaps in the SQL files"], "gaps")
# 3. Who uses accounts.balance: three mentions in another file, bound exactly.
body, text = query("uses-balance", "--role", "reference", "--kind", "column", "--uses", "accounts.balance")
expect(text, ["db/queries/accounts.sql"], "uses")
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
# 4. The same through MCP over stdio.
mcp = subprocess.Popen([str(bundle / "backend-mcp"), "--project", str(project), "--workspace", str(workspace),
                        "--endpoint", str(endpoint)], cwd=project, env=env, stdin=subprocess.PIPE,
                       stdout=subprocess.PIPE, stderr=(out / "mcp.stderr").open("wb"), start_new_session=True)


def rpc(seq, method, params):
    mcp.stdin.write((json.dumps({"jsonrpc": "2.0", "id": seq, "method": method, "params": params}) + "\n").encode())
    mcp.stdin.flush()
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
mcp.stdin.close()
mcp.wait(timeout=10)

# 5. Edit: drop the column in a new migration, reindex, the old queries point at a dropped column.
(project / "db/migrations/0004_drop_balance.sql").write_text("ALTER TABLE accounts DROP COLUMN balance;\n")
code, raw = cli("reindex-1", ["add", str(project), "--wait", "--wait-timeout", "600"], timeout=720)
if code != 0:
    fail("reindex after edit failed: " + raw[:400])
_, text = query("columns-after", "--role", "definition", "--kind", "column", "--parent", "accounts")
expect(text, ["no longer exists at the end of the migrations"], "columns-after")
_, text = query("uses-after", "--role", "reference", "--kind", "column", "--uses", "accounts.balance")
expect(text, ["(DROPPED)"], "uses-after")
# 6. An unparseable file is a gap, never an empty answer.
(project / "db/broken.sql").write_text("SELECT FROM WHERE;\n")
code, raw = cli("reindex-2", ["add", str(project), "--wait", "--wait-timeout", "600"], timeout=720)
if code != 0:
    fail("reindex with a broken file failed: " + raw[:400])
_, text = query("gap-after", "--role", "gap")
expect(text, ["rejected by the parser"], "gap-after")
_, text = query("uses-nothing", "--role", "reference", "--kind", "column", "--uses", "accounts.tier")
expect(text, ["NOT proof of absence"], "uses-nothing")

# 7. Cold restart: kill the owner without ceremony and ask again.
before = query("pre-kill", "--role", "definition", "--kind", "column", "--parent", "accounts")[1]
os.killpg(owner.pid, signal.SIGKILL)
owner.wait(timeout=10)
start_owner()
after = query("post-kill", "--role", "definition", "--kind", "column", "--parent", "accounts")[1]
if before != after:
    fail("definitions differ after a cold restart:\nBEFORE\n%s\nAFTER\n%s" % (before[:800], after[:800]))
cleanup()
(out / "journey.json").write_text(json.dumps(records, indent=1))
print("PASS: sql public journey (CLI + MCP + cold restart)")
