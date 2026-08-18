#!/usr/bin/env nu

# Bounded mutation gate for local/CI smoke runs.
# It intentionally targets one high-value engine file and one mutant at a time;
# the nightly full-check remains the place for the complete package sweep.

def main [
    --file (-f): string = "workspace/nudox-engine/src/search/hits.rs"
    --timeout (-t): int = 30
] {
    let tool = (do {
        run-external "cargo" "mutants" "--version"
    } | complete)
    if $tool.exit_code != 0 {
        print --stderr "FAIL: cargo-mutants is required for the mutation smoke gate"
        exit 127
    }

    let result = (do {
        run-external "cargo" "mutants" "--package" "nudox-engine" "--file" $file "--re" "collect_name_hits" "--timeout" ($timeout | into string) "--no-shuffle" "--in-place" "--" "--lib"
    } | complete)

    print $result.stdout
    if $result.exit_code != 0 {
        print --stderr $result.stderr
        exit $result.exit_code
    }
}
