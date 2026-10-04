# Tests Nix declarations, lint matrices, diagnostic counts, and role topology.
# Uses Nushell standard assertions over structured values instead of prose snapshots.
# Fails when any executable law lacks a distinct adversarial proof surface.

use std/assert

let config = $env.BACKEND_CONFIG_SNAPSHOT | path expand
let control = (open $env.BACKEND_CONTROL_PLANE)
let ast_root = $env.BACKEND_AST_GREP | path dirname
let rules = $control.lint.syntax | columns | sort

let tests = (
    glob ($ast_root | path join "tests/*.yml")
    | each {|path| $path | path parse | get stem }
    | sort
)

let snapshots = (
    glob ($ast_root | path join "metadata/*.json")
    | each {|path| $path | path parse | get stem }
    | sort
)

assert equal $rules $tests "every ast-grep rule must own a native test matrix"
assert equal $rules $snapshots "every ast-grep rule must own a frozen diagnostic snapshot"

for path in (glob ($ast_root | path join "tests/*.yml")) {
    let fixture = (open $path)
    assert greater or equal ($fixture.valid | length) 2 $"($fixture.id) needs at least two valid near-neighbors"
    assert greater or equal ($fixture.invalid | length) 2 $"($fixture.id) needs at least two invalid mutants"
}

let lint_rules = $control.lint.rules
let lint_ids = $lint_rules | get id
assert equal ($lint_ids | uniq | length) ($lint_ids | length) "lint identifiers must be unique"
for lint in $lint_rules {
    assert ($lint.engine in ["ast-grep" "dylint" "nushell"]) $"($lint.id) has an unknown engine"
    assert ($lint.severity in ["error" "warning"]) $"($lint.id) has an unknown severity"
    assert (not ($lint.category | is-empty)) $"($lint.id) needs a category"
    assert (not ($lint.scope | is-empty)) $"($lint.id) needs an honest scope"
    assert (not ($lint.proof | is-empty)) $"($lint.id) needs an executable proof contract"
}
let ast_policy = (
    $lint_rules
    | where engine == "ast-grep"
    | get id
    | sort
)
assert equal $rules $ast_policy "every ast-grep rule must have exactly one Nix policy owner"

let scratch = (mktemp --directory --tmpdir backend-ast-snapshots.XXXXXX)
for snapshot_path in (glob ($ast_root | path join "metadata/*.json") | sort) {
    let snapshot = (open $snapshot_path)
    for case in ($snapshot.cases | enumerate) {
        let directory = $scratch | path join $snapshot.id $"($case.index)"
        mkdir $directory
        let source = $directory | path join $snapshot.file
        $case.item.source | save --raw $source
        let result = (
            ^ast-grep scan --rule ($ast_root | path join "rules" $"($snapshot.id).yml") --json=stream $source
            | complete
        )
        let findings = (
            $result.stdout
            | lines
            | where {|line| not ($line | is-empty) }
            | each {|line| $line | from json }
        )
        assert equal ($findings | length) $case.item.count $"($snapshot.id) snapshot case ($case.index) diagnostic count changed"
        assert (
            $findings
            | all {|finding| ($finding.ruleId == $snapshot.id) and ($finding.severity == $snapshot.severity) }
        ) $"($snapshot.id) snapshot metadata changed"
    }
}

let count_expectations = (
    $control.lint.syntax
    | transpose rule specification
    | where {|row| "corpus" in ($row.specification | columns) }
)
for expectation in $count_expectations {
    let source = $scratch | path join $"($expectation.rule)-corpus.rs"
    $expectation.specification.corpus.source | save --raw $source
    let result = (
        ^ast-grep scan --rule ($ast_root | path join "rules" $"($expectation.rule).yml") --json=stream $source
        | complete
    )
    let findings = (
        $result.stdout
        | lines
        | where {|line| not ($line | is-empty) }
        | each {|line| $line | from json }
    )
    assert equal ($findings | length) $expectation.specification.corpus.count $"($expectation.rule) must report every corpus mutant"
}

let command_rows = (^backend catalog --json | from json)
let command_ids = $command_rows | get id
let command_names = $command_rows | get nu
let command_cli = $command_rows | get cli
assert equal ($command_ids | uniq | length) ($command_ids | length) "command identifiers must be unique"
assert equal ($command_names | uniq | length) ($command_names | length) "Nushell command names must be unique"
assert equal ($command_cli | uniq | length) ($command_cli | length) "public command spellings must be unique"
assert ($command_cli | all {|name| $name | str starts-with "backend " }) "every public command must use the backend namespace"

let roles = (
    $control.roles
    | transpose id contract
    | each {|row|
      $row.contract
      | merge {
          id: $row.id
          allowed: ($row.contract.tools | values | uniq)
        }
    }
)
for role in $roles {
    assert (not ($role.allowed | is-empty)) $"($role.id) needs executable capabilities"
    assert ($role.allowed | all {|id| $id in $command_ids }) $"($role.id) references only declared commands"
    assert equal ($role.allowed | uniq | length) ($role.allowed | length) $"($role.id) capabilities must be unique"
    assert (not ($role.entry | is-empty)) $"($role.id) needs entry conditions"
    assert (not ($role.exit | is-empty)) $"($role.id) needs an exit condition"
}
for id in ["luna-pair" "terra-academic"] {
    let role = $roles | where id == $id | first
    let classes = $command_rows | where {|row| $row.id in $role.allowed } | get class
    assert (
        not (
            $classes
            | any {|class| $class in ["measurement" "privileged-verification" "baseline-write" "closure"] }
        )
    ) $"($id) must not receive verifier privileges"
}
let luna = $roles | where id == "luna-pair" | first
let luna_classes = $command_rows | where {|row| $row.id in $luna.allowed } | get class | uniq
assert equal $luna_classes ["implementation-feedback"] "Luna may only receive opaque assignment feedback"
let reviewer = $roles | where id == "terra-reviewer" | first
let reviewer_classes = $command_rows | where {|row| $row.id in $reviewer.allowed } | get class
assert (
    not (
        $reviewer_classes
        | any {|class| $class in ["repository-write" "policy-write" "baseline-write" "closure"] }
    )
) "reviewer must remain read-only"

let authored_foreign_config = (
  (glob ($config | path join "**/*.nuon"))
  | append (glob ($config | path join "**/*.yml"))
  | append (glob ($config | path join "**/*.yaml"))
)
assert ($authored_foreign_config | is-empty) "NUON and YAML may exist only as Nix-generated foreign-tool artifacts"

let direnv_entrypoint = open --raw ($config | path join "direnv/entrypoint")
assert ($direnv_entrypoint | str contains "direnv_layout_dir()") "direnv must override its layout root"
assert ($direnv_entrypoint | str contains '$PWD/.local/direnv') "direnv state must live beneath .local"
assert (not ($direnv_entrypoint | str contains '$PWD/.direnv')) "direnv must never recreate a top-level cache"

let shell_declaration = open --raw ($config | path join "nix/shells.nix")
assert (not ($shell_declaration | str contains 'CARGO_TARGET_DIR = "$PWD')) "Nix attributes must not freeze a literal $PWD into Cargo paths"
assert (not ($shell_declaration | str contains 'CLIPPY_CONF_DIR = "$PWD')) "Nix attributes must not freeze a literal $PWD into Clippy paths"
assert ($shell_declaration | str contains 'export CARGO_TARGET_DIR="$PWD/.local/target"') "the shell hook must resolve Cargo state against the entered repository"
assert ($shell_declaration | str contains 'export CLIPPY_CONF_DIR="$PWD/.config"') "the shell hook must resolve Clippy configuration against the entered repository"

let nextest = $control.testing.nextest
let groups = $nextest.test-groups | columns
assert equal ($groups | sort) (
    [
        "allocator-global"
        "display-global"
        "live-qdrant"
        "native-compiler"
        "process-global"
        "telemetry-global"
    ]
    | sort
) "nextest scarce-resource groups changed"
assert equal $nextest.profile.default.retries 0 "default tests must expose flakes instead of retrying"
assert equal $nextest.profile.default.flaky-result "fail" "a flaky retry may never become green"
for override in $nextest.profile.default.overrides {
    assert ($override.test-group in $groups) $"nextest override references unknown group ($override.test-group)"
}
for profile in ["affected" "closure"] {
    let declaration = $nextest.profile | get $profile
    assert equal $declaration.junit.store-success-output false $"($profile) JUnit must not duplicate successful output"
    assert equal $declaration.junit.store-failure-output true $"($profile) JUnit must retain failing output"
}

let observation = $control.observability
assert $observation.localOnly "tool telemetry must remain local by construction"
assert ((
    $observation.eventRequired
    | where {|attribute| $attribute in $observation.prohibitedAttributes }
) | is-empty) "required telemetry attributes may not contain prohibited data"
let measurements = $observation.measurements.targets | transpose name declaration
assert (not ($measurements | is-empty)) "measurement authority needs explicit Nix targets"
for measurement in $measurements {
    assert ($measurement.name =~ $observation.measurements.namePattern) $"invalid measurement target name ($measurement.name)"
    for field in ["kind" "tool" "protocol"] {
        assert ($field in ($measurement.declaration | columns)) $"measurement ($measurement.name) lacks ($field)"
    }
    match $measurement.declaration.kind {
        "cargo-build" => { assert equal $measurement.declaration.selector "changed-packages" }
        "cargo-benchmark" => {
            assert ("package" in ($measurement.declaration | columns))
            assert ("benchmark" in ($measurement.declaration | columns))
        }
        "cargo-binary" | "cargo-profile" => {
            assert ("package" in ($measurement.declaration | columns))
            assert ("binary" in ($measurement.declaration | columns))
        }
        _ => { error make {msg: $"unknown measurement kind ($measurement.declaration.kind)"} }
    }
}

let required_rubric_fields = $control.rubric.required
assert equal ($required_rubric_fields | uniq | length) ($required_rubric_fields | length) "rubric fields must be unique"
for field in [
    "terminal"
    "red_falsifier"
    "anti_cheat_mutant"
    "representation_bound"
    "allocation_bound"
    "latency_or_work_bound"
    "error_fidelity"
    "concurrency_model"
    "authority"
    "independent_oracle"
    "scope_exclusions"
    "simplification_attack"
    "availability_policy"
] {
    assert ($field in $required_rubric_fields) $"rubric must require ($field)"
}
for role in ["luna" "terra" "sol"] {
    let promotion = $control.evaluation.promotion | get $role
    assert greater $promotion.trials 0 $"($role) promotion needs repeated trials"
    assert greater or equal $promotion.requiredFirstPass 0.875 $"($role) promotion bar is too weak"
    assert (not ($promotion.hardFailures | is-empty)) $"($role) promotion needs hard failures"
}

let cases = $control.evaluation.cases
assert greater or equal ($cases | length) 10 "agent evaluation needs a varied adversarial matrix"
assert equal ($cases.id | uniq | length) ($cases.id | length) "agent evaluation identifiers must be unique"
for case in $cases {
    let role = $roles | where id == $case.role | first
    assert ($case.first in $role.allowed) $"($case.id) first command must be executable by its role"
    assert ($case.allow | all {|id| $id in $role.allowed }) $"($case.id) allowed commands must belong to its role"
    assert ($case.deny | all {|id| $id not-in $role.allowed }) $"($case.id) denied commands must remain unavailable"
    assert (
        (
            $case.allow
            | append $case.deny
            | uniq
            | length
        ) == (($case.allow | length) + ($case.deny | length))
    ) $"($case.id) allow and deny sets must be disjoint"
}
