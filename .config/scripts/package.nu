#!/usr/bin/env nu
use common.nu *

use std/log

def main [target: string] {
    log info "Packaging release binary…"

    let prime = $env.MAIN_PACKAGE
    let out = $env.OUTPUT_DIRECTORY
    let artifact_dir = $"target/($target)/release"
    log debug $"Package: ($prime), Output: ($out), Artifact Dir: ($artifact_dir)"

    try {
        # We can't use 'just' here anymore if we are replacing it.
        # The original script called 'just build-release $target'.
        # We should assume the build is done or call the build command.
        # But 'build-release' is now a devshell command.
        # If we run this script from 'package' command, maybe we should run build-release first in the command chain.
        # Or call 'cargo build --release' directly here?
        # The user wants "linked commands".
        # Let's call cargo directly to be safe and independent of alias availability in non-interactive shells.
        
        log info $"Building workspace (release) for ($target)…"
        cargo build --workspace --release --bin $prime --target $target

        log info $'Destination is ($out)'

        # Windows the only one that has an executable extension
        let ext = if ($target | str contains 'windows-msvc') { '.exe' } else { '' }
        log debug $"Target: ($target), Extension: ($ext)"

        # Example: package-triplet
        let qualified_name = $"($prime)-($target)"

        let bin_path = $'($artifact_dir)/($prime)($ext)' # Where rust puts the binary artifact
        let out_path = $'($out)/($qualified_name)($ext)'
        log debug $"Source binary: ($bin_path), Target binary: ($out_path)"

        # Create output directory structure
        try {
            log debug $"Creating directory: ($out)"
            mkdir $out
        } catch {|e| 
            build_error $"Failed to create directory: ($out)" $e
        }

        # Copy completion scripts
        let completions = [$'($prime).bash', $'($prime).elv', $'($prime).fish', $'_($prime).ps1', $'_($prime)']
        log debug $"Looking for completion scripts: ($completions)"

        for completion in $completions {
            let src = $'($artifact_dir)/($completion)'
            let dst = $'($out)/($completion)'

            if ($src | path exists) {
                try {
                    log debug $"Copying ($src) to ($dst)"
                    cp --force $src $dst # Using force here because default nu copy only works with existing files otherwise
                    log info $"('Successfully copied completion to destination:' | ansi gradient --fgstart '0x00ff00' --fgend '0xff0080' --bgstart '0x1a1a1a' --bgend '0x0d0d0d') (basename $src)"
                } catch {|e| 
                    build_error $"Failed to copy completion script ($src)" $e
                }
            } else {
                log warning $"Warning: completion script missing: ($src)"
            }
        }

        # Copy main binary
        try {
            log debug $"Copying binary ($bin_path) to ($out_path)"
            cp --force $bin_path $out_path
            log info $"('Successfully copied binary to destination:' | ansi gradient --fgstart '0x00ff00' --fgend '0xff0080' --bgstart '0x1a1a1a' --bgend '0x0d0d0d') (basename $bin_path)"
        } catch {  |e| 
            build_error $"Failed to copy binary ($bin_path)" $e
        }

    } catch {  |e| 
        build_error "Packaging failed" $e
    }
}
