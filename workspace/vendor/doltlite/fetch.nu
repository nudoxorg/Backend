#!/usr/bin/env nu
# Restore the vendored DoltLite amalgamation from its pinned upstream release.
#
# Usage:
#   nu workspace/vendor/doltlite/fetch.nu              # fetch + verify + extract
#   nu workspace/vendor/doltlite/fetch.nu --check      # verify what is on disk, fetch nothing
#   nu workspace/vendor/doltlite/fetch.nu --print-hashes <zip>   # for upgrading the pin
#
# Why this exists at all: `doltlite.c` is ~12 MB of generated C and is
# `.gitignore`d, so every fresh checkout starts without an engine. Before this
# script, the documented recovery was a 380 MB `git clone` plus a TCL/autosetup
# build — enough friction that in practice nobody ran it, and the tree sat with no
# engine while `index`'s "sovereign versioned catalog" quietly ran stock SQLite
# instead (see workspace/vendor/README.md, "The doltlite trap"). Upstream
# publishes the generated amalgamation as a release asset, so the honest recovery
# is one pinned download.
#
# Same rule as nix build .#checks.corpus: every byte is verified against manifest.toml
# BEFORE anything is written into this directory. A hash mismatch is a hard
# failure and nothing is extracted — a fetcher that silently accepts a mismatch
# turns a real failure into an apparent success, which is the exact failure class
# this whole directory exists to prevent.

def manifest-path [] {
    ($env.FILE_PWD | path join "manifest.toml")
}

def load-manifest [] {
    let path = (manifest-path)
    if not ($path | path exists) {
        error make { msg: $"DoltLite pin manifest is missing: ($path)" }
    }
    open $path
}

# sha256 of a file as lowercase hex, matching `shasum -a 256`.
def sha256-hex [file: path] {
    open --raw $file | hash sha256
}

# Verify one extracted file against its manifest entry. Returns a record so the
# caller can report every problem at once rather than one per run.
def check-file [directory: path, entry: record] {
    let target = ($directory | path join $entry.name)
    if not ($target | path exists) {
        return { name: $entry.name, ok: false, detail: "absent" }
    }
    let size = (ls -l $target | get 0.size | into int)
    if $size != $entry.bytes {
        return { name: $entry.name, ok: false, detail: $"size ($size) != pinned ($entry.bytes)" }
    }
    let actual = (sha256-hex $target)
    if $actual != $entry.sha256 {
        return { name: $entry.name, ok: false, detail: $"sha256 ($actual) != pinned ($entry.sha256)" }
    }
    { name: $entry.name, ok: true, detail: "verified" }
}

def report [results: list<record>] {
    for row in $results {
        let mark = if $row.ok { "ok  " } else { "FAIL" }
        print $"  ($mark) ($row.name): ($row.detail)"
    }
    let bad = ($results | where not ok)
    if ($bad | length) > 0 {
        error make { msg: $"($bad | length) of ($results | length) vendored DoltLite files did not match manifest.toml" }
    }
}

# Verify the files already on disk. This is what CI (or a suspicious human) runs;
# it never touches the network.
def "main check" [] {
    let manifest = (load-manifest)
    let directory = $env.FILE_PWD
    print $"Checking vendored DoltLite ($manifest.engine.tag) in ($directory)"
    report ($manifest.files | each { |entry| check-file $directory $entry })
    print "All vendored DoltLite files match the pin."
}

# Print the hashes of a candidate archive so a new version can be pinned. Used
# when bumping `[engine] tag` — see the header of manifest.toml.
def "main print-hashes" [archive: path] {
    print $"[archive] sha256 = \"(sha256-hex $archive)\""
    print $"[archive] bytes = (ls -l $archive | get 0.size | into int)"
    let staging = (mktemp -d)
    ^unzip -q -o $archive -d $staging
    for file in (glob $"($staging)/**/*.{c,h}") {
        let name = ($file | path basename)
        print $"[[files]] name = \"($name)\" sha256 = \"(sha256-hex $file)\" bytes = (ls -l $file | get 0.size | into int)"
    }
    rm -rf $staging
}

def main [] {
    let manifest = (load-manifest)
    let directory = $env.FILE_PWD

    # Already correct? Then this is a no-op; re-downloading 3 MB to reproduce
    # bytes we can prove we already have is pure cost.
    let existing = ($manifest.files | each { |entry| check-file $directory $entry })
    if ($existing | all { |row| $row.ok }) {
        print $"Vendored DoltLite ($manifest.engine.tag) already matches the pin; nothing to do."
        return
    }

    print $"Fetching DoltLite ($manifest.engine.tag) \(commit ($manifest.engine.commit)\)"
    print $"  ($manifest.archive.url)"

    let staging = (mktemp -d)
    let archive = ($staging | path join $manifest.archive.asset)
    ^curl --silent --show-error --location --fail --output $archive $manifest.archive.url

    let archive_hash = (sha256-hex $archive)
    if $archive_hash != $manifest.archive.sha256 {
        rm -rf $staging
        error make { msg: $"DoltLite archive hash mismatch.\n  expected ($manifest.archive.sha256)\n  actual   ($archive_hash)\nNothing was extracted. Either the pin in manifest.toml is stale or the download was tampered with; do not 'fix' this by updating the hash without establishing which." }
    }
    print $"  archive sha256 verified: ($archive_hash)"

    # Extract into staging, verify each file there, and only then move it into
    # place. Verifying after installing would leave a bad engine on disk for the
    # next build to pick up.
    ^unzip -q -o $archive -d $staging
    let extracted = ($staging | path join ($manifest.archive.strip_prefix | str trim --right --char "/"))

    let staged = ($manifest.files | each { |entry| check-file $extracted $entry })
    if not ($staged | all { |row| $row.ok }) {
        report $staged
        rm -rf $staging
        return
    }

    for entry in $manifest.files {
        cp ($extracted | path join $entry.name) ($directory | path join $entry.name)
    }
    rm -rf $staging

    report ($manifest.files | each { |entry| check-file $directory $entry })
    print $"Vendored DoltLite ($manifest.engine.tag) restored. Rebuild to compile it in:"
    print "  RUSTC_BOOTSTRAP=1 cargo test -p index --features dolt-engine"
}
