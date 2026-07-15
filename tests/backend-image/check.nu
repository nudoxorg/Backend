# Full backend image pipeline check (Nushell).
#
# Env (wired by tests/backend-image/default.nix):
#   NUDOX_SERVER           — server derivation (bin/server)
#   NUDOX_BACKEND_IMAGE    — nix2container image JSON path
#   NUDOX_COMPILER_PACKAGE — packages.compiler-daemon store path
#   NUDOX_PROJECT_ROOT     — repo checkout for buck2 fallback (optional)
#   NUDOX_AXUM_VERSION / NUDOX_ZOD_VERSION / NUDOX_PIPELINE_DEADLINE_SECS
#   SSL_CERT_FILE          — for crates.io / npm
#
# Modules loaded via --include-path (tests/lib).

use stack.nu *
use http.nu *
use oci.nu *
use blobs.nu *
use compiler.nu *

# Tear down anything whose pid file / pgdata lives under $work.
def cleanup-work [work: string] {
  for name in [server.log compiler.log qdrant.log terminus.log] {
    let pf = $work | path join $"($name).pid"
    if ($pf | path exists) {
      let pid = (try { open --raw $pf | str trim | into int } catch { null })
      kill-pid $pid
    }
  }
  let pgdata = $work | path join "pgdata"
  if ($pgdata | path exists) {
    stop-postgres $pgdata
  }
  try { rm -rf $work } catch { }
}

def fail [work: string, msg: string] {
  try { dump-logs $work } catch { }
  cleanup-work $work
  print --stderr $msg
  exit 1
}

let server = $env.NUDOX_SERVER
let backend = $env.NUDOX_BACKEND_IMAGE
let compiler_pkg = ($env.NUDOX_COMPILER_PACKAGE? | default "")
let project_root = ($env.NUDOX_PROJECT_ROOT? | default "")
let axum_version = ($env.NUDOX_AXUM_VERSION? | default "0.7.9")
let zod_version = ($env.NUDOX_ZOD_VERSION? | default "3.24.2")
let deadline = ($env.NUDOX_PIPELINE_DEADLINE_SECS? | default "300" | into int)

# Outbound HTTPS for archive acquisition.
if ($env.SSL_CERT_FILE? | default "") != "" {
  $env.NIX_SSL_CERT_FILE = $env.SSL_CERT_FILE
}

# ── OCI config (always; no network) ─────────────────────────────────────────
assert-backend-image $backend

# ── Workdir + ports ─────────────────────────────────────────────────────────
let work = (make-workdir "backend-image-check")
let ports = (allocate-ports)

# ── Compiler binary ─────────────────────────────────────────────────────────
let compiler_bin = try {
  resolve-compiler $work $compiler_pkg $project_root
} catch {|err|
  fail $work $"resolve-compiler failed: ($err.msg? | default ($err | to nuon))"
}

# ── Deps stack ──────────────────────────────────────────────────────────────
let pg = (start-postgres $work $ports.pg)

let qdrant = (start-qdrant $work $ports.qdrant_http $ports.qdrant_grpc)
try {
  wait-http $"http://127.0.0.1:($ports.qdrant_http)/readyz"
} catch {
  fail $work "qdrant ready check failed"
}
# Server monomorphizes OpenAi3Small (1536 dims) — collection must match.
^curl -sf -X PUT $"http://127.0.0.1:($ports.qdrant_http)/collections/symbols" -H "Content-Type: application/json" -d '{"vectors":{"size":1536,"distance":"Cosine"}}' | ignore

let terminus = (start-terminus $work $ports.terminus)
try {
  wait-http-auth $"http://127.0.0.1:($ports.terminus)/api/info" "admin" "root"
} catch {
  fail $work "terminus ready check failed"
}
^curl -sf -u admin:root -X POST $"http://127.0.0.1:($ports.terminus)/api/db/admin/registry" -H "Content-Type: application/json" -d '{"label":"registry","comment":"backend-image-check"}' | ignore
^curl -sf -u admin:root $"http://127.0.0.1:($ports.terminus)/api/db/admin/registry" | ignore

let blobs = $work | path join "blobs"
let data_dir = $work | path join "server-data"
let cas_root = $work | path join "cas"
mkdir $blobs
mkdir $data_dir
mkdir $cas_root

# ── Compiler daemon ─────────────────────────────────────────────────────────
print $"==> starting compiler-daemon on 127.0.0.1:($ports.compiler)"
$env.NUDOX_COMPILER_ADDR = $"127.0.0.1:($ports.compiler)"
$env.NUDOX_CAS_ROOT = $cas_root
$env.CARGO_NET_OFFLINE = "true"
$env.RUSTUP_AUTO_INSTALL = "0"

if (which cargo | is-empty) {
  fail $work "cargo missing from PATH"
}
if (which rustc | is-empty) {
  fail $work "rustc missing from PATH"
}

let compiler_log = $work | path join "compiler.log"
let _compiler_pid = (spawn-bg $compiler_bin $compiler_log)

try {
  wait-http $"http://127.0.0.1:($ports.compiler)/health" 80
} catch {
  fail $work "compiler health check failed"
}
let cargo_path = (which cargo | get 0.path)
print $"    compiler healthy \(cargo=($cargo_path)\)"

# ── Server (binary the image ships) ─────────────────────────────────────────
print $"==> starting server on 127.0.0.1:($ports.server) \(role=all\)"
$env.NUDOX_SERVING_ADDRESS = $"127.0.0.1:($ports.server)"
$env.NUDOX_ROLE = "all"
$env.NUDOX_DEPLOYMENT = "development"
$env.NUDOX_COMPILER_ENDPOINT = $"http://127.0.0.1:($ports.compiler)"
$env.NUDOX_DEFINITIVE__NAME = "definitive"
$env.NUDOX_DEFINITIVE__DATA_DIRECTORY = $data_dir
$env.NUDOX_DEFINITIVE__ENDPOINTS__POSTGRES = $pg.url
$env.NUDOX_DEFINITIVE__ENDPOINTS__TERMINUS = $terminus.url
$env.NUDOX_DEFINITIVE__ENDPOINTS__TERMINUS_ORGANIZATION = "admin"
$env.NUDOX_DEFINITIVE__ENDPOINTS__TERMINUS_DATABASE = "registry"
$env.NUDOX_DEFINITIVE__ENDPOINTS__TERMINUS_USER = "admin"
$env.NUDOX_DEFINITIVE__ENDPOINTS__TERMINUS_PASSWORD = "root"
$env.NUDOX_DEFINITIVE__ENDPOINTS__QDRANT = $"http://127.0.0.1:($ports.qdrant_grpc)"
$env.NUDOX_DEFINITIVE__ENDPOINTS__QDRANT_COLLECTION = "symbols"
$env.NUDOX_DEFINITIVE__ENDPOINTS__OBJECT_STORE = $"file://($blobs)"
$env.RUST_LOG = "info,server=debug,registry=info"

let server_bin = $server | path join "bin/server"
let server_log = $work | path join "server.log"
let _server_pid = (spawn-bg $server_bin $server_log)

try {
  wait-http $"http://127.0.0.1:($ports.server)/healthz" 80
} catch {
  fail $work "server healthz failed"
}

let base_url = $"http://127.0.0.1:($ports.server)"

# ── HTTP assertions ─────────────────────────────────────────────────────────
print "==> GET /healthz"
let health_body = $work | path join "healthz.body"
let code = (^curl -s -o $health_body -w "%{http_code}" $"($base_url)/healthz" | str trim)
if $code != "200" {
  fail $work $"healthz expected 200, got ($code): (open --raw $health_body)"
}
print "    200 ok"

let axum_id = try {
  post-package $base_url $work "rust" "axum" $axum_version "axum"
} catch {|err|
  fail $work $"post axum failed: ($err.msg? | default ($err | to nuon))"
}

let zod_id = try {
  post-package $base_url $work "typescript" "zod" $zod_version "zod"
} catch {|err|
  fail $work $"post zod failed: ($err.msg? | default ($err | to nuon))"
}

try {
  await-stored $base_url $work $axum_id "axum" $deadline
  await-stored $base_url $work $zod_id "zod" $deadline
} catch {|err|
  fail $work $"await Stored failed: ($err.msg? | default ($err | to nuon))"
}

let blob_count = try {
  assert-blob-store $blobs 2
} catch {|err|
  fail $work $"blob assert failed: ($err.msg? | default ($err | to nuon))"
}

print "==> backend image full-pipeline check passed"
print $"    axum@($axum_version) → Stored \(($axum_id)\)"
print $"    zod@($zod_version) → Stored \(($zod_id)\)"
print $"    blobs: ($blob_count) objects under object_store"

cleanup-work $work
