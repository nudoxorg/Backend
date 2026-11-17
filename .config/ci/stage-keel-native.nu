# Development-only worker staging for the focused Linux gate. These are the
# final bytes observed by the owner build, not a formal release receipt.
use lib.nu run-bounded

def checked [program: string, ...args: string] {
    let result = run-external $program ...$args | complete
    if $result.exit_code != 0 {
        error make {msg: $"($program) failed: ($result.stderr)"}
    }
    $result.stdout | str trim
}

def digest [path: path] {
    open --raw $path | hash sha256
}

def needed [image: path] {
    checked "readelf" "-d" $image
    | lines
    | parse --regex 'Shared library: \[(?<name>[^\]]+)\]'
    | get name
    | sort
}

def interpreter [image: path] {
    let matches = (
        checked "readelf" "-l" $image
        | lines
        | parse --regex 'Requesting program interpreter: (?<path>[^\]]+)'
        | get path
    )
    if ($matches | length) != 1 or not (($matches | first) | str starts-with "/") {
        error make {msg: $"worker ($image) lacks exactly one absolute ELF interpreter"}
    }
    $matches | first
}

def soname [image: path] {
    checked "readelf" "-d" $image | lines | parse --regex 'Library soname: \[(?<name>[^\]]+)\]' | get name
}

def library-for [worker: path] {
    let matches = (
        checked "ldd" $worker
        | lines
        | parse --regex 'libgcc_s\.so\.1\s*=>\s*(?<path>/\S+)'
        | get path
    )
    if ($matches | length) != 1 { error make {msg: $"worker ($worker) has no unique libgcc"} }
    checked "realpath" ($matches | first)
}

def valid-id [id: string] {
    ($id =~ '^[0-9a-f]{64}$') and not ($id =~ '^0+$')
}

# Build, relocate and observe workers *before* building any owner application.
# The caller builds all apps with the returned anchor environment and then
# calls verify-keel-native after copying those binaries into stage/bin.
export def prepare-keel-native [
    host: string
    target: path
    jobs: int
    stage: path
] {
    let raw = $target | path join $host "debug"
    let sqlite_target = $target | path join "keel-sqlite"
    let sqlite_raw = $sqlite_target | path join $host "debug"
    let bin = $stage | path join "bin"
    let lib = $stage | path join "lib"
    let observers = $stage | path join "observers"
    mkdir $bin $lib $observers
    ^chmod 700 $stage $bin $lib $observers
    run-bounded 40min "cargo" "build" "--locked" "--target" $host "--jobs" ($jobs | into string) "-p" "backend-frontend-stylo" "--bin" "backend-stylo-worker"
    run-bounded 20min "cargo" "build" "--locked" "--target" $host "--jobs" ($jobs | into string) "-p" "backend-compile" "--example" "stylo_identity_observer"
    run-bounded 40min "cargo" "build" "--locked" "--target" $host "--jobs" ($jobs | into string) "--manifest-path" "frontends/sqlite/Cargo.toml" "--target-dir" $sqlite_target "--bin" "sqlite-worker" "--example" "installed_basis_observer"
    let raw_css = $raw | path join "backend-stylo-worker"
    let raw_sql = $sqlite_raw | path join "sqlite-worker"
    let css_observer = $raw | path join "examples/stylo_identity_observer"
    let sql_observer = $sqlite_raw | path join "examples/installed_basis_observer"
    ^cp $css_observer $sql_observer $observers
    ^chmod 700 ($observers | path join "stylo_identity_observer") ($observers | path join "installed_basis_observer")
    let css_lib = library-for $raw_css
    let sql_lib = library-for $raw_sql
    if (digest $css_lib) != (digest $sql_lib) { error make {msg: "CSS and SQLite do not share the same libgcc bytes"} }
    let system_names = (
        open --raw "tools/package/linux_system_sonames.txt"
        | lines
        | where {|name| $name != "" }
    )
    for image in [$raw_css $raw_sql] {
        let dependencies = needed $image
        let foreign = $dependencies | where {|name| $name not-in $system_names }
        if $foreign != ["libgcc_s.so.1"] { error make {msg: $"unexpected worker ELF dependencies: ($dependencies)"} }
    }
    let library_needed = needed $css_lib
    if (soname $css_lib) != ["libgcc_s.so.1"] { error make {msg: "libgcc SONAME differs"} }
    let css_interpreter = interpreter $raw_css
    let sql_interpreter = interpreter $raw_sql
    if not (($library_needed | where {|name| $name not-in $system_names }) | is-empty) {
        error make {msg: "libgcc has unexpected non-platform dependencies"}
    }
    let worker_css = $bin | path join "backend-stylo-worker"
    let worker_sql = $bin | path join "backend-sqlite-worker"
    let library = $lib | path join "libgcc_s.so.1"
    ^cp --dereference $css_lib $library
    ^cp $raw_css $worker_css
    ^cp $raw_sql $worker_sql
    ^chmod 600 $library
    ^chmod 700 $worker_css $worker_sql
    let css_needed = needed $worker_css
    let sql_needed = needed $worker_sql
    checked "patchelf" "--set-rpath" '$ORIGIN' $library | ignore
    checked "patchelf" "--set-rpath" '$ORIGIN/../lib' $worker_css | ignore
    checked "patchelf" "--set-rpath" '$ORIGIN/../lib' $worker_sql | ignore
    if (soname $library) != ["libgcc_s.so.1"] or (interpreter $worker_css) != $css_interpreter or (interpreter $worker_sql) != $sql_interpreter or (needed $library) != $library_needed or (needed $worker_css) != $css_needed or (needed $worker_sql) != $sql_needed {
        error make {msg: "relocation changed ELF dependencies"}
    }
    if (checked "patchelf" "--print-rpath" $library) != '$ORIGIN' or (checked "patchelf" "--print-rpath" $worker_css) != '$ORIGIN/../lib' or (checked "patchelf" "--print-rpath" $worker_sql) != '$ORIGIN/../lib' {
        error make {msg: "relocated private library search path differs"}
    }
    let css = checked $css_observer $worker_css $library | from json
    let sqlite = checked $sql_observer $worker_sql $library | from json
    if $css.state != "observed-helper" or not (valid-id $css.identity) or $sqlite.state != "observed-build-helper" or not (valid-id $sqlite.artifact_identity) {
        error make {msg: "invalid native worker observer identity"}
    }
    let basis = $sqlite.basis
    let lock_bytes = (
        digest "frontends/sqlite/Cargo.lock"
        | split chars
        | chunks 2
        | each {|digits| $digits | str join | into int --radix 16 }
    )
    if $basis.engine != "Syntaqlite4e4de76" or $basis.native_revision != "4e4de76312429fb2235ae093ab2125914320d5bd" or $basis.recipe != "syntaqlite-sqlite-fmt-analysis-dqs-default-v1" or $basis.lock_sha256 != $lock_bytes or ($basis.support_sha256 | length) != 32 or ($basis.support_sha256 | all {|byte| $byte == 0 }) or $basis.settings.Sqlite != {version: 2147483647, cflags: 0, analyzer_dqs_fallback: true} {
        error make {msg: "SQLite observer returned an unsupported native basis"}
    }
    let basis_json = $basis | to json --raw
    if ($basis_json | str length) > 8192 { error make {msg: "SQLite basis exceeds owner limit"} }
    let observation = {
        schema: "nudox.keel-development-native-observation.v1"
        source_revision: (checked "git" "rev-parse" "HEAD")
        source_status: (checked "git" "status" "--porcelain")
        tracked_diff_sha256: (checked "git" "diff" "--binary" "HEAD" | hash sha256)
        rustc_version: (checked "rustc" "-vV")
        host: $host
        workspace_lock_sha256: (digest "Cargo.lock")
        sqlite_lock_sha256: (digest "frontends/sqlite/Cargo.lock")
        stylo_worker_sha256: (digest $worker_css)
        sqlite_worker_sha256: (digest $worker_sql)
        libgcc_sha256: (digest $library)
        stylo_observer_sha256: (digest $css_observer)
        sqlite_observer_sha256: (digest $sql_observer)
        stylo: $css
        sqlite: $sqlite
    }
    $observation | to json | save ($stage | path join "development-observation.json")
    {NUDOX_BUILD_STYLO_WORKER_ID: $css.identity, NUDOX_BUILD_SQLITE_WORKER_ID: $sqlite.artifact_identity, NUDOX_BUILD_SQLITE_BASIS_JSON: $basis_json}
}

# The owner must bind exactly the bytes that remain beside it after the build.
export def verify-keel-native [
    host: string
    target: path
    stage: path
    --without-keel
] {
    let raw = $target | path join $host "debug/examples"
    let observation = open ($stage | path join "development-observation.json")
    let css = $stage | path join "bin/backend-stylo-worker"
    let sql = $stage | path join "bin/backend-sqlite-worker"
    let lib = $stage | path join "lib/libgcc_s.so.1"
    let css_observer = $stage | path join "observers/stylo_identity_observer"
    let sql_observer = $stage | path join "observers/installed_basis_observer"
    if (checked "git" "rev-parse" "HEAD") != $observation.source_revision or (checked "git" "status" "--porcelain") != $observation.source_status or (checked "git" "diff" "--binary" "HEAD" | hash sha256) != $observation.tracked_diff_sha256 {
        error make {msg: "source checkout changed during native owner build"}
    }
    if (digest $css) != $observation.stylo_worker_sha256 or (digest $sql) != $observation.sqlite_worker_sha256 or (digest $lib) != $observation.libgcc_sha256 or (digest $css_observer) != $observation.stylo_observer_sha256 or (digest $sql_observer) != $observation.sqlite_observer_sha256 or (digest "Cargo.lock") != $observation.workspace_lock_sha256 or (digest "frontends/sqlite/Cargo.lock") != $observation.sqlite_lock_sha256 {
        error make {msg: "native worker, observer or lock changed during owner build"}
    }
    if (checked $css_observer $css $lib | from json) != $observation.stylo or (checked $sql_observer $sql $lib | from json) != $observation.sqlite {
        error make {msg: "relocated native worker identity changed during owner build"}
    }
    let apps = [
        "backend-locald"
        "backend-cli"
        "backend-mcp"
        "backend-desktop"
        "backend-metrics"
    ] | append (if $without_keel { [] } else { ["backend-keel-harness"] })
    let outputs = ($apps | each {|name|
        {name: $name, sha256: (digest ($stage | path join "bin" $name))}
    })
    $observation | upsert product_outputs $outputs | to json | save --force ($stage | path join "development-observation.json")
}
