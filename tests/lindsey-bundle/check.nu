# The cargo-bundle wrap contract, against a fake .app.
#
# A real `cargo bundle` of lindsey compiles gpui; that lives in
# `nix build .#lindsey-app`. The wrap is a post-processing step: copy oracles
# and the embed model into Resources/nudox, write a Contents/MacOS/lindsey
# wrapper that defaults the producer env vars. A fake binary that prints those
# vars is enough to prove the wrap, and running the Nix-built oracles is enough
# to prove the schema handshake the wrap is what puts on PATH.

def must-contain [haystack: string, needle: string, why: string] {
  if not ($haystack | str contains $needle) {
    print --stderr $"lindsey-bundle: missing {($needle)}"
    print --stderr $why
    print --stderr $haystack
    error make { msg: $"lindsey-bundle contract: ($why)" }
  }
}

let work = ($env.TMPDIR? | default "/tmp") | path join $"lindsey-bundle-($nu.pid)"
mkdir $work

# ── Fake .app ──────────────────────────────────────────────────────────────
let app = $work | path join "lindsey.app"
mkdir ($app | path join "Contents/MacOS")
mkdir ($app | path join "Contents/Resources")

let fake_bin = $app | path join "Contents/MacOS/lindsey"
[
  "#!/bin/sh"
  "echo GO=\"$NUDOX_GO_ORACLE_BIN\""
  "echo JAVA=\"$NUDOX_JAVA_ORACLE_CLASSES\""
  "echo CSHARP=\"$NUDOX_CSHARP_ORACLE\""
  "echo DOTNET=\"$NUDOX_DOTNET\""
  "echo MODEL=\"$NUDOX_EMBED_MODEL_DIR\""
  "echo LIBCLANG=\"$LIBCLANG_PATH\""
  "echo PATH=\"$PATH\""
] | str join (char newline) | save --raw $fake_bin
chmod +x $fake_bin

^($env.NUDOX_INSTALL_WRAPPER) $app

let wrapped = open --raw ($app | path join "Contents/MacOS/lindsey")
must-contain $wrapped $env.NUDOX_ENV_GO "wrapper must set the Go oracle override the producer looks up"
must-contain $wrapped $env.NUDOX_ENV_JAVA "wrapper must set the Java doclet-classes override"
must-contain $wrapped $env.NUDOX_ENV_CSHARP "wrapper must set the C# oracle.dll override"
must-contain $wrapped $env.NUDOX_ENV_DOTNET "wrapper must set NUDOX_DOTNET so the host is not scavenged from PATH"
must-contain $wrapped $env.NUDOX_ENV_MODEL "wrapper must set NUDOX_EMBED_MODEL_DIR (NoModelConfigured otherwise)"
must-contain $wrapped $env.NUDOX_ENV_LIBCLANG "wrapper must set LIBCLANG_PATH for the clang producer"
must-contain $wrapped "embed-model" "wrapper must default the embed model at the bundled copy, not a build-machine store path"

let resources = $app | path join "Contents/Resources/nudox"
if not ($resources | path join "nudox-go-oracle" | path exists) {
  error make { msg: "wrap did not copy nudox-go-oracle into Resources/nudox" }
}
if not ($resources | path join "java-oracle/nudox/oracle/Extractor.class" | path exists) {
  error make { msg: "wrap did not copy the Java doclet classes into Resources/nudox/java-oracle" }
}
if not ($resources | path join "csharp-oracle/oracle.dll" | path exists) {
  error make { msg: "wrap did not copy oracle.dll into Resources/nudox/csharp-oracle" }
}
if not ($resources | path join "embed-model/model.onnx" | path exists) {
  error make { msg: "wrap did not copy model.onnx into Resources/nudox/embed-model" }
}
# Darwin GUI links @rpath/libonnxruntime.1.dylib; Linux wrap has no ORT tarball.
if (uname | get kernel-name) == "Darwin" {
  if not ($app | path join "Contents/Frameworks/libonnxruntime.1.dylib" | path exists) {
    error make { msg: "wrap did not copy libonnxruntime.1.dylib into Contents/Frameworks (dyld @rpath abort at launch)" }
  }
}

let output = (^($app | path join "Contents/MacOS/lindsey") | complete)
if $output.exit_code != 0 {
  print --stderr $output.stderr
  error make { msg: $"wrapped fake lindsey exited ($output.exit_code)" }
}
let stdout = $output.stdout
must-contain $stdout "GO=" "fake binary must print the Go oracle path"
must-contain $stdout "nudox-go-oracle" "default GO oracle is the bundled copy"
must-contain $stdout "java-oracle" "default Java oracle is the bundled classes"
must-contain $stdout "csharp-oracle" "default C# oracle is the bundled oracle.dll"
must-contain $stdout "embed-model" "default embed model is the bundled copy"

# ── Live Go oracle schema emit ─────────────────────────────────────────────
let fixture = $work | path join "fixture"
mkdir $fixture
"module example.com/fixture\ngo 1.23\n" | save --raw ($fixture | path join "go.mod")
"package fixture\n\nfunc Hello() string { return \"ok\" }\n" | save --raw ($fixture | path join "hello.go")

let oracle_bin = $env.NUDOX_GO_ORACLE | path join "bin/nudox-go-oracle"
if not ($oracle_bin | path exists) {
  error make { msg: $"go-oracle package has no bin/nudox-go-oracle at ($oracle_bin)" }
}

$env.GOCACHE = ($work | path join "go-cache")
$env.GOPROXY = "off"
$env.GOSUMDB = "off"
$env.GO111MODULE = "on"
$env.GOFLAGS = "-mod=mod"
mkdir $env.GOCACHE

let payload = (^($oracle_bin) $fixture | complete)
if $payload.exit_code != 0 {
  print --stderr $payload.stderr
  error make { msg: $"nudox-go-oracle exited ($payload.exit_code) over the wrap-check fixture" }
}
let schema = ($payload.stdout | from json | get schemaVersion)
if $schema != 2 {
  print --stderr $payload.stdout
  error make { msg: $"nudox-go-oracle emitted schemaVersion=($schema), lindsey reads 2; this is the schema-0-vs-schema-2 wrap failure" }
}

# ── Live C# oracle schema emit ─────────────────────────────────────────────
let cs_dll = $resources | path join "csharp-oracle/oracle.dll"
let cs_fixture = $work | path join "cs-fixture"
mkdir $cs_fixture
"public static class Hello { public static string Greet() => \"ok\"; }\n" | save --raw ($cs_fixture | path join "Hello.cs")

let cs_payload = (^dotnet $cs_dll --mode source --root $cs_fixture --assembly-name fixture | complete)
if $cs_payload.exit_code != 0 {
  print --stderr $cs_payload.stderr
  error make { msg: $"oracle.dll exited ($cs_payload.exit_code) over the wrap-check fixture" }
}
let cs_format = ($cs_payload.stdout | from json | get format)
if $cs_format != 1 {
  print --stderr $cs_payload.stdout
  error make { msg: $"oracle.dll emitted format=($cs_format), lindsey reads 1; this is the stale-oracle wrap failure" }
}

print "lindsey-bundle: wrap contract, go oracle schema 2, csharp oracle format 1 ok"
