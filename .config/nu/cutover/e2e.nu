# Run the process-level cutover gates with one bounded external-process
# deadline. Rust owns protocol admission and work accounting; this wrapper
# only selects the declared suite and checks that each named journey ran.
use std/assert

source ../core/failure.nu
source ../core/root.nu
source ../core/control.nu
source ../core/capability.nu
source ../core/telemetry.nu
source ../core/process.nu

let default_config = if (($env.PWD | path join ".config") | path exists) {
    $env.PWD | path join ".config"
} else {
    $env.PWD | path join "implementation/.config"
}

let root = $env.BACKEND_CONFIG_SNAPSHOT? | default $default_config
let workspace = $env.BACKEND_WORKSPACE_SNAPSHOT? | default ($root | path dirname)
let contract = open ($root | path join "nu/cutover/e2e-contract.json")
assert equal $contract.schema 1 "cutover E2E contract schema changed"
assert ($contract.build_timeout_seconds > 0) "cutover E2E build timeout must be positive"
assert ($contract.timeout_seconds > 0) "cutover E2E timeout must be positive"
assert (($contract.failure_injections | length) >= 3) "cutover E2E must retain fault coverage"
assert (($contract.work_counters | columns | length) >= 4) "cutover E2E must declare work counters"
assert (($contract.publication_fault_points | length) == 6) "publication fault coverage was reduced"
assert equal $contract.process_remote_contract.locald_profile "builtin" "compiled locald profile changed"
assert equal $contract.process_remote_contract.worker_profile "builtin" "compiled worker profile changed"
assert (($contract.process_remote_contract.required_observations | length) >= 8) "process remote coverage was reduced"
assert (($contract.regression_cases | length) >= 7) "cutover regression coverage was reduced"
assert (($contract.regression_test_target | str trim) != "") "cutover regression target is missing"

let runner = $workspace | path join "tests/journeys/run-cutover-e2e.sh"
let build_timeout = $"($contract.build_timeout_seconds)s"

let build_arguments = [
    "--signal=KILL" $build_timeout
    "sh"
    $runner
    "prepare"
]

let build = process-result "timeout" $build_arguments
assert equal $build.status 0 $"cutover E2E preparation failed: ($build.stderr)"

let runtime_timeout = $"($contract.timeout_seconds)s"

let runtime_arguments = [
    "--signal=KILL" $runtime_timeout
    "sh"
    $runner
    "execute"
]

let result = process-result "timeout" $runtime_arguments
assert equal $result.status 0 $"cutover E2E failed: ($result.stderr)"
for journey in $contract.journeys {
    assert ($result.stdout | str contains $journey) $"cutover E2E journey did not run: ($journey)"
}
for regression in $contract.regression_cases {
    assert ($result.stdout | str contains $regression) $"cutover regression did not run: ($regression)"
}
print $result.stdout
