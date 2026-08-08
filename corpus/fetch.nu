#!/usr/bin/env nu
# Fetch and materialize the corpus from manifest.toml
# This is a fallback for environments without Nix (see flake.nix's `buildCorpus`
# for the Nix-native fixed-output-derivation path, which is crates.io-only).
#
# Usage:
#   nu corpus/fetch.nu [--output-dir <path>]
#   nu corpus/fetch.nu hash-url <ecosystem> <name> <version> [--url <override>]
#
# `hash-url` downloads a candidate package *without* requiring it to already be
# in the manifest, and prints the hash to paste in. See corpus/README.md.
#
# Every download is verified against its manifest `hash` (a nix-base32-encoded
# sha256, computed with a pure-Nushell reimplementation of Nix's base32 codec —
# no `nix` binary required). A mismatch is a hard failure: the package is never
# extracted into the output directory, and the whole run exits non-zero. A
# fetcher that silently accepts a hash mismatch converts a real failure into an
# apparent success, which is worse than not fetching at all.

# ---------------------------------------------------------------------------
# Nix base32 (the encoding Nix/`nix-prefetch-url` uses for fixed-output-derivation
# hashes, and the format every hash in manifest.toml is written in). This is a
# bespoke base32 variant (alphabet "0123456789abcdfghijklmnpqrsvwxyz", no e/o/t/u,
# and bits packed least-significant-group-first) — NOT RFC 4648 base32, so no
# stock `encode base32` command applies. See Nix's `libutil/hash.cc:printHash32`.
# ---------------------------------------------------------------------------

def hex-to-bytes [hex: string] {
    $hex | split chars | chunks 2 | each { |pair| ("0x" + ($pair | str join)) | into int }
}

def nix-base32-encode [bytes: list<int>] {
    let chars = ("0123456789abcdfghijklmnpqrsvwxyz" | split chars)
    let hash_size = ($bytes | length)
    let len = (($hash_size * 8 - 1) // 5) + 1
    mut result = ""
    for n in (($len - 1)..0) {
        let b = $n * 5
        let i = $b // 8
        let j = $b mod 8
        let lo = (($bytes | get $i) | bits shr $j)
        let hi = if $i >= ($hash_size - 1) { 0 } else { (($bytes | get ($i + 1)) | bits shl (8 - $j)) }
        let c = (($lo | bits or $hi) | bits and 0x1f)
        $result = $result + ($chars | get $c)
    }
    $result
}

# The content hash of a downloaded archive, in the same format as manifest.toml.
def compute-nix-sha256 [file: path] {
    let hex = (open --raw $file | hash sha256)
    nix-base32-encode (hex-to-bytes $hex)
}

# ---------------------------------------------------------------------------
# Per-ecosystem URL resolution.
#
#   crates.io -> static.crates.io .crate (tar.gz)                    [source]
#   go        -> proxy.golang.org module zip                          [source]
#   maven     -> repo1.maven.org SOURCES jar, not the binary jar      [source]
#   nuget     -> nuget.org v3 flat-container .nupkg                   [binary; see README]
#   npm       -> registry.npmjs.org tarball                           [source]
#   pypi      -> pypi.org sdist, not a wheel                          [source]
#   cpp       -> explicit `url` per version; no ecosystem convention exists
#                (see README for why: GitHub's auto-generated tag tarballs are
#                not guaranteed byte-stable, so each cpp entry pins a real,
#                maintainer-uploaded URL instead of a computed one)
# ---------------------------------------------------------------------------

# Go module proxy path/version escaping: every uppercase letter becomes
# "!" + its lowercase form (golang.org/x/mod/module.EscapePath).
def go-escape-path [s: string] {
    $s | split chars | each { |c| if ($c =~ '[A-Z]') { $"!($c | str lowercase)" } else { $c } } | str join
}

def pypi-sdist-url [name: string, version: string] {
    let meta_url = $"https://pypi.org/pypi/($name)/($version)/json"
    let meta = (curl -sS -L -f --max-time 60 $meta_url | from json)
    let sdists = ($meta.urls | where packagetype == "sdist")
    if ($sdists | is-empty) {
        error make { msg: $"no sdist published for ($name) ($version) on PyPI" }
    }
    ($sdists | first).url
}

# Resolves the download URL for one package version. `override_url` (nullable)
# always wins — it is how `cpp` entries (and any other one-off exception) work.
def resolve-url [ecosystem: string, name: string, version: string, override_url] {
    if $override_url != null {
        return $override_url
    }
    match $ecosystem {
        "crates.io" => $"https://static.crates.io/crates/($name)/($name)-($version).crate"
        "go" => {
            let epath = (go-escape-path $name)
            let ever = (go-escape-path $version)
            $"https://proxy.golang.org/($epath)/@v/($ever).zip"
        }
        "maven" => {
            let parts = ($name | split row ":")
            if ($parts | length) != 2 {
                error make { msg: $"maven package name must be 'groupId:artifactId', got '($name)'" }
            }
            let group_path = ($parts | get 0 | str replace --all "." "/")
            let artifact = ($parts | get 1)
            $"https://repo1.maven.org/maven2/($group_path)/($artifact)/($version)/($artifact)-($version)-sources.jar"
        }
        "nuget" => {
            let id_lower = ($name | str lowercase)
            let ver_lower = ($version | str lowercase)
            $"https://api.nuget.org/v3-flatcontainer/($id_lower)/($ver_lower)/($id_lower).($ver_lower).nupkg"
        }
        "npm" => {
            let basename = if ($name | str starts-with "@") { ($name | split row "/" | get 1) } else { $name }
            $"https://registry.npmjs.org/($name)/-/($basename)-($version).tgz"
        }
        "pypi" => (pypi-sdist-url $name $version)
        "cpp" => {
            error make { msg: "cpp packages have no URL convention; the manifest entry must set an explicit `url` per version (see corpus/README.md)" }
        }
        _ => {
            error make { msg: $"unknown ecosystem '($ecosystem)'" }
        }
    }
}

def guess-extension [url: string] {
    if ($url | str ends-with ".tar.gz") { ".tar.gz" } else if ($url | str ends-with ".tgz") { ".tgz" } else if ($url | str ends-with ".zip") { ".zip" } else if ($url | str ends-with ".jar") { ".jar" } else if ($url | str ends-with ".nupkg") { ".nupkg" } else if ($url | str ends-with ".crate") { ".crate" } else { "" }
}

# Last path segment of a URL, used to name a raw (non-archive) download.
def url-basename [url: string] {
    $url | split row "/" | last
}

# zip-family archives (go module zips, maven/nuget jars) vs tar.gz-family
# (crates.io, npm, pypi sdists, cpp tarballs).
def is-zip-family [path: string] {
    ($path | str ends-with ".zip") or ($path | str ends-with ".jar") or ($path | str ends-with ".nupkg")
}

def extract-to [archive_path: path, dest_dir: path] {
    mkdir $dest_dir
    if (is-zip-family $archive_path) {
        unzip -q -o $archive_path -d $dest_dir
    } else {
        tar -xzf $archive_path -C $dest_dir
    }
}

# Descends through directories that contain nothing but a single subdirectory.
# Go module zips need this to be recursive, not one level: the whole archive
# is nested as `<module-path>@<version>/...`, and a module path itself has
# path separators (e.g. `github.com/pkg/errors@v0.9.1/errors.go` unpacks as
# github.com/ -> pkg/ -> errors@v0.9.1/ -> *.go), i.e. several directories in
# a row that each contain exactly one entry.
def deepest-sole-dir [dir: path] {
    let entries = (ls $dir)
    if (($entries | length) == 1) and ((($entries | first).type) == "dir") {
        deepest-sole-dir ($entries | first).name
    } else {
        $dir
    }
}

# Archives either wrap their contents in one or more nested single-child
# directories (crates.io, npm, pypi sdists, go module zips, cpp tarballs) or
# have none (maven sources jars and nuget nupkgs, which put files straight at
# the archive root). Handle both without hardcoding a name-version directory
# convention that not every ecosystem follows.
def finalize-package [extract_dir: path, pkg_dir: path] {
    let source = (deepest-sole-dir $extract_dir)
    mkdir ($pkg_dir | path dirname)
    mv $source $pkg_dir
}

# Filesystem-safe directory name: package names can contain '/' (go modules,
# scoped npm packages) or ':' (maven GAV coordinates), neither of which may
# appear as a single path component.
def safe-dir-name [name: string, version: string] {
    let clean = ($name | str replace --all "/" "__" | str replace --all ":" "__")
    $"($clean)-($version)"
}

# Appends an empty [workspace] table to a fixture's Cargo.toml so the repo's
# root Cargo workspace does not auto-promote it as a member (see
# AGENTS-DOCTRINE.md §8). Only crates.io fixtures ever have a Cargo.toml, so
# this is a no-op for every other ecosystem.
def append-cargo-workspace [pkg_dir: path] {
    let cargo_toml = $"($pkg_dir)/Cargo.toml"
    if ($cargo_toml | path exists) {
        try {
            # NOTE: nu's `append` command appends rows to a *list*, not bytes
            # to a *file* -- `... | append $path` silently builds a 2-element
            # list and prints it, touching nothing on disk. That was this
            # function's original implementation (inherited from before this
            # revision) and it never actually wrote the [workspace] table via
            # this script; every existing fixture that has one got it from
            # flake.nix's shell-based `echo ... >> Cargo.toml` instead. `save
            # --append` is the real file-append primitive.
            "\n[workspace]\n" | save --append $cargo_toml
        } catch {
            print $"WARN: could not append [workspace] to ($cargo_toml)"
        }
    }
}

# Fetches, verifies, and extracts one package version. Returns a result record
# instead of mutating shared state, so the whole run can stay a plain `each`
# (Nushell `catch` blocks cannot capture outer `mut` variables).
def fetch-one [ecosystem: string, name: string, ver_entry: record, output_dir: path] {
    let version = $ver_entry.version
    let hash = $ver_entry.hash
    let override_url = $ver_entry.url?
    let pkg_dir = $"($output_dir)/(safe-dir-name $name $version)"

    if ($pkg_dir | path exists) {
        print $"SKIP: ($ecosystem)/($name) ($version) already present"
        return { ecosystem: $ecosystem, name: $name, version: $version, status: "skipped", detail: "already present" }
    }

    print $"INFO: Fetching ($ecosystem)/($name) ($version)"
    let temp_dir = (mktemp -d)

    let result = (try {
        let url = (resolve-url $ecosystem $name $version $override_url)
        let archive_path = $"($temp_dir)/archive(guess-extension $url)"

        curl -sS -L -f --retry 2 --retry-delay 2 --max-time 180 -o $archive_path $url

        if not ($archive_path | path exists) {
            error make { msg: $"download produced no file (url: ($url))" }
        }

        let actual_hash = (compute-nix-sha256 $archive_path)
        if $actual_hash != $hash {
            error make { msg: $"hash mismatch: manifest says ($hash), downloaded content hashes to ($actual_hash) \(url: ($url)\)" }
        }

        # A handful of cpp release assets are a single raw header, not an
        # archive (e.g. simdjson's `simdjson.h`, Catch2's
        # `catch_amalgamated.hpp`). guess-extension returns "" for those;
        # place the file itself into pkg_dir instead of trying to unpack it.
        if (guess-extension $url) == "" {
            mkdir $pkg_dir
            cp $archive_path $"($pkg_dir)/(url-basename $url)"
        } else {
            let extract_dir = $"($temp_dir)/extracted"
            extract-to $archive_path $extract_dir
            finalize-package $extract_dir $pkg_dir
        }
        append-cargo-workspace $pkg_dir

        print $"OK: ($ecosystem)/($name)-($version) ready"
        { ecosystem: $ecosystem, name: $name, version: $version, status: "ok", detail: $pkg_dir }
    } catch { |err|
        let is_mismatch = ($err.msg | str starts-with "hash mismatch")
        let status = if $is_mismatch { "hash-mismatch" } else { "error" }
        print $"(if $is_mismatch { 'HASH MISMATCH' } else { 'ERROR' }): ($ecosystem)/($name) ($version): ($err.msg)"
        { ecosystem: $ecosystem, name: $name, version: $version, status: $status, detail: $err.msg }
    })

    rm -rf $temp_dir
    $result
}

def main [--output-dir: path = ".real-crates", --threads: int = 4] {
    let manifest_path = "corpus/manifest.toml"

    if not ($manifest_path | path exists) {
        print $"ERROR: manifest.toml not found; run from repo root"
        exit 1
    }

    mkdir $output_dir
    let manifest = open $manifest_path

    let all_versions = ($manifest.packages | each { |pkg_entry|
        $pkg_entry.versions | each { |ver_entry|
            { ecosystem: $pkg_entry.ecosystem, name: $pkg_entry.name, ver_entry: $ver_entry }
        }
    } | flatten)

    let total = ($all_versions | length)
    print $"INFO: Materializing corpus into '($output_dir)' -- ($total) package versions, ($threads) at a time"

    # Independent network fetches, each returning a result record rather than
    # touching shared state (see fetch-one's doc comment) -- safe to run
    # concurrently. Seven ecosystems' worth of registries means no single
    # host takes the full fan-out.
    let results = ($all_versions | par-each -t $threads { |v|
        fetch-one $v.ecosystem $v.name $v.ver_entry $output_dir
    })

    let ok = ($results | where status == "ok" | length)
    let skipped = ($results | where status == "skipped" | length)
    let failed = ($results | where { |r| $r.status == "error" or $r.status == "hash-mismatch" })

    print ""
    print $"INFO: ($ok) fetched, ($skipped) already present, ($failed | length) failed"

    if ($failed | length) > 0 {
        print "ERROR: the following packages failed and were NOT added to the corpus:"
        for r in $failed {
            print $"  [($r.status)] ($r.ecosystem)/($r.name)@($r.version): ($r.detail)"
        }
        exit 1
    }

    print $"INFO: Corpus materialized to ($output_dir)"
}

# Fetch a single candidate package and print its content hash, without
# requiring a manifest entry to already exist. Use this to onboard a new
# package: run it, paste the printed hash into manifest.toml.
#
#   nu corpus/fetch.nu hash-url crates.io regex 1.10.3
#   nu corpus/fetch.nu hash-url cpp nlohmann-json v3.11.3 --url https://github.com/nlohmann/json/releases/download/v3.11.3/include.zip
def "main hash-url" [ecosystem: string, name: string, version: string, --url: string] {
    let override_url = if ($url | is-empty) { null } else { $url }
    let resolved = (resolve-url $ecosystem $name $version $override_url)
    print $"INFO: resolved URL: ($resolved)"

    let temp_dir = (mktemp -d)
    let archive_path = $"($temp_dir)/archive(guess-extension $resolved)"
    curl -sS -L -f --retry 2 --retry-delay 2 --max-time 180 -o $archive_path $resolved

    if not ($archive_path | path exists) {
        rm -rf $temp_dir
        print $"ERROR: download produced no file"
        exit 1
    }

    let h = (compute-nix-sha256 $archive_path)
    rm -rf $temp_dir

    print ""
    print $"hash = ($h)"
    print ""
    print "Manifest snippet:"
    print $"  { version = \"($version)\", hash = \"($h)\" }"
}
