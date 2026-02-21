#!/usr/bin/env nu

export def build_error [msg: string, error?: record] {
    if ($error != null) {
        let annotated_error = ($error | upsert msg $'($msg): ($error.msg)')
        $annotated_error.rendered | print --stderr
        exit 1
    } else {
        (error make --unspanned { msg: $msg }) | print --stderr
        exit 1
    }
}

export def log [icon: string, msg: string] {
    print $"($icon) ($msg)"
}
