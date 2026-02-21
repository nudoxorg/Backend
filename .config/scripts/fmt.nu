#!/usr/bin/env nu
use common.nu *

def main [] {
    log "💅" "Formatting"

		# Format all in the workspace
    cargo fmt

		# Format all found nix files
		nixfmt ...(glob **/*.nix)

		# Format TOML
		tombi format

		# Format all Markdown files
		hongdown --write ...(glob **/*.md)
}
