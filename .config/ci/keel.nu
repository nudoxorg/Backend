# Run the declarative product histories against already-built artifacts. Building
# the product here would substitute new binaries for the ones the caller admitted.
use lib.nu run-bounded

def main [
    --bin-dir: path
    --runner: path
    --evidence: path
    --order: path
    --case: string
    --list
] {
    let root = $env.FILE_PWD | path dirname | path dirname
    let executable = if $runner != null {
        $runner | path expand
    } else if ($env.BACKEND_KEEL_HARNESS? | default "") != "" {
        $env.BACKEND_KEEL_HARNESS | path expand
    } else if $bin_dir != null {
        $bin_dir | path expand | path join "backend-keel-harness"
    } else {
        error make {msg: "Keel needs --runner, BACKEND_KEEL_HARNESS, or --bin-dir containing backend-keel-harness"}
    }
    if not ($executable | path exists) {
        error make {msg: $"Keel runner is absent: ($executable); enter the pinned compiler shell or pass --runner"}
    }
    if not $list and ($bin_dir == null or $evidence == null) {
        error make {msg: "Keel execution needs --bin-dir and a new --evidence directory"}
    }
    if $evidence != null and ($evidence | path exists) {
        error make {msg: "Keel evidence destination already exists; choose a new directory"}
    }
    let model = $root | path join "tests/keel/default.nix"
    let evaluated = (^nix-instantiate --eval --strict --json $model | complete)
    if $evaluated.exit_code != 0 {
        error make {msg: $"Keel case definitions did not evaluate: ($evaluated.stderr)"}
    }
    # Parse before touching the evidence destination. Invalid/empty definitions
    # cannot become a successful empty run through a shell pipeline.
    let definition = $evaluated.stdout | from json
    if $definition.schema != 1 or ($definition.cases | is-empty) {
        error make {msg: "Keel case definitions need schema 1 and a nonempty case inventory"}
    }
    let temporary = (^mktemp -d | str trim)
    let manifest = $temporary | path join "manifest.json"
    $evaluated.stdout | save $manifest
    let outcome = try {
        let selected = if $case == null { [] } else { ["--case" $case] }
        let ordering = if $order == null { [] } else { ["--order" ($order | path expand)] }
        if $list {
            run-bounded 1min $executable "--manifest" $manifest "--list" ...$selected ...$ordering
        } else {
            run-bounded 30min $executable "--manifest" $manifest "--bin-dir" ($bin_dir | path expand) "--evidence" ($evidence | path expand) ...$selected ...$ordering
        }
        null
    } catch {|failure| $failure }
    # Keep the exact evaluated input beside the raw process evidence, including
    # failed runs, so a later model edit cannot change what this run meant.
    if not $list and ($evidence | path exists) {
        cp $manifest ($evidence | path join "manifest.json")
    }
    rm --recursive $temporary
    if $outcome != null { error make {msg: $outcome.msg} }
}
