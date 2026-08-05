#!/usr/bin/env nu
# Fetch and materialize the corpus from manifest.toml
# This is a fallback for environments without Nix.
# Usage: nu corpus/fetch.nu [--output-dir <path>]

def main [--output-dir: path = ".real-crates"] {
    let manifest_path = "corpus/manifest.toml"

    # Check that we're in the repo root
    if not ($manifest_path | path exists) {
        print $"ERROR: manifest.toml not found; run from repo root"
        exit 1
    }

    # Create output directory
    mkdir $output_dir

    # Parse the manifest
    let manifest = open $manifest_path

    print $"INFO: Materializing corpus into '($output_dir)'"

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

            print $"INFO: Fetching ($name) ($version)"

            # Create temp directory for download
            let temp_dir = (mktemp -d)

            try {
                # Download the archive
                let archive_path = $"($temp_dir)/($name)-($version).crate"
                curl -sS -L -o $archive_path $archive_url

                # Verify the file was downloaded
                if not ($archive_path | path exists) {
                    print $"ERROR: Failed to download ($name)-($version)"
                    rm -rf $temp_dir
                    continue
                }

                # Extract to temp (tar.gz format, despite .crate extension)
                cd $temp_dir
                tar -xzf $archive_path

                # Move extracted directory to output
                if ($"($name)-($version)" | path exists) {
                    mkdir -p ($pkg_dir | path dirname)
                    mv $"($name)-($version)" $pkg_dir
                } else {
                    print $"ERROR: Extracted archive structure unexpected for ($name)-($version)"
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
                        print $"INFO: Updated ($pkg_dir)/Cargo.toml with [workspace] table"
                    } catch {
                        print $"WARN: Could not append [workspace] to ($cargo_toml)"
                    }
                }

                print $"OK: ($name)-($version) ready"
            } catch { |err|
                print $"ERROR: Failed to process ($name)-($version): ($err)"
            } finally {
                # Cleanup temp directory
                rm -rf $temp_dir
            }
        }
    }

    print $"INFO: Corpus materialized to ($output_dir)"
}
