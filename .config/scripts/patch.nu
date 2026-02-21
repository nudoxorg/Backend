#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
	# Hardcoded, could cause issues — really should check Radicle's idea of the main branch
	# TODO: Check active patches to signal if this is new or old to user
	if (git rev-parse --abbrev-ref HEAD) != "main" {
		git push rad HEAD:refs/patches
	} else {
    build_error "You're already on the canonical branch, move changes to an alternative branch"
	}
}
