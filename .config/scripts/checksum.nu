#!/usr/bin/env nu
use common.nu *

use std/log

def main [directory: string] {
    let dir = $directory
    log info $"🔒 Generating checksums in '($dir)'…"

    # Validate directory exists
    if not ($dir | path exists) {
        log error $"'($dir)' is not a directory."
        build_error $"'($dir)' is not a directory."
    }

    try {
        log debug $"Changing directory to ($dir)"
        cd $dir

        # Remove existing checksum files
        try {
            log debug "Removing existing .sum files"
            glob '*.sum' | each { |file| 
                log debug $"Removing ($file)"
                rm $file 
            }
        } catch {
            # Ignore errors if no .sum files exist
            log debug "No .sum files found to remove or error removing them"
        }

        # Get all files except checksum files
        let files = ls | where type == file | where name !~ '\.sum$' | get name
        log debug $"Found ($files | length) files to checksum: ($files | str join ', ')"

        if (($files | length) == 0) {
            log warning "No files found to checksum"
            return
        }

        # Generate SHA256 checksums
        try {
            log debug "Generating SHA256 checksums..."
            let sha256_results = $files | each { |file| 
                log debug $"SHA256 hashing ($file)..."
                let hash = (open --raw $file | hash sha256)
                $"($hash)  ./($file | path basename)"
            }
            $sha256_results | str join (char newline) | save SHA256.sum
            log debug "Saved SHA256.sum"
        } catch {|e| 
            build_error $"Failed to generate SHA256 checksums" $e
        }

        # Generate MD5 checksums
        try {
            log debug "Generating MD5 checksums..."
            let md5_results = $files | each { |file| 
                log debug $"MD5 hashing ($file)..."
                let hash = (open --raw $file | hash md5)
                $"($hash)  ./($file | path basename)"
            }
            $md5_results | str join (char newline) | save MD5.sum
            log debug "Saved MD5.sum"
        } catch {|e| 
            build_error $"Failed to generate MD5 checksums" $e
        }

        # Generate BLAKE3 checksums (using b3sum command)
        try {
            log debug "Generating BLAKE3 checksums..."
            let b3_results = $files | each { |file| 
                log debug $"BLAKE3 hashing ($file)..."
                let result = (run-external 'b3sum' $file | complete)
                if $result.exit_code != 0 {
                    build_error $"b3sum failed for ($file): ($result.stderr)"
                }
                let hash = ($result.stdout | str trim | split row ' ' | get 0)
                $"($hash)  ./($file | path basename)"
            }
            $b3_results | str join (char newline) | save BLAKE3.sum
            log debug "Saved BLAKE3.sum"
        } catch {|e| 
            build_error $"Failed to generate BLAKE3 checksums" $e
        }

        log info $"✅ Checksums created in '($dir)'"

    } catch {|e|
        build_error $"Checksum generation failed" $e
    }
}
