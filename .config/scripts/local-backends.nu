#!/usr/bin/env nu
# Bring up (and down) the local backend stack the `index` crate's
# `tests/server_common::assembled_server`/`required_assembled_server` gate
# needs when `SERVER_TEST_BACKENDS=1` is set. See docs/TESTING.md's "Live"
# tier for the full picture.
#
# What actually needs a live network service (see
# `workspace/index/server/config.rs`'s `Endpoints::localhost_defaults`):
#   - qdrant (vector store, gRPC on 127.0.0.1:6334): a real dependency, no
#     local-file fallback. Started here, see `find-qdrant`.
#   - embeddings (OpenAI-compatible HTTP): the compiled-in test brand
#     (`server_common::TestModel = JinaCodeV2`) requests model id
#     `jinaai/jina-embeddings-v2-base-code`, whose canonical weights are a
#     641 MB pinned ONNX artifact this environment cannot fetch/serve, and
#     that id isn't a real Ollama model name either. `stub-embeddings-server.py`
#     listens on 127.0.0.1:21434 (NOT Ollama's own 11434 -- a real Ollama may
#     already be running as a host service and is left alone) and forwards
#     each request to that real local Ollama under a model it actually has
#     (`nomic-embed-text` by default), so requests get genuine embeddings when
#     an upstream is reachable, degrading to a documented synthetic fallback
#     otherwise. See that file's docstring for the full picture. Because the
#     port differs from the config default, tests need
#     `NUDOX_DEFINITIVE__ENDPOINTS__EMBEDDINGS=http://127.0.0.1:21434/v1/embeddings`
#     alongside `SERVER_TEST_BACKENDS=1` -- `main up` below prints (and writes
#     to `.local-backends/env.sh`) the exact line.
#
# What does NOT need a service, despite being named a "backend" elsewhere:
#   - catalog: `Endpoints::localhost_defaults().catalog_directory` is a
#     plain local directory (DoltLite, opened in-process). No server.
#   - object store: defaults to a `file://` URL under the OS temp dir. No
#     server.
#   - "terminus": vestigial in one doc comment only
#     (`workspace/index/server/config.rs`); no code path connects to it.
#
# State (PIDs, qdrant storage) lives under `$PRJ_ROOT/.local-backends/` --
# gitignored, safe to delete any time both processes are stopped.

use std/log

def state-dir [] {
    let root = ($env.PRJ_ROOT? | default (git rev-parse --show-toplevel | str trim))
    $root | path join ".local-backends"
}

def qdrant-pid-file [] { state-dir | path join "qdrant.pid" }
def embeddings-pid-file [] { state-dir | path join "embeddings.pid" }
def qdrant-log-file [] { state-dir | path join "qdrant.log" }
def embeddings-log-file [] { state-dir | path join "embeddings.log" }

# ── The serving tier (`nudox-serve`) ─────────────────────────────────────────
# Distinct from the *test* tier above. Tests need the stub on 21434 because
# `server_common::TestModel` is hardcoded to `JinaCodeV2`, whose model id
# (`jinaai/jina-embeddings-v2-base-code`) no Ollama registry has. `nudox-serve`
# has no such problem: `main.rs`'s `EmbedModel` is `NomicEmbedText`, whose
# `ModelId` is literally `nomic-embed-text` -- exactly what Ollama serves. So
# the serving tier talks to the REAL Ollama directly on 11434 and the stub is
# not in its path at all.
def serve-pid-file [] { state-dir | path join "serve.pid" }
def serve-log-file [] { state-dir | path join "serve.log" }

# `SourceConfig::data_directory()` defaults to `$TMPDIR/nudox/<source>`. On
# macOS that is a `/var/folders/...` path the OS periodically purges, which
# silently empties the tantivy symbol index while qdrant (durable elsewhere)
# keeps its vectors -- lexical search then returns zero hits forever while
# `/readyz` still reports `{"ready":true,"degraded":[]}`. Pinning it under
# `.local-backends/` makes the lexical plane survive a reboot.
def serve-data-dir [] { state-dir | path join "nudox-data" }
def serve-catalog-dir [] { state-dir | path join "catalog" }

const SERVE_PORT = 8080
const OLLAMA_PORT = 11434
const OLLAMA_EMBED_URL = "http://127.0.0.1:11434/v1/embeddings"

# The model id `NomicEmbedText::id()` returns; Ollama must have it pulled.
const OLLAMA_EMBED_MODEL = "nomic-embed-text"

def is-alive [pid: int] {
    (do -i { ^kill -0 $pid } | complete).exit_code == 0
}

def wait-for-http [url: string, timeout_secs: int] {
    let deadline = (date now) + ($timeout_secs * 1sec)
    loop {
        let ok = (do -i { ^curl -sf -o /dev/null -m 2 $url } | complete).exit_code == 0
        if $ok {
            return true
        }
        if (date now) > $deadline {
            return false
        }
        sleep 200ms
    }
}

# Locate a `qdrant` binary without forcing nixpkgs' from-source build (which
# has no aarch64-darwin binary-cache hit and would add minutes to every
# devshell entry if it were a default package -- see flake.nix's comment
# where `qdrant` was deliberately left out of the package list). Preference
# order: PATH (an operator who *did* add it, or a non-Nix install), then an
# already-realized `/nix/store/*-qdrant-*` output (this environment ships one
# pre-built; a fresh machine without it needs `nix build nixpkgs#qdrant`
# once, or add it to the devshell packages knowingly accepting the build
# cost).
def find-qdrant [] {
    if (has-command qdrant) {
        return "qdrant"
    }
    let candidates = (
        ls /nix/store
        | where name =~ '-qdrant-[0-9]+\.[0-9]+(\.[0-9]+)?$'
        | where type == dir
        | get name
        | sort
    )
    for candidate in $candidates {
        let bin = ($candidate | path join "bin" "qdrant")
        if ($bin | path exists) {
            return $bin
        }
    }
    null
}

# Spawn `argv` fully detached from this nu process (new session, stdio
# redirected to `log_file`, backgrounded) and return its OS pid. `job spawn`
# was tried first and rejected: its children are reaped when the spawning nu
# process exits (or, worse, kept alive but silently unreachable by `ps`'s pid
# column in some runs), so a `local-backends.nu up` invocation that exits
# after starting things left nothing running a moment later. `setsid` (or, on
# macOS where it's absent, plain nohup backgrounding, since macOS
# already re-parents backgrounded children to launchd rather than the shell)
# via `sh -c` is the standard-library way to actually daemonize.
def spawn-detached [argv: list<string>, log_file: string, env_vars: record] {
    let joined = ($argv | each {|a| $"'($a | str replace --all "'" "'\\''")'"} | str join ' ')
    let cmd = $"nohup ($joined) > '($log_file)' 2>&1 & echo $!"
    with-env $env_vars {
        (^sh -c $cmd | str trim | into int)
    }
}

def start-qdrant [] {
    if (wait-for-http "http://127.0.0.1:6333/healthz" 0) {
        log info "qdrant already reachable on 127.0.0.1:6333 -- reusing (not managed by this script; leaving it running on `down`)"
        return
    }
    let pid_file = (qdrant-pid-file)
    if ($pid_file | path exists) {
        let existing = (open $pid_file | into int)
        if (is-alive $existing) {
            log info $"qdrant already running \(pid ($existing)\), waiting for health"
        }
    }
    let qdrant_bin = (find-qdrant)
    if ($qdrant_bin == null) {
        error make {msg: "no qdrant binary found on PATH or under /nix/store -- run `nix build nixpkgs#qdrant` once, or add `qdrant` to flake.nix's devShell packages and re-enter the shell"}
    }
    mkdir (state-dir)
    let storage_dir = (state-dir | path join "qdrant-storage")
    mkdir $storage_dir
    log info $"starting qdrant \(binary: ($qdrant_bin), storage: ($storage_dir)\)"
    let pid = (spawn-detached [$qdrant_bin] (qdrant-log-file) {
        QDRANT__STORAGE__STORAGE_PATH: $storage_dir
        QDRANT__SERVICE__GRPC_PORT: "6334"
        QDRANT__SERVICE__HTTP_PORT: "6333"
        QDRANT__TELEMETRY_DISABLED: "true"
        QDRANT__LOG_LEVEL: "warn"
    })
    $pid | save --force $pid_file
    if not (wait-for-http "http://127.0.0.1:6333/healthz" 30) {
        error make {msg: $"qdrant \(pid ($pid)\) did not become healthy within 30s -- see (qdrant-log-file)"}
    }
    log info $"qdrant is healthy on 127.0.0.1:6333 \(http\) / 6334 \(grpc\), pid ($pid)"
}

const EMBEDDINGS_PORT = 21434

def start-embeddings [] {
    if (wait-for-http $"http://127.0.0.1:($EMBEDDINGS_PORT)/healthz" 0) {
        log info $"stub embeddings server already reachable on 127.0.0.1:($EMBEDDINGS_PORT) -- reusing"
        return
    }
    if not (has-command python3) {
        error make {msg: "python3 is not on PATH -- needed for the stub embeddings server"}
    }
    mkdir (state-dir)
    let script = ($env.PRJ_ROOT | path join ".config" "scripts" "stub-embeddings-server.py")
    let upstream_alive = (wait-for-http "http://127.0.0.1:11434/api/tags" 0)
    if $upstream_alive {
        log info $"starting stub embeddings server on 127.0.0.1:($EMBEDDINGS_PORT) \(real upstream: Ollama on 11434\)"
    } else {
        log info $"starting stub embeddings server on 127.0.0.1:($EMBEDDINGS_PORT) \(no Ollama reachable on 11434 -- synthetic fallback only\)"
    }
    let pid = (spawn-detached ["python3" $script] (embeddings-log-file) {
        STUB_EMBEDDINGS_PORT: ($EMBEDDINGS_PORT | into string)
    })
    $pid | save --force (embeddings-pid-file)
    if not (wait-for-http $"http://127.0.0.1:($EMBEDDINGS_PORT)/healthz" 10) {
        error make {msg: $"stub embeddings server \(pid ($pid)\) did not come up within 10s -- see (embeddings-log-file)"}
    }
    log info $"stub embeddings server is healthy on 127.0.0.1:($EMBEDDINGS_PORT), pid ($pid)"
}

def stop-one [pid_file: string, label: string] {
    if not ($pid_file | path exists) {
        log info $"($label): not running \(no pid file\)"
        return
    }
    let pid = (open $pid_file | into int)
    if (is-alive $pid) {
        ^kill $pid
        log info $"($label): stopped \(pid ($pid)\)"
    } else {
        log info $"($label): pid file present but process ($pid) is not alive"
    }
    rm -f $pid_file
}

def has-command [name: string] {
    (which $name | is-not-empty)
}

# Whether a real Ollama is reachable AND has the embedding model pulled. Both
# halves matter: a reachable Ollama missing `nomic-embed-text` answers every
# embed request with a 404, and `nudox-serve` surfaces that as a 503 on every
# semantic query (`ServerError::Embed` -> HTTP 503) rather than degrading.
def ollama-ready [] {
    if not (wait-for-http $"http://127.0.0.1:($OLLAMA_PORT)/api/tags" 0) {
        return {reachable: false, model: false}
    }
    let tags = (do -i { ^curl -sf -m 5 $"http://127.0.0.1:($OLLAMA_PORT)/api/tags" } | complete)
    let has_model = ($tags.stdout | str contains $OLLAMA_EMBED_MODEL)
    {reachable: true, model: $has_model}
}

# Verify the embedder end to end and return the observed dimensionality, so a
# dimension drift (which qdrant would only reject at insert time, or worse,
# silently accept into the wrong collection) is caught before serving.
def probe-embedding-dimensions [] {
    let body = ({model: $OLLAMA_EMBED_MODEL, input: ["dimension probe"], encoding_format: "float"} | to json)
    let out = (do -i {
        ^curl -sf -m 30 $OLLAMA_EMBED_URL -H "Content-Type: application/json" -d $body
    } | complete)
    if $out.exit_code != 0 {
        return null
    }
    try {
        ($out.stdout | from json | get data.0.embedding | length)
    } catch {
        null
    }
}

def main [] {
    log info "usage: local-backends <up|serve|ingest|down|status>"
}

# Start `nudox-serve` against the local backends with a REAL embedder.
#
# Nothing here overrides `endpoints.embeddings`: its built-in default already
# is `http://127.0.0.1:11434/v1/embeddings` (`config.rs`'s `defaults::embeddings`).
# It is passed explicitly anyway so the command is self-documenting and so a
# stray `NUDOX_...__EMBEDDINGS` left over from the test tier (which points at
# the 21434 stub) cannot leak in from the caller's environment.
def "main serve" [
    --rebuild       # cargo build the binary first
] {
    $env.PRJ_ROOT = ($env.PRJ_ROOT? | default (git rev-parse --show-toplevel | str trim))
    mkdir (state-dir)

    if (wait-for-http $"http://127.0.0.1:($SERVE_PORT)/healthz" 0) {
        log info $"nudox-serve already reachable on 127.0.0.1:($SERVE_PORT) -- reusing"
        return
    }

    if not (wait-for-http "http://127.0.0.1:6333/healthz" 0) {
        error make {msg: "qdrant is not reachable on 127.0.0.1:6333 -- run `local-backends up` first"}
    }

    let ollama = (ollama-ready)
    if not $ollama.reachable {
        error make {msg: $"no Ollama on 127.0.0.1:($OLLAMA_PORT). `nudox-serve` is compiled with `EmbedModel = NomicEmbedText` \(server/main.rs\), so its embedder is Ollama serving ($OLLAMA_EMBED_MODEL). Start Ollama, then `ollama pull ($OLLAMA_EMBED_MODEL)`."}
    }
    if not $ollama.model {
        error make {msg: $"Ollama is up but has no ($OLLAMA_EMBED_MODEL) model -- run `ollama pull ($OLLAMA_EMBED_MODEL)`. Without it every semantic query answers 503."}
    }

    let dims = (probe-embedding-dimensions)
    if $dims == null {
        error make {msg: $"Ollama is up and has ($OLLAMA_EMBED_MODEL), but ($OLLAMA_EMBED_URL) did not return a usable embedding"}
    }
    # `NomicEmbedText::DIMENSIONS` is 768 and `CollectionConfig::NomicDev` builds
    # a 768-wide qdrant collection. `Embedding::from_vec` rejects any other
    # width, so a mismatch here is a hard stop, not a warning.
    if $dims != 768 {
        error make {msg: $"($OLLAMA_EMBED_MODEL) returned ($dims)-dimensional vectors, but the compiled-in NomicEmbedText brand is 768-dimensional \(collection symbols__dev_nomic_embed_text_768\). Refusing to serve into a mismatched collection."}
    }
    log info $"embedder verified: ($OLLAMA_EMBED_MODEL) via ($OLLAMA_EMBED_URL), ($dims) dimensions"

    let binary = ($env.PRJ_ROOT | path join "target" "debug" "nudox-serve")
    if $rebuild or (not ($binary | path exists)) {
        log info "building nudox-serve (--features server)"
        ^cargo build -p index --features server --bin nudox-serve
    }
    if not ($binary | path exists) {
        error make {msg: $"($binary) does not exist -- build it with `cargo build -p index --features server --bin nudox-serve`"}
    }

    mkdir (serve-data-dir)
    mkdir (serve-catalog-dir)
    # `OTEL_SDK_DISABLED` also gates pyroscope (`heart::telemetry::config`).
    # Two reasons to set it here. The mild one: no collector listens on :4318 or
    # :4040 in a dev shell, so the profiler logs an ERROR every ten seconds
    # about a session it cannot send. The serious one: with pyroscope's
    # sampling timer running, this binary reliably aborted after a few served
    # requests with
    #
    #   fatal runtime error: current thread handle already set during thread spawn
    #
    # — a std-level abort, not a panic, so no backtrace and no clean shutdown.
    # Disabling the SDK removes the profiler's threads and the abort with them.
    # That is a workaround at the launcher, not a fix; the abort is real and
    # reproducible with telemetry on.
    let pid = (spawn-detached [$binary] (serve-log-file) {
        NUDOX_DEFINITIVE__DATA_DIRECTORY: (serve-data-dir)
        NUDOX_DEFINITIVE__ENDPOINTS__CATALOG_DIRECTORY: (serve-catalog-dir)
        NUDOX_DEFINITIVE__ENDPOINTS__EMBEDDINGS: $OLLAMA_EMBED_URL
        OTEL_SDK_DISABLED: "true"
        RUST_LOG: "info"
    })
    $pid | save --force (serve-pid-file)
    if not (wait-for-http $"http://127.0.0.1:($SERVE_PORT)/healthz" 60) {
        error make {msg: $"nudox-serve \(pid ($pid)\) did not become healthy within 60s -- see (serve-log-file)"}
    }
    log info $"nudox-serve is healthy on 127.0.0.1:($SERVE_PORT), pid ($pid)"
    print ""
    print "Index up. Ingest a real package with:"
    print "  nu .config/scripts/local-backends.nu ingest rust itoa 1.0.11"
    print "Point the GUI at it (this is already lindsey's default) with:"
    print $"  NUDOX_SERVER_URL=http://127.0.0.1:($SERVE_PORT) cargo run --manifest-path workspace/gui/Cargo.toml --bin lindsey"
}

# Ingest one real package and wait until it is searchable on BOTH planes.
#
# # Why this re-POSTs after the package reaches `Stored`
#
# The Text sink gets exactly one outbox intent per package, emitted by
# `store::apply::apply_run`'s `CatalogOp::UpsertVersion` arm -- i.e. when the
# version row is first written, at registration time. The package's symbols do
# not exist yet at that moment: they appear ~a minute later, when compilation
# finishes. `runtime::text::poll`'s `poll_once` consumes that intent within its
# poll interval, calls `symbols_for(package)`, gets `NotFound`, maps it to an
# empty batch (poll.rs:138-144) and still advances the durable watermark
# (poll.rs:153). `store/lifecycle.rs` deliberately no longer fans out to the
# Text sink on phase transitions, so nothing re-signals once the symbols land.
# Net effect: a freshly ingested package is semantically searchable but
# lexically invisible, permanently, with no error anywhere.
#
# Re-registering the package emits a fresh `UpsertVersion` intent, which the
# poller now resolves against a catalog that *does* have the symbols. This is
# an operational workaround for that defect, not a fix -- see docs/TESTING.md.
def "main ingest" [
    ecosystem: string   # rust | go | npm | pypi | maven | nuget
    name: string        # package name
    version: string     # exact version
    --timeout: int = 600  # seconds to wait for the compile phase
] {
    $env.PRJ_ROOT = ($env.PRJ_ROOT? | default (git rev-parse --show-toplevel | str trim))
    if not (wait-for-http $"http://127.0.0.1:($SERVE_PORT)/healthz" 0) {
        error make {msg: $"nudox-serve is not reachable on 127.0.0.1:($SERVE_PORT) -- run `local-backends serve` first"}
    }

    let body = ({ecosystem: $ecosystem, name: $name, version: $version} | to json)
    let post = {|| (^curl -sf -m 120 -X POST $"http://127.0.0.1:($SERVE_PORT)/packages" -H "Content-Type: application/json" -d $body | from json) }

    let initial = (do $post)
    let package = $initial.package
    log info $"($ecosystem)/($name)@($version) registered as ($package)"

    # Phase 1: wait for the compile pipeline to reach its terminal `Stored`
    # state (`Unindexed` -> `Progressing{Acquiring,Extracting,Compiling,Emitting}`
    # -> `Stored{hash}`). Vectors are written during this phase.
    let deadline = (date now) + ($timeout * 1sec)
    mut stored = false
    while (date now) < $deadline {
        let state = (
            ^curl -sf -m 30 $"http://127.0.0.1:($SERVE_PORT)/packages/($package)"
            | from json
            | get state
        )
        let encoded = ($state | to json)
        if ($encoded | str contains "Stored") {
            $stored = true
            break
        }
        # `Failed`/`DeadLettered` are terminal for this attempt. Reporting the
        # server's own message immediately beats burning the whole timeout and
        # then saying only "did not reach Stored" -- the interesting half of a
        # failed ingest is *why*, and it is already in the state.
        if ($encoded | str contains "Failed") or ($encoded | str contains "DeadLettered") {
            error make {msg: $"($name)@($version) failed to index:\n($encoded)"}
        }
        sleep 5sec
    }
    if not $stored {
        error make {msg: $"($name)@($version) did not reach `Stored` within ($timeout)s -- see (serve-log-file)"}
    }
    log info $"($name)@($version) compiled and stored"

    # Phase 2: re-register so the Text sink gets an intent it can actually
    # resolve into symbols (see this command's doc comment).
    do $post | ignore
    sleep 10sec
    log info $"($name)@($version) re-registered to fan out to the text index"

    print ""
    print "Verify both planes:"
    print $"  curl -s -XPOST http://127.0.0.1:($SERVE_PORT)/search -H 'Content-Type: application/json' -d '{\"target\":\"Symbols\",\"text\":\"($name)\",\"page\":{\"limit\":5}}'"
    print $"  curl -s -XPOST http://127.0.0.1:($SERVE_PORT)/search -H 'Content-Type: application/json' -d '{\"target\":\"Symbols\",\"text\":\"what does ($name) do\",\"mode\":\"semantic\",\"page\":{\"limit\":5}}'"
}

def "main up" [] {
    $env.PRJ_ROOT = ($env.PRJ_ROOT? | default (git rev-parse --show-toplevel | str trim))
    mkdir (state-dir)
    start-qdrant
    start-embeddings
    let env_file = (state-dir | path join "env.sh")
    $"export SERVER_TEST_BACKENDS=1\nexport NUDOX_DEFINITIVE__ENDPOINTS__EMBEDDINGS=http://127.0.0.1:($EMBEDDINGS_PORT)/v1/embeddings\n"
        | save --force $env_file
    print ""
    print "Backends up. Run tests with:"
    print $"  source (state-dir)/env.sh && cargo nextest run -p index --features server --run-ignored all"

    let ollama = (ollama-ready)
    if $ollama.reachable and $ollama.model {
        let dims = (probe-embedding-dimensions)
        print ""
        print $"Real embedder available: ($OLLAMA_EMBED_MODEL) on 127.0.0.1:($OLLAMA_PORT), ($dims) dimensions."
        print "Start a live index against it with:"
        print "  nu .config/scripts/local-backends.nu serve"
    } else if $ollama.reachable {
        print ""
        print $"NOTE: Ollama is up but has no ($OLLAMA_EMBED_MODEL) -- `ollama pull ($OLLAMA_EMBED_MODEL)`."
        print "Until then the stub falls back to synthetic vectors and `serve` refuses to start."
    } else {
        print ""
        print $"NOTE: no Ollama on 127.0.0.1:($OLLAMA_PORT). The stub is on synthetic vectors and"
        print "`local-backends serve` will refuse to start (it requires a real embedder)."
    }
    print "Tear down with: local-backends down"
}

def "main down" [] {
    $env.PRJ_ROOT = ($env.PRJ_ROOT? | default (git rev-parse --show-toplevel | str trim))
    stop-one (serve-pid-file) "nudox-serve"
    stop-one (qdrant-pid-file) "qdrant"
    stop-one (embeddings-pid-file) "stub embeddings server"
}

def "main status" [] {
    $env.PRJ_ROOT = ($env.PRJ_ROOT? | default (git rev-parse --show-toplevel | str trim))
    print $"qdrant \(127.0.0.1:6333\): (if (wait-for-http 'http://127.0.0.1:6333/healthz' 0) {'reachable'} else {'unreachable'})"
    print $"stub embeddings \(127.0.0.1:($EMBEDDINGS_PORT)\): (if (wait-for-http $'http://127.0.0.1:($EMBEDDINGS_PORT)/healthz' 0) {'reachable'} else {'unreachable'})"
    let ollama = (ollama-ready)
    let ollama_state = if $ollama.reachable and $ollama.model {
        $"reachable, ($OLLAMA_EMBED_MODEL) pulled \((probe-embedding-dimensions) dimensions\)"
    } else if $ollama.reachable {
        $"reachable but ($OLLAMA_EMBED_MODEL) is NOT pulled"
    } else {
        "unreachable"
    }
    print $"ollama \(127.0.0.1:($OLLAMA_PORT), the real embedder\): ($ollama_state)"
    print $"nudox-serve \(127.0.0.1:($SERVE_PORT)\): (if (wait-for-http $'http://127.0.0.1:($SERVE_PORT)/healthz' 0) {'reachable'} else {'unreachable'})"
    for pair in [[(qdrant-pid-file) "qdrant (managed by this script)"] [(embeddings-pid-file) "stub embeddings server"] [(serve-pid-file) "nudox-serve"]] {
        let pid_file = $pair.0
        let label = $pair.1
        if ($pid_file | path exists) {
            let pid = (open $pid_file | into int)
            if (is-alive $pid) {
                print $"($label): running \(pid ($pid)\)"
            } else {
                print $"($label): pid file present, process ($pid) is dead"
            }
        } else {
            print $"($label): not running"
        }
    }
}
