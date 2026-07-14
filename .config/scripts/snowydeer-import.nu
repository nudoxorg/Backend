#!/usr/bin/env nu

use std/log

def main [target?: string] {
    if $target == null {
        print -e "Usage: snowydeer-import <buck2-target>"
        print -e "Example: snowydeer-import //workspace/compiler:compiler-daemon"
        exit 1
    }
    log info $"Importing ($target) via snowydeer..."
    ^buck2 bxl //snowydeer:snowydeer.bxl:main -- --target $target
}
