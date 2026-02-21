#!/usr/bin/env nu

use std/log

export def build_error [msg: string, error?: record] {
    if ($error != null) {
        let annotated_error = ($error | upsert msg $'($msg): ($error.msg)')
        log error $annotated_error.rendered
        exit 1
    } else {
        log error $msg
        exit 1
    }
}

