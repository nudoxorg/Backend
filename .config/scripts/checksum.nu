#!/usr/bin/env nu
use common.nu *

def main [directory: string] {
    let dir = $directory
    print $"🔒 Generating checksums in '($dir)'…"

    # Validate directory exists
    if not ($dir | path exists) {
        build_error $"'($dir)' is not a directory."
    }

    try {
        cd $dir

        # Remove existing checksum files
        try {
            glob '*.sum' | each { |file| rm $file }
        } catch {
            # Ignore errors if no .sum files exist
        }

        # Get all files except checksum files
        let files = ls | where type == file | where name !~ '\.sum$' | get name

        if (($files | length) == 0) {
            print --stderr "Warning: No files found to checksum"
            return
        }

        # Generate SHA256 checksums
        try {
            let sha256_results = $files | each { |file| 
                let hash = (open --raw $file | hash sha256)
                $"($hash)  ./($file | path basename)"
            }
            $sha256_results | str join (char newline) | save SHA256.sum
        } catch {|e| 
            build_error $"Failed to generate SHA256 checksums" $e
        }

        # Generate MD5 checksums
        try {
            let md5_results = $files | each { |file| 
                let hash = (open --raw $file | hash md5)
                $"($hash)  ./($file | path basename)"
            }
            $md5_results | str join (char newline) | save MD5.sum
        } catch {|e| 
            build_error $"Failed to generate MD5 checksums" $e
        }

        # Generate BLAKE3 checksums (using b3sum command)
        try {
            let b3_results = $files | each { |file| 
                let result = (run-external 'b3sum' $file | complete)
                if $result.exit_code != 0 {
                    build_error $"b3sum failed for ($file): ($result.stderr)"
                }
                let hash = ($result.stdout | str trim | split row ' ' | get 0)
                $"($hash)  ./($file | path basename)"
            }
            $b3_results | str join (char newline) | save BLAKE3.sum
        } catch {|e| 
            build_error $"Failed to generate BLAKE3 checksums" $e
        }

        print $"✅ Checksums created in '($dir)'"

    } catch {|e| 
        build_error $"Checksum generation failed" $e
    }
}
