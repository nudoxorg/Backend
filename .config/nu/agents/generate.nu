# Generates transient agent skills from live Nushell commands and role policy.
# Couples every role capability to an executable command instead of stale prose.
# Validates privilege separation before writing ignored skill artifacts.

# Returns live commands joined to adjacent docs, classes, and typed signatures.
def command-catalog []: nothing -> table {
    let live = scope commands | where {|command| ($command.name == "main") or ($command.name | str starts-with "main ") }
    let documented = (command-documentation)
    let live_names = $live | get name | sort
    let documented_names = $documented | get nu | sort
    if $live_names != $documented_names {
        let undocumented = $live_names | where {|name| $name not-in $documented_names }
        let detached = $documented_names | where {|name| $name not-in $live_names }
        tooling-fail "command-catalog-drift" $"undocumented live commands: ($undocumented | str join ', '); detached metadata: ($detached | str join ', ')"
    }
    $documented
    | where {|authored| $authored.nu | str starts-with "main " }
    | each {|authored|
      let matches = $live | where name == $authored.nu
      if ($matches | length) != 1 {
        tooling-fail "missing-command" $"documented command ($authored.nu) does not resolve exactly once"
      }
      let command = $matches | first
      $authored | merge {
        description: $authored.summary
        signatures: $command.signatures
        category: $command.category?
        command_type: $command.command_type?
      }
    }
    | sort-by id
}

# Returns every Nix-declared role contract in stable identifier order.
def role-contracts []: nothing -> table {
    role-declarations
}

# Resolves role-local tool names to live command metadata.
def role-commands [role: record, catalog: table]: nothing -> table {
    $role.tools
    | transpose tool command_id
    | each {|binding|
      let command = $catalog | where id == $binding.command_id | first
      $command | merge {tool: $binding.tool}
    }
    | sort-by tool
}

# Returns the canonical digest of one role contract and its command bindings.
def role-contract-digest [role: record]: nothing -> string {
    $role.contractDigest
}

# Returns progressively disclosed references for one role.
def role-references [role: record]: nothing -> table {
    let rubric = control-plane | get rubric
    match $role.id {
        "luna-pair" => [
            {
                file: "behavior.json"
                purpose: "the small behavioral contract for every Luna card"
                required: true
                content: $rubric.luna
            }
        ]
        "terra-academic" | "terra-reviewer" => {
            let terra = $rubric.terra
            [
                {
                    file: "engineering.json"
                    purpose: "architecture, error, proof, and delegation laws used on every candidate"
                    required: true
                    content: (
                        $terra
                        | select required architecture errors_and_observation proof delegation
                    )
                }
                {
                    file: "representation.json"
                    purpose: "representation, hot-path, and concurrency laws for data structures or performance-sensitive work"
                    required: false
                    content: ($terra | select representation hot_paths concurrency)
                }
                {
                    file: "migration.json"
                    purpose: "Cartesian completeness law for migrations, backends, and public projections"
                    required: false
                    content: {migrationCompleteness: $terra.migrationCompleteness}
                }
            ]
        }
        "sol-integrator" => [
            {
                file: "checklist.json"
                purpose: "the final cross-boundary integration checklist"
                required: true
                content: {checklist: $rubric.solChecklist}
            }
        ]
    }
}

# Renders a compact generated role skill with executable capability facts.
def render-role-skill [role: record, catalog: table, contract_digest: string]: nothing -> string {
    let commands = (role-commands $role $catalog)
    let command_lines = (
        $commands
        | each {|command| $"- `($command.tool)`: ($command.description)" }
        | str join "\n"
    )
    let terminal_lines = (
        $commands
        | where {|command| $command.id in $role.terminalCommands }
        | each {|command| $"- `($command.tool)`" }
        | str join "\n"
    )
    let entry = $role.entry | each {|condition| $"- ($condition)" } | str join "\n"
    let laws = $role.laws | each {|law| $"- ($law)" } | str join "\n"
    let forbidden = $role.forbidden | each {|law| $"- ($law)" } | str join "\n"
    let decision_fields = $role.decisionRequired | each {|field| $"- `($field)`" } | str join "\n"
    let finding_fields = (
        control-plane
        | get evaluation.findingRequired
        | each {|field| $"`($field)`" }
        | str join ", "
    )
    let finding_severities = control-plane | get evaluation.findingSeverities | str join ", "
    let finding_kinds = control-plane | get evaluation.findingKinds | str join ", "
    let route_lines = if "next_role" in $role.decisionRequired {
        control-plane
        | get evaluation.decisionRoutes
        | get $role.id
        | transpose verdict next_role
        | each {|route| $"- `($route.verdict)` -> `($route.next_role)`" }
        | str join "\n"
    } else {
        "- This terminal has no successor role."
    }
    let shape_lines = if $role.id in (control-plane | get evaluation.decisionShapes | columns) {
        control-plane
        | get evaluation.decisionShapes
        | get $role.id
        | transpose verdict constraints
        | each {|row|
            let fields = $row.constraints | transpose field constraint | each {|item| $"`($item.field)` = `($item.constraint)`" } | str join ", "
            $"- `($row.verdict)`: ($fields)"
        }
        | str join "\n"
    } else {
        "- No verdict-specific nullable fields."
    }
    let reference_lines = (
        role-references $role
        | each {|reference|
            let condition = if $reference.required { "Read before acting." } else { "Read only when the card touches this risk." }
            $"- `references/($reference.file)`: ($reference.purpose). ($condition)"
        }
        | str join "\n"
    )
    let allowed_tools = $commands | get tool | to json
    let model_id = $role.model? | default ""
    let model_registry = control-plane | get models
    let model = if ($model_id | is-empty) { null } else {
        $model_registry | get $model_id
    }
    let model_metadata = if $model == null { "" } else { $"  model: ($model_id | to json)\n" }
    let model_section = if $model == null {
        ""
    } else {
        $"## Model binding\n\nRun as the `($model_id)` agent from provider `($model.provider)`. Model custody is declared once in control.nix; substituting or relabeling the model inside a run is an orchestration failure, never a product finding.\n\n"
    }
    let description = $"Use when acting as ($role.title) for ($role.purpose | str downcase)"
    let budget_lines = (
        control-plane
        | get evaluation.budgets
        | get $role.id
        | transpose measure bound
        | each {|row| $"- `($row.measure)`: ($row.bound)" }
        | str join "\n"
    )
    $'---
name: ($role.id)
description: ($description | to json)
allowed-tools: ($allowed_tools)
metadata:
  role: ($role.id | to json)
($model_metadata)  contract-digest: ($contract_digest | to json)
---

# ($role.title)

Stable role id: `($role.id)`.
Contract digest: `($contract_digest)`.

($role.purpose)

($model_section)## Entry conditions

($entry)

## References

($reference_lines)

Load only the references routed above. The card supplies changing scope and acceptance facts; this skill supplies stable role behavior.

## Executable capabilities

Your role-specific PATH contains only these short commands. Invoke the short name exactly; never call `backend`, Cargo, Git, or a substitute directly.

($command_lines)

## Laws

($laws)

## Forbidden

($forbidden)

## Handoff

($role.exit)

The following short commands are mandatory terminal evidence for the final candidate identity:

($terminal_lines)

## Context envelope

Role instructions: ($role.context.role_tokens) tokens. Card: ($role.context.card_tokens) tokens. Direct code: ($role.context.code_tokens) tokens. Transcript policy: `($role.context.transcript)`.

## Termination budget

($budget_lines)

The wrapper derives candidate identity from tracked and untracked content before every short command. Hitting a bound ends this role run; it never licenses a retry, broader command, or weaker claim.

## Machine record

Return one valid JSON object, never prose or a sequence of forms. Set `role_id` to exact value `($role.id)`. Command use is audited from tooling events and must not be self-reported. `verdict` must be one of: ($role.decisionVerdicts | str join ", "). Required fields:

($decision_fields)

`findings` is a JSON array of objects. Every finding has exactly these semantic fields: ($finding_fields). `severity` is exactly one of: ($finding_severities). `kind` is exactly one of: ($finding_kinds). Product failures describe the candidate; contract, infrastructure, and orchestration failures never count as product evidence.

Closed routing by verdict:

($route_lines)

Verdict-specific field constraints use `identity` for a nonempty stable string and `null` for JSON null:

($shape_lines)
'
}

# Validates the closed model registry and every declared role model binding.
def validate-model-bindings [roles: table]: nothing -> nothing {
    let models = control-plane | get models
    if ($models | describe) !~ '^record' {
        tooling-fail "missing-model-registry" "control plane must declare the closed model registry"
    }
    for row in ($models | transpose id declaration) {
        for field in ["provider" "model"] {
            if $field not-in ($row.declaration | columns) {
                tooling-fail "invalid-model-declaration" $"model ($row.id) lacks required field ($field)"
            }
            let value = $row.declaration | get $field
            if (($value | describe) !~ '^string') or ($value | str trim | is-empty) {
                tooling-fail "invalid-model-declaration" $"model ($row.id) field ($field) must be a nonempty string"
            }
        }
    }
    for role in $roles {
        let binding = $role.model? | default ""
        if not ($binding | is-empty) {
            if ($binding | describe) !~ '^string' {
                tooling-fail "invalid-role-model" $"role ($role.id) model binding must be a string"
            }
            if $binding not-in ($models | columns) {
                tooling-fail "unknown-role-model" $"role ($role.id) binds undeclared model ($binding)"
            }
        }
    }
}

# Proves role capabilities are real and privileged operations remain separated.
def validate-role-contracts [roles: table, catalog: table]: nothing -> nothing {
    let command_ids = $catalog | get id
    let declared_classes = control-plane | get evaluation.capabilityClasses
    let unknown_classes = $catalog | get class | uniq | where {|class| $class not-in $declared_classes }
    if not ($unknown_classes | is-empty) {
        tooling-fail "unknown-capability-class" $"live commands use undeclared capability classes: ($unknown_classes | str join ', ')"
    }
    validate-model-bindings $roles
    for role in $roles {
        let commands = (role-commands $role $catalog)
        if ($commands | is-empty) {
            tooling-fail "empty-role" $"role ($role.id) has no executable commands"
        }
        if $role.firstTool not-in ($role.tools | columns) {
            tooling-fail "invalid-first-tool" $"role ($role.id) first tool is unavailable: ($role.firstTool)"
        }
        for id in $role.allowed {
            if $id not-in $command_ids {
                tooling-fail "stale-role-command" $"role ($role.id) references missing command id ($id)"
            }
        }
        let unavailable_terminal_commands = $role.terminalCommands | where {|id| $id not-in $role.allowed }
        if not ($unavailable_terminal_commands | is-empty) {
            tooling-fail "unexecutable-role-terminal" $"role ($role.id) cannot execute terminal commands: ($unavailable_terminal_commands | str join ', ')"
        }
        let invalid_role_classes = $role.admittedClasses | where {|class| $class not-in $declared_classes }
        if not ($invalid_role_classes | is-empty) {
            tooling-fail "unknown-role-class" $"role ($role.id) admits undeclared classes: ($invalid_role_classes | str join ', ')"
        }
        let class_leaks = $commands | where {|command| $command.class not-in $role.admittedClasses }
        if not ($class_leaks | is-empty) {
            tooling-fail "role-class-leak" $"role ($role.id) received commands outside its admitted classes: ($class_leaks.id | str join ', ')"
        }
        if $role.id in ["luna-pair" "terra-academic"] {
            let leaked = (
                $commands
                | where {|command| $command.class in ["measurement" "privileged-verification" "baseline-write" "closure"] }
            )
            if not ($leaked | is-empty) {
                tooling-fail "privilege-leak" $"role ($role.id) received privileged command ids: ($leaked.id | str join ', ')"
            }
        }
        if $role.id == "luna-pair" {
            let classes = $commands | get class | uniq
            if $classes != ["implementation-feedback"] {
                tooling-fail "luna-privilege-leak" "Luna may execute only the opaque implementation-feedback command"
            }
        }
        if $role.id == "terra-reviewer" {
            let writes = (
                $commands
                | where {|command| $command.class in ["repository-write" "policy-write" "baseline-write" "closure"] }
            )
            if not ($writes | is-empty) {
                tooling-fail "reviewer-privilege-leak" $"reviewer received state-changing command ids: ($writes.id | str join ', ')"
            }
        }
        if ($role.id == "luna-pair") and not ($role.exit | str contains "terra-academic") {
            tooling-fail "invalid-handoff" "Luna red work must return to Terra academic before reviewer entry"
        }
    }
}

# Validates the executable workflow graph, budgets, and handoff schema.
def validate-evaluation-contract [roles: table]: nothing -> nothing {
    let evaluation = control-plane | get evaluation
    let states = $evaluation.workflow.states
    if ($states | uniq | length) != ($states | length) {
        tooling-fail "workflow-state-drift" "workflow states must be unique"
    }
    for terminal in $evaluation.workflow.terminal {
        if $terminal not-in $states {
            tooling-fail "workflow-terminal-drift" $"unknown workflow terminal: ($terminal)"
        }
    }
    let role_ids = $roles | get id | sort
    if ($evaluation.workflow.transitions | columns | sort) != $role_ids {
        tooling-fail "workflow-role-drift" "workflow transitions must cover every role exactly once"
    }
    if ($evaluation.budgets | columns | sort) != $role_ids {
        tooling-fail "budget-role-drift" "evaluation budgets must cover every role exactly once"
    }
    for role in $roles {
        for edge in ($evaluation.workflow.transitions | get $role.id) {
            let parts = $edge | split row ">"
            if (($parts | length) != 2) or (($parts | where {|state| $state not-in $states }) | is-not-empty) {
                tooling-fail "workflow-edge-drift" $"role ($role.id) has invalid workflow edge: ($edge)"
            }
            if ($parts | first) in $evaluation.workflow.terminal {
                tooling-fail "workflow-terminal-escape" $"terminal workflow state has an outgoing edge: ($edge)"
            }
        }
        let budget = $evaluation.budgets | get $role.id
        if ($budget.maximumToolCalls | into int) < 1 {
            tooling-fail "invalid-role-budget" $"role ($role.id) must have a positive tool-call budget"
        }
        if ($budget.maximumCallsPerCandidate | into int) < 1 {
            tooling-fail "invalid-candidate-budget" $"role ($role.id) must have a positive per-candidate call budget"
        }
        if ($budget.maximumIdenticalFailures | into int) != 1 {
            tooling-fail "invalid-retry-budget" $"role ($role.id) must stop after one identical failure without a changed candidate"
        }
        let has_route = "next_role" in $role.decisionRequired
        let route_roles = $evaluation.decisionRoutes | columns
        if $has_route {
            if $role.id not-in $route_roles {
                tooling-fail "missing-decision-route" $"role ($role.id) requires next_role but has no closed route table"
            }
            let routes = $evaluation.decisionRoutes | get $role.id
            if ($routes | columns | sort) != ($role.decisionVerdicts | sort) {
                tooling-fail "decision-route-drift" $"role ($role.id) routes must cover every verdict exactly once"
            }
            for destination in ($routes | values) {
                if $destination not-in $role_ids {
                    tooling-fail "decision-route-target" $"role ($role.id) routes to unknown role ($destination)"
                }
            }
        } else if $role.id in $route_roles {
            tooling-fail "unexpected-decision-route" $"role ($role.id) has a route table but no next_role field"
        }
        let shape_roles = $evaluation.decisionShapes | columns
        if $role.id in $shape_roles {
            let shapes = $evaluation.decisionShapes | get $role.id
            if ($shapes | columns | sort) != ($role.decisionVerdicts | sort) {
                tooling-fail "decision-shape-drift" $"role ($role.id) field constraints must cover every verdict exactly once"
            }
            for shape in ($shapes | values) {
                for constraint in ($shape | transpose field constraint) {
                    if $constraint.field not-in $role.decisionRequired {
                        tooling-fail "decision-shape-field" $"role ($role.id) constrains undeclared field ($constraint.field)"
                    }
                    if ($constraint.constraint not-in ["identity" "null"]) and ($constraint.constraint not-in $role_ids) {
                        tooling-fail "decision-shape-constraint" $"role ($role.id) has unknown field constraint ($constraint.constraint)"
                    }
                }
            }
        }
    }
    if ($evaluation.handoff.required | uniq | length) != ($evaluation.handoff.required | length) {
        tooling-fail "handoff-schema-drift" "handoff fields must be unique"
    }
    let envelope = $evaluation.contextEnvelope
    if $envelope.rawTranscript or ($envelope.unknownReference != "reject") {
        tooling-fail "context-envelope-drift" "delegation must reject unknown references and exclude raw transcripts"
    }
    if ($envelope.maximumSummaryTokens > $evaluation.handoff.maximumSummaryTokens) or ($envelope.maximumArtifactRefs < 1) {
        tooling-fail "context-envelope-bound" "delegation context must remain within handoff bounds"
    }
    let task = $evaluation.delegatedTask
    for terminal in $task.terminal {
        if not (($task.transitions | get $terminal) | is-empty) {
            tooling-fail "task-terminal-transition" $"delegated terminal state may not transition: ($terminal)"
        }
    }
    for row in ($task.transitions | transpose state destinations) {
        for destination in $row.destinations {
            if $destination not-in $task.states {
                tooling-fail "task-transition-drift" $"delegated task routes to unknown state: ($destination)"
            }
        }
    }
    if ($evaluation.resultIdentity | uniq | length) != ($evaluation.resultIdentity | length) {
        tooling-fail "result-identity-drift" "evaluation result identity fields must be unique"
    }
    if ($evaluation.casePartitions | columns | sort) != ["development" "holdout" "training"] {
        tooling-fail "case-partition-drift" "evaluation cases require training, development, and holdout partitions"
    }
    let case_ids = $evaluation.cases | get id
    let partitioned = $evaluation.casePartitions | values | flatten
    if (($partitioned | uniq | length) != ($partitioned | length)) or (($partitioned | sort) != ($case_ids | sort)) {
        tooling-fail "case-partition-coverage" "every evaluation case must occur in exactly one partition"
    }
    let holdout_roles = (
        $evaluation.cases
        | where {|case| $case.id in $evaluation.casePartitions.holdout }
        | get role
        | uniq
        | sort
    )
    if $holdout_roles != $role_ids {
        tooling-fail "holdout-role-coverage" "the locked holdout must independently exercise every role"
    }
    if ($evaluation.optimization.promoteWith != "holdout") or ($evaluation.optimization.holdoutVisibility != "evaluator-only") {
        tooling-fail "holdout-policy-drift" "promotion must use evaluator-only holdout cases"
    }
    let protocol = $evaluation.trialProtocol
    if ($protocol.seeds | length) != $evaluation.trialsPerCase {
        tooling-fail "trial-seed-drift" "every trial repetition requires one stable independent seed"
    }
    if (not $protocol.resetBetweenTrials) or (not $protocol.freshReproduction) or $protocol.ambientNetwork or $protocol.ambientCredentials {
        tooling-fail "trial-isolation-drift" "agent trials require clean reset with no ambient network or credentials"
    }
    if ($protocol.privateOracleVisibility != "evaluator-only") or ($protocol.successAggregation != "pass^k") {
        tooling-fail "trial-oracle-drift" "promotion requires an evaluator-only oracle and all-run pass^k reliability"
    }
    if (($protocol.terminalReasons | uniq | length) != ($protocol.terminalReasons | length)) or (($protocol.exploitEvents | uniq | length) != ($protocol.exploitEvents | length)) {
        tooling-fail "trial-taxonomy-drift" "trial terminal reasons and exploit events must be closed unique sets"
    }
    if $protocol.exploitOutcome != "fail" {
        tooling-fail "trial-exploit-drift" "any evaluator tampering or privilege exploit must fail the trial regardless of product output"
    }
    let comparison = $evaluation.comparison
    if ($comparison.mode != "paired") or (not $comparison.randomizeOrder) or (not $comparison.blindCandidateIdentity) or (not $comparison.requireSameCases) {
        tooling-fail "comparison-policy-drift" "skill promotion requires blinded randomized paired evaluation over identical cases"
    }
    if (not $comparison.judgeCalibration.orderInvariant) or (not $comparison.judgeCalibration.independentGoldLabels) {
        tooling-fail "judge-calibration-drift" "subjective graders require independent gold labels and order-invariance calibration"
    }
    let scorer_owners = $role_ids | append ["card-authority" "control-plane"]
    for row in ($evaluation.scorers | transpose id scorer) {
        if $row.scorer.owner not-in $scorer_owners {
            tooling-fail "scorer-owner-drift" $"scorer ($row.id) has unknown owner ($row.scorer.owner)"
        }
        if (($row.scorer.evidence | describe) !~ '^string') or ($row.scorer.evidence | str trim | is-empty) {
            tooling-fail "scorer-evidence-drift" $"scorer ($row.id) lacks a bounded evidence kind"
        }
        if ($row.scorer.hard | describe) != "bool" {
            tooling-fail "scorer-hardness-drift" $"scorer ($row.id) hard must be boolean"
        }
    }
    if $evaluation.scorers.performance.owner != "terra-reviewer" {
        tooling-fail "measurement-custody-drift" "performance scoring belongs only to Terra reviewer"
    }
    for role in ($roles | where id != "terra-reviewer") {
        let measurement_leaks = $role.admittedClasses | where {|class| $class in ["measurement" "baseline-write"] }
        if not ($measurement_leaks | is-empty) {
            tooling-fail "measurement-custody-drift" $"role ($role.id) received reviewer-owned measurement commands"
        }
    }
}

# Reconstructs one role trajectory from immutable command events.
def validate-agent-trajectory [role: record, event_directory: path]: nothing -> record {
    let selected = $event_directory | path expand
    let admitted_root = local-root | path expand
    if not (($selected == $admitted_root) or ($selected | str starts-with $"($admitted_root)/")) {
        tooling-fail "trajectory-path-escape" "agent trajectory must live beneath .local"
    }
    let paths = glob ($selected | path join "*.json") | sort
    if ($paths | is-empty) {
        tooling-fail "missing-trajectory" "agent decision has no command trajectory"
    }
    let expected_fields = control-plane | get observability.eventRequired | sort
    let events = $paths | each {|path|
        let bytes = ls $path | get size | first | into int
        if $bytes > 65536 {
            tooling-fail "trajectory-event-size" $"trajectory event exceeds 64 KiB: ($path | path basename)"
        }
        let event = open $path
        if (($event | columns | sort) != $expected_fields) {
            tooling-fail "trajectory-event-shape" $"trajectory event schema drift: ($path | path basename)"
        }
        $event
    } | where role == $role.id
    if ($events | is-empty) {
        tooling-fail "missing-role-trajectory" $"trajectory contains no events for role ($role.id)"
    }
    let admissions = $events | where phase == "admitted"
    if ($admissions | is-empty) or (($admissions | first).operation != $role.firstTool) {
        tooling-fail "invalid-first-command" $"role ($role.id) must begin with short tool ($role.firstTool)"
    }
    for field in ["run" "card" "contract_digest" "catalog_digest"] {
        if ($events | get $field | uniq | length) != 1 {
            tooling-fail "trajectory-identity-drift" $"trajectory spans multiple ($field) identities"
        }
    }
    if ($events | get contract_digest | first) != $role.contractDigest {
        tooling-fail "trajectory-contract-drift" $"trajectory does not use the active contract for role ($role.id)"
    }
    let admitted_invocations = $admissions | get invocation_id
    if ($admitted_invocations | uniq | length) != ($admitted_invocations | length) {
        tooling-fail "duplicate-admission" "one invocation may be admitted only once"
    }
    let budget = control-plane | get evaluation.budgets | get $role.id
    if ($admissions | length) > $budget.maximumToolCalls {
        tooling-fail "tool-call-budget" $"role ($role.id) exceeded its tool-call budget"
    }
    let stalled_candidates = (
        $admissions
        | group-by changed_paths_digest
        | transpose candidate calls
        | where {|group| ($group.calls | length) > $budget.maximumCallsPerCandidate }
    )
    if not ($stalled_candidates | is-empty) {
        tooling-fail "candidate-call-budget" $"role ($role.id) kept invoking tools without changing the candidate"
    }
    for event in $events {
        if $event.operation not-in ($role.tools | columns) {
            tooling-fail "trajectory-tool-leak" $"role ($role.id) used undeclared short tool ($event.operation)"
        }
        if ($role.tools | get $event.operation) != $event.command_id {
            tooling-fail "trajectory-command-mismatch" $"short tool ($event.operation) was recorded as ($event.command_id)"
        }
        if ($event.phase not-in ["admitted" "result"]) or ($event.status not-in ["passed" "failed"]) {
            tooling-fail "trajectory-state" "trajectory event has an unknown phase or status"
        }
        if ($event.status == "failed") and ($event.failure_fingerprint | is-empty) {
            tooling-fail "trajectory-fingerprint" "failed command result lacks a failure fingerprint"
        }
        if ($event.phase == "result") and ($event.invocation_id not-in $admitted_invocations) {
            tooling-fail "orphan-command-result" "command result has no matching admitted invocation"
        }
    }
    let failed = (
        $events
        | where {|event| ($event.phase == "result") and ($event.status == "failed") }
        | insert retry_key {|event| $"($event.operation):($event.failure_fingerprint):($event.changed_paths_digest)" }
    )
    if not ($failed | is-empty) {
        let repeated = $failed | group-by retry_key | transpose key attempts | where {|group| ($group.attempts | length) > $budget.maximumIdenticalFailures }
        if not ($repeated | is-empty) {
            tooling-fail "repeated-identical-failure" $"role ($role.id) repeated a failure without a changed candidate"
        }
    }
    let terminal_proof = (
        $role.terminalCommands
        | each {|command_id|
            let tool = (
                $role.tools
                | transpose tool bound_command
                | where bound_command == $command_id
                | get tool
                | first
            )
            let invocations = $admissions | where operation == $tool
            if ($invocations | is-empty) {
                {tool: $tool, candidate: null, passed: false, result_count: 0}
            } else {
                let invocation = $invocations | last
                let results = $events | where {|event| ($event.phase == "result") and ($event.invocation_id == $invocation.invocation_id) }
                {
                    tool: $tool
                    candidate: $invocation.changed_paths_digest
                    passed: ((not ($results | is-empty)) and ($results | all {|event| $event.status == "passed" }))
                    result_count: ($results | length)
                }
            }
        }
    )
    {
        role: $role.id
        admissions: ($admissions | length)
        events: ($events | length)
        terminal_proof: $terminal_proof
        status: "passed"
    }
}

# Grades one normalized agent outcome against typed findings and handoff policy.
def grade-agent-decision [role: record, decision: record]: nothing -> record {
    let expected = $role.decisionRequired | sort
    let observed = $decision | columns | sort
    if $observed != $expected {
        let unexpected = $observed | where {|field| $field not-in $expected }
        let absent = $expected | where {|field| $field not-in $observed }
        tooling-fail "invalid-decision-shape" $"role ($role.id) decision fields differ; missing: ($absent | str join ', '); unexpected: ($unexpected | str join ', ')"
    }
    let missing = (
        $role.decisionRequired
        | where {|field| $field not-in ($decision | columns) }
    )
    if not ($missing | is-empty) {
        tooling-fail "incomplete-decision" $"role ($role.id) omitted required fields: ($missing | str join ', ')"
    }
    if ($decision.role_id? | default "") != $role.id {
        tooling-fail "invalid-role-id" $"expected role_id ($role.id)"
    }
    if $decision.verdict not-in $role.decisionVerdicts {
        tooling-fail "invalid-verdict" $"role ($role.id) returned unsupported verdict ($decision.verdict)"
    }
    if ($decision.findings | describe) !~ '^(list|table)' {
        tooling-fail "invalid-findings" "decision findings must be a structured list"
    }
    let finding_fields = control-plane | get evaluation.findingRequired
    for finding in $decision.findings {
        if ($finding | describe) !~ '^record' {
            tooling-fail "invalid-finding" "every finding must be a structured record"
        }
        let missing_finding = $finding_fields | where {|field| $field not-in ($finding | columns) }
        if not ($missing_finding | is-empty) {
            tooling-fail "incomplete-finding" $"finding omitted fields: ($missing_finding | str join ', ')"
        }
        let unexpected_finding = $finding | columns | where {|field| $field not-in $finding_fields }
        if not ($unexpected_finding | is-empty) {
            tooling-fail "invalid-finding-shape" $"finding added unsupported fields: ($unexpected_finding | str join ', ')"
        }
        if $finding.severity not-in (control-plane | get evaluation.findingSeverities) {
            tooling-fail "invalid-finding-severity" $"unsupported finding severity: ($finding.severity)"
        }
        if $finding.kind not-in (control-plane | get evaluation.findingKinds) {
            tooling-fail "invalid-finding-kind" $"unsupported finding kind: ($finding.kind)"
        }
        for field in ["law" "evidence" "redesign"] {
            let value = $finding | get $field
            if (($value | describe) !~ '^string') or ($value | str trim | is-empty) {
                tooling-fail "invalid-finding-value" $"finding ($field) must be a nonempty string"
            }
        }
    }
    if ($role.id == "luna-pair") and ($decision.verdict == "RED") and ($decision.red_owner != "terra-academic") {
        tooling-fail "invalid-red-owner" "Luna RED work must return to Terra academic"
    }
    if $role.id == "terra-reviewer" {
        if ($decision.verdict == "REJECT") and ($decision.red_owner != "terra-academic") {
            tooling-fail "invalid-review-repair-owner" "Terra reviewer rejection must return to terra-academic"
        }
    }
    if "next_role" in $role.decisionRequired {
        let expected_route = control-plane | get evaluation.decisionRoutes | get $role.id | get $decision.verdict
        if $decision.next_role != $expected_route {
            tooling-fail "invalid-decision-route" $"role ($role.id) verdict ($decision.verdict) must route to ($expected_route)"
        }
    }
    let shape_roles = control-plane | get evaluation.decisionShapes | columns
    if $role.id in $shape_roles {
        let constraints = control-plane | get evaluation.decisionShapes | get $role.id | get $decision.verdict
        for constraint in ($constraints | transpose field expected) {
            let value = $decision | get $constraint.field
            match $constraint.expected {
                "identity" => {
                    if (($value | describe) !~ '^string') or ($value | str trim | is-empty) {
                        tooling-fail "invalid-decision-field" $"field ($constraint.field) requires a nonempty identity"
                    }
                }
                "null" => {
                    if $value != null {
                        tooling-fail "invalid-decision-field" $"field ($constraint.field) must be null for verdict ($decision.verdict)"
                    }
                }
                _ => {
                    if $value != $constraint.expected {
                        tooling-fail "invalid-decision-field" $"field ($constraint.field) must equal ($constraint.expected) for verdict ($decision.verdict)"
                    }
                }
            }
        }
    }
    if ($role.id == "terra-reviewer") and ($decision.verdict == "REJECT") and ($decision.findings | is-empty) {
        tooling-fail "unsupported-rejection" "a reviewer REJECT verdict requires at least one typed finding"
    }
    if ($role.id == "sol-integrator") and ($decision.verdict == "INTEGRATED") and not ($decision.remaining_reds | is-empty) {
        tooling-fail "contradictory-integration" "an INTEGRATED verdict cannot retain unresolved reds"
    }
    {
        role: $role.id
        status: "passed"
        finding_count: ($decision.findings | length)
    }
}

# Opens one bounded, non-escaping decision record from the durable agent run tree.
def bounded-agent-decision [candidate: path]: nothing -> record {
    let selected = $candidate | path expand --strict
    let admitted_root = local-root | path join "agent" "runs" | path expand --strict
    if not ($selected | str starts-with $"($admitted_root)/") {
        tooling-fail "decision-path-escape" "agent decisions must live beneath .local/agent/runs"
    }
    if ($selected | path type) != "file" {
        tooling-fail "decision-file-type" "agent decision must be one regular file"
    }
    let size = ls $selected | get size | first | into int
    if $size > 65536 {
        tooling-fail "decision-size" "agent decision exceeds the 64 KiB admission bound"
    }
    let decision = try {
        open $selected
    } catch {
        tooling-fail "invalid-decision-json" "agent decision is not a valid structured record"
    }
    if ($decision | describe) !~ '^record' {
        tooling-fail "invalid-decision-json" "agent decision must contain exactly one structured record"
    }
    $decision
}

# Grades a captured adversarial trial record against one live role contract.
# @class policy-read
def "main agents grade" [role: string, decision: path, --events: path]: nothing -> record {
    require-command "agents-grade"
    if $events == null {
        tooling-fail "missing-trajectory" "agent grading requires the bounded command-event directory for the same run"
    }
    let roles = (role-contracts)
    let matches = $roles | where id == $role
    if ($matches | is-empty) {
        tooling-fail "unknown-role" $"no role contract named ($role)"
    }
    let contract = $matches | first
    let normalized = bounded-agent-decision $decision
    let outcome = grade-agent-decision $contract $normalized
    let trajectory = validate-agent-trajectory $contract $events
    let positive = $normalized.verdict in ["CANDIDATE" "ACCEPT" "INTEGRATED"]
    if $positive {
        let unproved = $trajectory.terminal_proof | where passed == false
        if not ($unproved | is-empty) {
            tooling-fail "missing-terminal-proof" $"role ($role) lacks passing terminal evidence from: ($unproved.tool | str join ', ')"
        }
        if ($role == "luna-pair") and ($normalized.candidate != ($trajectory.terminal_proof | last | get candidate)) {
            tooling-fail "candidate-identity-mismatch" "Luna candidate identity must equal the content identity evaluated by its final test"
        }
    }
    $outcome | merge {trajectory: $trajectory}
}

# Generates digest-addressed transient skills and the live command catalog.
# @class policy-write
def "main agents generate" [--role: string, --output: path]: nothing -> record {
    require-command "agents-generate"
    let catalog = (command-catalog)
    let roles = (role-contracts)
    validate-role-contracts $roles $catalog
    validate-evaluation-contract $roles
    let selected = if ($role | is-empty) { $roles } else {
        $roles | where id == $role
    }
    if ($selected | is-empty) {
        tooling-fail "unknown-role" $"no role contract named ($role)"
    }
    let control = (control-plane)
    let generator_source = open --raw (configuration-root | path join "nu/agents/generate.nu")
    let digest = (
        {catalog: $catalog, control: $control, generator: $generator_source, roles: ($selected | get id)}
        | to json
        | hash sha256
    )
    let parent = if $output == null {
        local-root | path join "agent"
    } else {
        let selected = $output | path expand
        let mode = $env.BACKEND_CONFIG_MODE? | default "live"
        if $mode == "immutable" {
            let declared_output = $env.out? | default ""
            if ($declared_output | is-empty) {
                tooling-fail "skill-output-escape" "immutable skill generation requires the declared Nix output"
            }
            let store_output = $declared_output | path expand
            if $selected != $store_output {
                tooling-fail "skill-output-escape" "immutable skill generation may write only to its declared Nix output"
            }
        } else {
            let live_root = local-root | path join "agent"
            if not (($selected == $live_root) or ($selected | str starts-with $"($live_root)/")) {
                tooling-fail "skill-output-escape" "live skill generation may write only beneath .local/agent"
            }
        }
        $selected
    }
    let root = $parent | path join $digest
    if ($root | path exists) {
        return {
            digest: $digest
            directory: $root
            roles: ($selected | get id)
            commands: ($catalog | length)
        }
    }
    mkdir $parent
    let temporary = $parent | path join $".skill-(random uuid)"
    mkdir ($temporary | path join "skills")
    $catalog | to json --indent 2 | save ($temporary | path join "catalog.json")
    for contract in $selected {
        let directory = $temporary | path join "skills" $contract.id
        let references = $directory | path join "references"
        mkdir $references
        let contract_digest = role-contract-digest $contract
        render-role-skill $contract $catalog $contract_digest | save ($directory | path join "SKILL.md")
        for reference in (role-references $contract) {
            $reference.content | to json --indent 2 | save ($references | path join $reference.file)
        }
    }
    mv $temporary $root
    {
        digest: $digest
        directory: $root
        roles: ($selected | get id)
        commands: ($catalog | length)
    }
}

# Validates role denial laws, rubric schema, eval cases, and prompt snapshots.
# @class policy-read
def "main agents verify" []: nothing -> record {
    require-command "agents-verify"
    let catalog = (command-catalog)
    let roles = (role-contracts)
    validate-role-contracts $roles $catalog
    validate-evaluation-contract $roles
    let cases = control-plane | get evaluation.cases
    for case in $cases {
        let role = $roles | where id == $case.role | first
        let commands = role-commands $role $catalog | get id
        let unknown = ($case.allow | append $case.deny | uniq) | where {|id| $id not-in ($catalog | get id) }
        if not ($unknown | is-empty) {
            tooling-fail "agent-eval" $"case ($case.id) references unknown command ids: ($unknown | str join ', ')"
        }
        let overlap = $case.allow | where {|id| $id in $case.deny }
        if not ($overlap | is-empty) {
            tooling-fail "agent-eval" $"case ($case.id) both allows and denies: ($overlap | str join ', ')"
        }
        if (($case.allow | uniq | length) != ($case.allow | length)) or (($case.deny | uniq | length) != ($case.deny | length)) {
            tooling-fail "agent-eval" $"case ($case.id) repeats command ids"
        }
        if $case.first not-in $commands {
            tooling-fail "agent-eval" $"case ($case.id) first command is unavailable to ($case.role): ($case.first)"
        }
        for id in $case.allow {
            if $id not-in $commands {
                tooling-fail "agent-eval" $"case ($case.id) expected ($case.role) to allow ($id)"
            }
        }
        for id in $case.deny {
            if $id in $commands {
                tooling-fail "agent-eval" $"case ($case.id) expected ($case.role) to deny ($id)"
            }
        }
    }
    let fixtures = control-plane | get evaluation.fixtures
    for fixture in $fixtures.valid {
        let decision = $fixture.decision
        let role = $roles | where id == $decision.role_id | first
        grade-agent-decision $role $decision | ignore
    }
    for fixture in $fixtures.invalid {
        let decision = $fixture.decision
        let role = $roles | where id == $decision.role_id | first
        let rejected = (try {
      grade-agent-decision $role $decision
      null
    } catch {|failure| $failure })
        if $rejected == null {
            tooling-fail "agent-eval" $"invalid agent fixture was accepted: ($fixture.id)"
        }
    }
    {
        roles: ($roles | length)
        cases: ($cases | length)
        valid_fixtures: ($fixtures.valid | length)
        invalid_fixtures: ($fixtures.invalid | length)
        status: "passed"
    }
}
