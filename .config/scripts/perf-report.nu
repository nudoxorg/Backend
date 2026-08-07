#!/usr/bin/env nu

# perf-report.nu — turn `cost case=` lines into a report you can iterate against.
#
# Doctrine §4 says every integration test is also a benchmark: each one wraps its
# measured region in `nudox_test_support::measured()`, which prints one line:
#
#   cost case=<name> wall_ms=<f64> rss_bytes=<u64|unknown> disk_delta_bytes=<i64>
#
# The screenshot suite prints a richer variant of the same line:
#
#   cost case=shot/<corpus>/<slug> wall_ms=… px=WxH colours=N changed=N -> <path>
#
# Until now nothing collected them, so the numbers scrolled past and were lost.
# This script parses them out of captured test output and emits both a markdown
# table (for humans deciding what to optimise) and JSON (for comparing two runs
# and proving a change actually helped).
#
# Usage:
#   # capture some output, then report on it
#   RUSTC_BOOTSTRAP=1 cargo test ... --nocapture out> /tmp/run.txt
#   nu .config/scripts/perf-report.nu /tmp/run.txt
#
#   # or pipe directly — `--stdin` is LOAD-BEARING here, not optional
#   # decoration: this script is invoked as `nu <path>`, a separate process
#   # from whatever produced the test output, and by default nushell does
#   # NOT wire an external pipe into a script's own `$in`/`main` — confirmed
#   # on this host (nushell 0.114.1): `echo hi | nu script.nu` reads `$in` as
#   # empty inside `main`, and this script then dies with "Input type not
#   # supported ... input type: nothing" at the `open --raw`/`$in` branch
#   # below. `nu --stdin <path>` is the flag that actually relays the pipe
#   # (`nu --help`: "redirect standard input to a command (with -c) or a
#   # script file"). Both forms below were verified against this nextest
#   # track's `nextest-suite.nu`, which uses the `--stdin` form:
#   cargo nextest run -P perf ... | nu --stdin .config/scripts/perf-report.nu
#   cargo test ... --nocapture | nu --stdin .config/scripts/perf-report.nu
#
#   # compare against a previous run's JSON to see what moved
#   nu .config/scripts/perf-report.nu /tmp/run.txt --baseline perf-baseline.json
#
# Flags:
#   --json <path>      also write the parsed rows as JSON (default: perf-latest.json)
#   --baseline <path>  compare against a previous JSON and add a delta column
#   --markdown <path>  write the table to a file as well as stdout

# Parse one `cost case=` line into a record, or null if it is not one.
def parse-cost-line [line: string] {
    if not ($line | str contains "cost case=") { return null }

    # Fields are `key=value` separated by whitespace. Take everything from
    # `cost ` onward so surrounding test-harness noise on the same line is
    # ignored rather than corrupting the parse.
    let body = ($line | split row "cost " | last)
    let pairs = ($body
        | split row " "
        | where { |it| ($it | str contains "=") }
        | reduce --fold {} { |it, acc|
            let kv = ($it | split row "=")
            if (($kv | length) < 2) { $acc } else {
                $acc | upsert ($kv | first) ($kv | skip 1 | str join "=")
            }
        })

    if not ("case" in ($pairs | columns)) { return null }

    # `rss_bytes` is literally "unknown" on platforms with no supported counter —
    # that is a deliberate choice in nudox-test-support (a missing platform
    # counter must not turn a functional test into a platform test), so it must
    # not become 0 here either. 0 would silently average into the totals and
    # understate real memory use.
    let rss = (if ("rss_bytes" in ($pairs | columns)) {
        if ($pairs.rss_bytes == "unknown") { null } else { ($pairs.rss_bytes | into int) }
    } else { null })

    {
        case: $pairs.case
        wall_ms: (if ("wall_ms" in ($pairs | columns)) { $pairs.wall_ms | into float } else { 0.0 })
        rss_bytes: $rss
        disk_delta_bytes: (if ("disk_delta_bytes" in ($pairs | columns)) { $pairs.disk_delta_bytes | into int } else { 0 })
        kind: (if ($pairs.case | str starts-with "shot/") { "shot" } else { "lower" })
    }
}

def fmt-mb [bytes: any] {
    if ($bytes == null) { "—" } else { ($bytes / 1048576 | math round --precision 1 | into string) }
}

export def main [
    source?: path                       # file of captured test output; omit to read stdin
    --json: path = "perf-latest.json"   # where to write parsed rows
    --baseline: path                    # a previous --json file to diff against
    --markdown: path                    # also write the table here
] {
    let raw = (if ($source == null) { $in } else { open --raw $source })
    let rows = ($raw
        | lines
        | each { |l| parse-cost-line $l }
        | compact
        | sort-by wall_ms --reverse)

    if ($rows | is-empty) {
        print "no `cost case=` lines found."
        print ""
        print "Every integration test should emit one (doctrine §4). If a suite you"
        print "just ran produced none, either it has no measured region or you"
        print "forgot --nocapture — cargo swallows stdout for passing tests by default."
        return
    }

    # Attach deltas when a baseline is supplied. A missing case is reported as
    # NEW rather than skipped: a benchmark that silently stops running is the
    # failure mode a perf report exists to catch.
    let table = (if ($baseline == null) { $rows } else {
        let base = (open $baseline | reduce --fold {} { |it, acc| $acc | upsert $it.case $it.wall_ms })
        $rows | each { |r|
            let prev = (if ($r.case in ($base | columns)) { ($base | get $r.case) } else { null })
            $r | upsert delta_pct (if ($prev == null or $prev == 0.0) { null } else {
                (($r.wall_ms - $prev) / $prev * 100.0 | math round --precision 1)
            })
        }
    })

    let lowers = ($table | where kind == "lower")
    let shots  = ($table | where kind == "shot")

    mut md = "# Performance report\n\n"
    $md = $md + $"Rows: ($table | length) — ($lowers | length) lowering, ($shots | length) frame captures.\n\n"

    if not ($lowers | is-empty) {
        let total = ($lowers | get wall_ms | math sum)
        $md = $md + $"## Package lowering\n\nTotal wall: ($total / 1000 | math round --precision 1) s\n\n"
        $md = $md + "| case | wall (s) | peak RSS (MB) | disk Δ |"
        $md = $md + (if ($baseline == null) { "\n|---|---:|---:|---:|\n" } else { " Δ% |\n|---|---:|---:|---:|---:|\n" })
        for r in $lowers {
            let d = (if ($baseline == null) { "" } else {
                $" ($r.delta_pct | default "NEW") |"
            })
            $md = $md + $"| `($r.case)` | ($r.wall_ms / 1000 | math round --precision 1) | (fmt-mb $r.rss_bytes) | ($r.disk_delta_bytes) |($d)\n"
        }
        $md = $md + "\n"
    }

    if not ($shots | is-empty) {
        let total = ($shots | get wall_ms | math sum)
        $md = $md + $"## Frame captures\n\nTotal wall: ($total / 1000 | math round --precision 1) s across ($shots | length) frames.\n\n"
        $md = $md + "| frame | capture (ms) |\n|---|---:|\n"
        for r in $shots {
            $md = $md + $"| `($r.case)` | ($r.wall_ms | math round --precision 1) |\n"
        }
        $md = $md + "\n"
    }

    print $md
    $table | to json | save --force $json
    print $"parsed rows -> ($json)"
    if ($markdown != null) {
        $md | save --force $markdown
        print $"report -> ($markdown)"
    }
}
