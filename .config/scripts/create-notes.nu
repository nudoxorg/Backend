#!/usr/bin/env nu
use common.nu *

use std/log

def main [raw_tag: string, outfile: string, changelog: string] {
    let tag_v = $raw_tag
    let tag = ($tag_v | str replace --regex '^v' '')  # Remove prefix v
    let changelog_file = $changelog
    
    try {
        # Verify changelog exists
        if not ($changelog_file | path exists) {
            build_error $"($changelog_file) not found."
        }

        log info $"Extracting notes for tag: ($tag_v) \(searching for section [($tag)]\)"

        # Write header to output file
        "# What's new\n" | save --force $outfile

        # Read and process changelog
        let content = (open $changelog_file | lines)

        # Find the start of the target section
        let start_idx = ($content | enumerate | where ($it.item | str contains $tag) | get index | first)

        if ($start_idx | is-empty) {
            build_error $"Could not find tag ($tag) in ($changelog_file)"
        }

        # Find the end of the target section (next ## [ header)
        let remaining_lines = ($content | skip ($start_idx + 1))
        let next_section_idx = ($remaining_lines | enumerate | where item =~ '^## \[' | get index | first)

        let section_lines = if ($next_section_idx | is-empty) {
            $remaining_lines
        } else {
            $remaining_lines | take $next_section_idx
        }

        # Append section content to output file
        $section_lines | str join (char newline) | save --append $outfile

        # Check if output file has meaningful content
        let output_size = (open $outfile | str length)
        if $output_size > 20 {  # More than just the header
            log info $"Successfully extracted release notes to '($outfile)'."
        } else {
            log warning $"Warning: '($outfile)' appears empty. Is '($tag)' present in '($changelog_file)'?"
        }

    } catch { |e| 
        build_error $"Failed to extract release notes:" $e
    }
}
