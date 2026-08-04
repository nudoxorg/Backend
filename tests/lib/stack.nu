# Service lifecycle helpers for integration checks: postgres, qdrant, terminusdb.
# Process control uses PID files (portable across Nushell versions).

# Allocate a block of high, process-unique ports for the local stack.
export def allocate-ports [] {
  let base = 22000 + (($nu.pid mod 2000) * 10)
  {
    pg: ($base + 1)
    terminus: ($base + 2)
    qdrant_http: ($base + 3)
    qdrant_grpc: ($base + 4)
    server: ($base + 5)
    compiler: ($base + 6)
  }
}

# Create a workdir under TMPDIR and return its path.
export def make-workdir [name: string = "nudox-check"] {
  let tmp = ($env.TMPDIR? | default "/tmp")
  let work = $tmp | path join $"($name)-($nu.pid)"
  mkdir $work
  $work
}

# Dump tail of common log files under $work (for failure diagnostics).
export def dump-logs [work: string] {
  for name in [server.log compiler.log postgres.log qdrant.log terminus.log] {
    let p = $work | path join $name
    print --stderr $"---- ($name) \(tail\) ----"
    if ($p | path exists) {
      try {
        open --raw $p | lines | last 100 | str join "\n" | print --stderr
      } catch { }
    }
  }
}

# Spawn a shell command in the background; return pid.
# $cmdline is executed via bash so paths/args with spaces must be pre-quoted.
export def spawn-bg [cmdline: string, log: string] {
  let pid_file = $"($log).pid"
  ^bash -c $"($cmdline) > ($log) 2>&1 & echo $! > ($pid_file)"
  open --raw $pid_file | str trim | into int
}

export def kill-pid [pid: any] {
  if $pid == null { return }
  let p = (try { $pid | into int } catch { return })
  try { ^kill $p | ignore } catch { }
  try { ^bash -c $"wait ($p) 2>/dev/null" | ignore } catch { }
}

# ── Postgres ────────────────────────────────────────────────────────────────

export def start-postgres [work: string, port: int] {
  print $"==> starting postgres on 127.0.0.1:($port)"
  let pgdata = $work | path join "pgdata"
  $env.PGDATA = $pgdata
  $env.PGHOST = "127.0.0.1"
  $env.PGPORT = ($port | into string)

  ^initdb -D $pgdata -U nudox --auth=trust --no-sync --locale=C | ignore
  [
    "listen_addresses = '127.0.0.1'"
    $"port = ($port)"
    $"unix_socket_directories = '($work)'"
  ] | str join "\n" | save --append ($pgdata | path join "postgresql.conf")

  ^pg_ctl -D $pgdata -l ($work | path join "postgres.log") -w start
  ^createdb -h 127.0.0.1 -p ($port | into string) -U nudox nudox

  {
    pgdata: $pgdata
    url: $"postgres://nudox@127.0.0.1:($port)/nudox"
    port: $port
  }
}

export def stop-postgres [pgdata: string] {
  if ($pgdata | path exists) {
    try { ^pg_ctl -D $pgdata -m fast stop | ignore } catch { }
  }
}

# ── Qdrant ──────────────────────────────────────────────────────────────────

export def start-qdrant [work: string, http_port: int, grpc_port: int] {
  print $"==> starting qdrant http=($http_port) grpc=($grpc_port)"
  let qdir = $work | path join "qdrant"
  let storage = $qdir | path join "storage"
  mkdir $storage
  let cfg = $qdir | path join "config.yaml"
  [
    "log_level: WARN"
    "storage:"
    $"  storage_path: ($storage)"
    "service:"
    "  host: 127.0.0.1"
    $"  http_port: ($http_port)"
    $"  grpc_port: ($grpc_port)"
    "  enable_cors: true"
    "cluster:"
    "  enabled: false"
    "telemetry_disabled: true"
  ] | str join "\n" | save -f $cfg

  let log = $work | path join "qdrant.log"
  let pid = (spawn-bg $"qdrant --config-path ($cfg) --disable-telemetry" $log)

  {
    pid: $pid
    http_port: $http_port
    grpc_port: $grpc_port
    log: $log
  }
}

# ── TerminusDB ──────────────────────────────────────────────────────────────

export def start-terminus [work: string, port: int] {
  print $"==> starting terminusdb on 127.0.0.1:($port)"
  let db_path = $work | path join "terminus"
  mkdir $db_path
  $env.TERMINUSDB_SERVER_DB_PATH = $db_path
  $env.TERMINUSDB_SERVER_PORT = ($port | into string)

  ^terminusdb store init --key root | ignore

  let log = $work | path join "terminus.log"
  let pid = (spawn-bg "terminusdb serve" $log)

  {
    pid: $pid
    port: $port
    log: $log
    url: $"http://127.0.0.1:($port)"
  }
}
