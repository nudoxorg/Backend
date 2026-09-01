# Enforces runtime command custody from the single Nix role declaration.
# Distinguishes human use from stable agent role identifiers without aliases.
# Makes a role-policy edit immediately affect every privileged command boundary.

# Returns the active stable role identifier or the explicit human authority.
def active-role []: nothing -> string {
    let role = $env.BACKEND_AGENT_ROLE? | default "human"
    let known = role-declarations | get id | append "human"
    if $role not-in $known {
        tooling-fail "unknown-role" $"BACKEND_AGENT_ROLE names no declared role: ($role)"
    }
    $role
}

# Proves that the active role owns one command derived from live Nu metadata.
def --env require-command [command_id: string]: nothing -> nothing {
    let catalog = (command-catalog)
    if $command_id not-in $catalog.id {
        tooling-fail "unknown-command" $"capability check references undeclared command id ($command_id)"
    }
    let role = (active-role)
    $env.BACKEND_AGENT_COMMAND_ID = $command_id
    $env.BACKEND_AGENT_INVOCATION_ID = $env.BACKEND_AGENT_INVOCATION_ID? | default (random uuid)
    if $role == "human" { return }
    let contract = role-declarations | where id == $role | first
    let expected_digest = $contract.contractDigest
    let observed_digest = $env.BACKEND_AGENT_CONTRACT_DIGEST? | default ""
    if $observed_digest != $expected_digest {
        tooling-fail "stale-role-contract" $"role ($role) command surface does not match the active contract" "enter through the current Nix-built role toolbox"
    }
    let tool = $env.BACKEND_AGENT_TOOL? | default ""
    if ($tool | is-empty) or ($tool not-in ($contract.tools | columns)) {
        tooling-fail "unbound-role-tool" $"role ($role) command lacks a declared short tool identity"
    }
    if ($contract.tools | get $tool) != $command_id {
        tooling-fail "role-tool-mismatch" $"short tool ($tool) may not invoke ($command_id)"
    }
    if $command_id not-in $contract.allowed {
        let command = $catalog | where id == $command_id | first
        tooling-fail "capability-denied" $"role ($role) may not execute ($command.cli)" $"allowed command ids: ($contract.allowed | str join ', ')"
    }
    record-tooling-event $tool (date now) 0 0 0 "admitted" "capability" "" | ignore
}
