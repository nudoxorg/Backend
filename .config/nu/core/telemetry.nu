# Records bounded tooling telemetry as immutable local event facts.
# Keeps sensitive paths, arguments, source, identifiers, and error text out of events.
# Makes OTLP export an explicit verifier action instead of a command-path dependency.

# Writes one concurrency-safe local tooling event and returns its artifact path.
def record-tooling-event [
    operation: string
    started_at: datetime
    status: int
    stdout_bytes: int
    stderr_bytes: int
    phase: string = "result"
    scope: string = "external-process"
    failure_fingerprint: string = ""
]: nothing -> string {
    let elapsed = (date now) - $started_at | into int
    let directory = local-root | path join "observability" "tooling" "events"
    mkdir $directory
    let artifact = (
        $directory
        | path join $"(date now | format date '%Y%m%dT%H%M%S%.fZ')-(random uuid).json"
    )
    let temporary = $directory | path join $".event-(random uuid)"
    {
        schema: 1
        operation: $operation
        command_id: ($env.BACKEND_AGENT_COMMAND_ID? | default $operation)
        role: (active-role)
        run: ($env.BACKEND_AGENT_RUN? | default "none" | hash sha256)
        card: ($env.BACKEND_AGENT_CARD? | default "none" | hash sha256)
        contract_digest: ($env.BACKEND_AGENT_CONTRACT_DIGEST? | default "human")
        catalog_digest: ($env.BACKEND_COMMAND_CATALOG_DIGEST? | default "human")
        invocation_id: ($env.BACKEND_AGENT_INVOCATION_ID? | default (random uuid))
        parent_event_id: ($env.BACKEND_AGENT_PARENT_EVENT_ID? | default "")
        scope: $scope
        phase: $phase
        started_at: ($started_at | format date "%Y-%m-%dT%H:%M:%S%.fZ")
        elapsed_ns: $elapsed
        status: (if $status == 0 { "passed" } else { "failed" })
        changed_paths_digest: ($env.BACKEND_AGENT_CHANGE_DIGEST? | default "none" | hash sha256)
        failure_fingerprint: $failure_fingerprint
        stdout_bytes: $stdout_bytes
        stderr_bytes: $stderr_bytes
    } | to json --indent 2 | save --raw $temporary
    ^chmod "0600" $temporary
    mv $temporary $artifact
    $artifact
}
