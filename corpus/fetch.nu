#!/usr/bin/env nu
# Fetch and materialize the corpus from manifest.toml
# This is a fallback for environments without Nix.
# Usage: corpus/fetch.nu [--output-dir <path>]

use std log

def main [--output-dir: path = ".real-crates"] {
    let manifest_path = "corpus/manifest.toml"

    # Check that we're in the repo root
    if not ($manifest_path | path exists) {
        log error "manifest.toml not found; run from repo root"
        exit 1
    }

    # Create output directory
    mkdir $output_dir

    # Parse the manifest
    let manifest = open $manifest_path

    log info $"Materializing corpus into '($output_dir)'"

    # Process each package entry
    for pkg_entry in $manifest.packages {
        let ecosystem = $pkg_entry.ecosystem
        let name = $pkg_entry.name

        # Process each version
        for ver_entry in $pkg_entry.versions {
            let version = $ver_entry.version
            let hash = $ver_entry.hash

            let pkg_dir = $"($output_dir)/($name)-($version)"
            let archive_url = $"https://static.crates.io/crates/($name)/($name)-($version).crate"

            log info $"Fetching ($name) ($version) from ($archive_url)"

            # Create temp directory for download
            let temp_dir = (mktemp -d)

            try {
                # Download the archive
                let archive_path = $"($temp_dir)/($name)-($version).crate"
                curl -sS -L -o $archive_path $archive_url

                # Verify the file was downloaded
                if not ($archive_path | path exists) {
                    log error $"Failed to download ($name)-($version)"
                    rm -rf $temp_dir
                    continue
                }

                # Verify hash using b3sum (if available, otherwise skip)
                let expected_hash = $hash
                if (which b3sum | is-empty) {
                    log warn "b3sum not available; skipping hash verification"
                } else {
                    let actual_hash = (b3sum $archive_path | split column ' ' | get column1.0)
                    if $actual_hash != $expected_hash {
                        log error $"Hash mismatch for ($name)-($version): expected ($expected_hash), got ($actual_hash)"
                        rm -rf $temp_dir
                        continue
                    }
                }

                # Extract to temp (tar.gz format, despite .crate extension)
                cd $temp_dir
                tar -xzf $archive_path

                # Move extracted directory to output
                if ($"($name)-($version)" | path exists) {
                    mkdir -p ($pkg_dir | path dirname)
                    mv $"($name)-($version)" $pkg_dir
                } else {
                    log error $"Extracted archive structure unexpected for ($name)-($version)"
                    cd -
                    rm -rf $temp_dir
                    continue
                }

                cd -

                # Append [workspace] table to Cargo.toml
                let cargo_toml = $"($pkg_dir)/Cargo.toml"
                if ($cargo_toml | path exists) {
                    try {
                        echo "\n[workspace]" | append $cargo_toml
                        log info $"Updated ($pkg_dir)/Cargo.toml with [workspace] table"
                    } catch {
                        log warn $"Could not append [workspace] to ($cargo_toml)"
                    }
                }

                log info $"✓ ($name)-($version) ready"
            } catch { |err|
                log error $"Failed to process ($name)-($version): ($err)"
            } finally {
                # Cleanup temp directory
                rm -rf $temp_dir
            }
        }
    }

    log info $"Corpus materialized to ($output_dir)"
}
