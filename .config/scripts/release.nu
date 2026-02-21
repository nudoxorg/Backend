#!/usr/bin/env nu
use common.nu *

def main [] {
    nu .config/scripts/build-release.nu
    nu .config/scripts/checksum.nu $env.OUTPUT_DIRECTORY
}
