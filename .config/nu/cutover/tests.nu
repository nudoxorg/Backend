# Mutation tests for bootstrap control-plane contracts. These test the ledger
# laws without invoking Rust or a network service.
use std/assert

# Load the executable implementation used by the integration scenarios below.
# The scenarios intentionally change cwd into disposable Git repositories so
# the repository's real HEAD, index, and append-only ledger are exercised.
source ../core/failure.nu
source ../core/root.nu
source ../core/control.nu
source ../core/capability.nu
source ../core/telemetry.nu
source ../core/process.nu
source main.nu

let default_config = if (($env.PWD | path join ".config") | path exists) {
    $env.PWD | path join ".config"
} else {
    $env.PWD | path join "implementation/.config"
}
let root = $env.BACKEND_CONFIG_SNAPSHOT? | default $default_config
let test_policy_root = "test-policy-root"
$env.BACKEND_POLICY_ROOT_DIGEST = $test_policy_root
let source = open ($root | path join "contracts/schema.json")
assert equal $source.schema_version 1 "canonical schema version changed"
assert equal $source.unknown_fields "reject" "unknown fields must be rejected"
let schema_dir = $root | path join "contracts/schemas"
for path in (glob ($schema_dir | path join "*.json")) {
    let item = open $path
    assert equal $item.x-backend-schema-version 1 $"schema version drift: ($path)"
    assert ($item.additionalProperties == false) $"schema accepts unknown fields: ($path)"
}

let fixture = open ($root | path join "fixtures/control-plane/scope-exactly-one.json")
let workspace_root = $env.BACKEND_WORKSPACE_SNAPSHOT? | default ($root | path dirname)
let git_probe = (^git -C $workspace_root rev-parse --show-toplevel | complete)
let workspace_git_root = $git_probe.stdout | str trim
let workspace_available = ($workspace_root | path join "Cargo.toml" | path exists)
let git_available = $git_probe.exit_code == 0 and $workspace_git_root == ($workspace_root | path expand)

let discovered_paths = if $workspace_available and not $git_available {
    glob ($workspace_root | path join "**" "*")
    | where {|path| ($path | path type) == "file"}
    | each {|path| $path | path relative-to $workspace_root}
    | where {|path| not ($path | str starts-with ".local/") and not ($path | str starts-with ".backend/") and not ($path | str starts-with "target/") and not ($path | str ends-with ".rmeta") and not ($path | str ends-with ".rlib")}
} else { [] }

let git_paths = if $git_available {
    ((^git -C $workspace_root ls-files | lines)
        | append (^git -C $workspace_root ls-files --others --exclude-standard | lines)
        | append $discovered_paths
        | uniq)
} else { $discovered_paths }
for case in $fixture.cases {
    assert equal ($case.scopes | length) 1 $"scope overlap or gap: ($case.path)"
    assert ($case.path in $git_paths) $"scope fixture path is absent from the current repository: ($case.path)"
}
if $workspace_available {
    let metadata = (
        ^cargo metadata --no-deps --manifest-path ($workspace_root | path join "Cargo.toml") --format-version 1
        | from json
    )

    let package_roots = $metadata.packages | each {|package|
    let manifest = $package.manifest_path | path relative-to $workspace_root
    $manifest | path dirname
}

    let mechanical = $fixture.inventory.mechanical_scopes
    for root in ($mechanical | columns) {
        let expected = $mechanical | get $root
        let paths = $git_paths | where {|path| $path == $root or ($path | str starts-with $"($root)/")}
        assert ($paths | is-not-empty) $"mechanical scope root is absent: ($root)"
        for path in $paths {
            let matches = if $root in [
                "apps"
                "crates"
                "extensions"
                "frontends"
                "tests"
                "tools"
            ] { [$expected] } else { [] }
            assert equal ($matches | length) 1 $"current Git path lacks exactly one generated scope: ($path)"
        }
    }
    assert equal ($package_roots | uniq | length) ($metadata.packages | length) "cargo metadata package roots must be unique"
    assert (($package_roots | all {|package_root|
    ($mechanical | columns | any {|scope_root| $package_root == $scope_root or ($package_root | str starts-with $"($scope_root)/")})
})) "every current Cargo package must be mechanically scoped"
    let count_owners = {|scope_fixture, candidate_path|
        let package_matches = $package_roots | where {|package_root| $candidate_path == $package_root or ($candidate_path | str starts-with $"($package_root)/")}
        let scoped_roots = $scope_fixture.inventory.shared_roots
        let shared_matches = $scoped_roots | columns | where {|scope_root| $candidate_path == $scope_root or ($candidate_path | str starts-with $"($scope_root)/")}
        let explicit_shared = if $candidate_path in $scope_fixture.inventory.shared_paths { ["shared-path"] } else { [] }
        ($package_matches | length) + ($shared_matches | length) + ($explicit_shared | length)
    }
    for path in $git_paths {
        let owner_count = do $count_owners $fixture $path
        assert equal $owner_count 1 $"governed path must have exactly one scope: ($path)"
    }
    let missing_scope_path = "unclassified/omitted.txt"
    assert equal (do $count_owners $fixture $missing_scope_path) 0 "missing scope mutation must be rejected"
    let app_root = $package_roots | where {|package_root| $package_root | str starts-with "apps/"} | first
    let duplicate_fixture = $fixture | upsert inventory.shared_roots.apps "duplicate-app-scope"
    assert (
        (do $count_owners $duplicate_fixture ($app_root | path join "Cargo.toml")) > 1
    ) "overlapping scope mutation must be rejected"
} else {
    print "scope inventory: immutable configuration snapshot has no Cargo workspace"
}
let mutation_fixture = open ($root | path join "fixtures/control-plane/cutover-mutations.json")
assert equal $mutation_fixture.schema_version 1 "cutover mutation fixture schema drift"
assert (($mutation_fixture.mutations | length) >= 15) "cutover mutation campaign was reduced"
for mutation in $mutation_fixture.mutations {
    assert (($mutation.id | str trim) != "") "mutation fixture case lacks an id"
    assert (($mutation.input | str trim) != "") "mutation fixture case lacks an input"
    assert (($mutation.expected | str trim) != "") "mutation fixture case lacks an oracle"
}
let regression_fixture = open ($root | path join "fixtures/cutover/regression-cases.json")
assert equal $regression_fixture.schema_version 1 "cutover regression fixture schema drift"
assert (($regression_fixture.runner | str trim) != "") "cutover regression fixture lacks a runner"
assert (($regression_fixture.cases | length) >= 7) "cutover regression fixture was reduced"
for case in $regression_fixture.cases {
    for field in [id test oracle fault] {
        assert (($case | get $field | str trim) != "") $"regression case lacks ($field): ($case.id)"
    }
}
let ids = [
    CellDescriptor
    AcceptanceContract
    TestContract
    EvaluationKey
    EvaluationContract
    CandidateReceipt
    EvaluationReceipt
    DecisionReceipt
    AgentWorkKey
    LeaseFence
    EvidenceRecord
    Supersession
    ContextCursor
    ContextDelta
]
assert equal ($ids | length) ($source.definitions | columns | length) "schema source definitions are incomplete"
let schemas = glob ($schema_dir | path join "*.json") | each {|path| open $path}
for name in $ids {
    let matches = (
        $schemas
        | where {|schema| (try { $schema | get '$id' } catch { "" }) == $"backend.control-plane.v1/($name)" }
    )
    assert equal ($matches | length) 1 $"canonical definition lacks exactly one projected schema: ($name)"
}
assert equal ($schemas | length) ($ids | length) "schema projection has extra definitions"

# The receipt boundary is authority owned and the CAS must have a precondition.
let receipt = open ($schema_dir | path join "receipt.schema.json")
assert equal $receipt.properties.authority.enum ["sol"] "decision authority widened"
assert ("expected_head" in $receipt.required) "decision CAS precondition disappeared"
print "control-plane contract mutations: passed"
assert equal (digest-algorithm) "blake3-b3sum-length-domain-framed-v1" "control-plane digest algorithm drift"
assert equal (blake3-bytes (frame "backend.control-plane.v1" "abc")) "723e08299e6efde568e8d58b65106b66d19d388d971265bd7c9de723ba371162" "BLAKE3 framing vector changed"

def expect-failure [label: string, operation: closure, expected: string] {
    let result = (
        try {
            do $operation
            {ok: true}
        } catch {|failure| {
            ok: false
            msg: ($failure.msg? | default "")
        } }
    )
    assert equal $result.ok false $"($label): operation unexpectedly succeeded"
    assert equal $result.msg $expected $"($label): wrong rejection"
}

let policy_digest_fixture = (^mktemp --suffix .json | str trim)
{policyRootDigest: "pinned-policy-root"} | to json | save -f $policy_digest_fixture
with-env {BACKEND_CONTROL_PLANE: $policy_digest_fixture, BACKEND_POLICY_ROOT_DIGEST: "tampered-policy-root"} {
    expect-failure "policy root mutation" { require-policy-root } "policy-root-digest-mismatch"
}
let missing_policy_digest = (^mktemp --suffix .json | str trim)
{} | to json | save -f $missing_policy_digest
with-env {BACKEND_CONTROL_PLANE: $missing_policy_digest, BACKEND_POLICY_ROOT_DIGEST: $test_policy_root} {
    expect-failure "missing policy root" { require-policy-root } "policy-root-missing"
}
with-env {BACKEND_CONTROL_PLANE: $policy_digest_fixture} {
    hide-env BACKEND_POLICY_ROOT_DIGEST
    expect-failure "missing policy root environment" { require-policy-root } "policy-root-digest-missing"
}

def make-repo [] {
    let repo = (^mktemp -d | str trim)
    mkdir $repo
    cd $repo
    ^git init -q
    ^git config user.email cutover@example.invalid
    ^git config user.name cutover-test
    ".local/" | save -f .gitignore
    "base" | save -f owned.txt
    ^git add .gitignore owned.txt
    ^git commit -qm base
    $repo
}

def role-digest [role: string] {
    role-declarations | where id == $role | get contractDigest | first
}

def as-role [role: string, tool: string, operation: closure] {
    with-env {
        BACKEND_AGENT_ROLE: $role
        BACKEND_AGENT_CONTRACT_DIGEST: (role-digest $role)
        BACKEND_AGENT_TOOL: $tool
        BACKEND_AGENT_OWNER: "owner"
    } { do $operation }
}

def child-lease-script [
    repo: string
    policy: string
    role: string
    tool: string
    digest: string
    work: string
    owner: string
    barrier: string
    output: string
] {
    let config_root = $root
    let failure = $config_root | path join "nu" "core" "failure.nu"
    let root = $config_root | path join "nu" "core" "root.nu"
    let control = $config_root | path join "nu" "core" "control.nu"
    let main = $config_root | path join "nu" "cutover" "main.nu"
    let publication_ready = $barrier | path join $"($owner).publication-ready"
    let publication_release = $barrier | path join "publication-release"
    let lock_waiting = $barrier | path join $"($owner).lock-waiting"
    let script = $"source ($failure)\nsource ($root)\nsource ($control)\nsource ($main)\n\"ready\" | save -f '($barrier)/($owner).ready'\nlet start = '($barrier)/go-($owner)'\nwhile not __GO__ { sleep 1ms }\nwith-env {BACKEND_CONTROL_PLANE:($policy), BACKEND_POLICY_ROOT_DIGEST:($test_policy_root), BACKEND_AGENT_ROLE:($role), BACKEND_AGENT_CONTRACT_DIGEST:($digest), BACKEND_AGENT_TOOL:($tool), BACKEND_AGENT_OWNER:($owner), BACKEND_CUTOVER_PAUSE_AT:head-rename, BACKEND_CUTOVER_PAUSE_READY:($publication_ready), BACKEND_CUTOVER_PAUSE_RELEASE:($publication_release), BACKEND_CUTOVER_LOCK_WAITING:($lock_waiting)} { main cutover lease acquire ($work) ($owner) --ttl-ms 10000 | to json }\n"
    $script | str replace "__GO__" "($start | path exists)" | save -f $output
}

def test-concurrent-lease-once [] {
    let repo = (make-repo)
    let policy = (^mktemp --suffix .json | str trim)
    let scripts = (^mktemp -d | str trim)
    let roles = {
        terra-academic: {
            contractDigest: "terra-contract"
            tools: {
                cell-id: "cutover-id"
                lease-acquire: "cutover-lease-acquire"
                lease-renew: "cutover-lease-renew"
                lease-freeze: "cutover-lease-freeze"
                lease-expire: "cutover-lease-expire"
                lease-quarantine: "cutover-lease-quarantine"
                candidate-admit: "cutover-candidate-admit"
                evidence-admit: "cutover-receipt-admit"
                context-delta: "cutover-context-delta"
            }
        }
        terra-reviewer: {
            contractDigest: "review-contract"
            tools: {evaluation: "cutover-evaluate", evidence-admit: "cutover-receipt-admit", context-delta: "cutover-context-delta"}
        }
        sol-integrator: {
            contractDigest: "sol-contract"
            tools: {evidence-admit: "cutover-receipt-admit", decision: "cutover-decide", context-delta: "cutover-context-delta"}
        }
    }
    {policyRootDigest: $test_policy_root, roles: $roles} | to json | save -f $policy
    $env.BACKEND_CONTROL_PLANE = $policy
    let a = $scripts | path join "a.nu"
    let b = $scripts | path join "b.nu"
    (child-lease-script
        $repo
        $policy
        terra-academic
        lease-acquire
        terra-contract
        shared
        owner-a
        $scripts
        ($a)
    )
    (child-lease-script
        $repo
        $policy
        terra-academic
        lease-acquire
        terra-contract
        shared
        owner-b
        $scripts
        ($b)
    )
    let launcher = $scripts | path join "launch.sh"
    $"#!/bin/sh\ncd '($repo)'\nnu --no-config-file '($a)' > a.out 2>&1 & pa=\$!\nnu --no-config-file '($b)' > b.out 2>&1 & pb=\$!\nwhile [ ! -f '($scripts)/owner-a.ready' ] || [ ! -f '($scripts)/owner-b.ready' ]; do sleep 0.01; done\ntouch '($scripts)/go-owner-a'\nwhile [ ! -f '($scripts)/owner-a.publication-ready' ]; do sleep 0.01; done\ntouch '($scripts)/go-owner-b'\nwhile [ ! -f '($scripts)/owner-b.lock-waiting' ]; do sleep 0.01; done\ntouch '($scripts)/publication-release'\nwait \$pa; sa=\$?\nwait \$pb; sb=\$?\nprintf '%s %s\\n' \$sa \$sb\n" | save -f $launcher
    ^chmod +x $launcher
    let status = (^sh $launcher | str trim)
    if $status != "0 0" {
        print $status
        print (open ($repo | path join "a.out"))
        print (open ($repo | path join "b.out"))
    }
    assert equal $status "0 0" "concurrent contenders must both return a bounded result"
    let events = $repo | path join ".local" "cutover" "events"
    let lease_events = (
        glob ($events | path join "*.json")
        | each {|p| open $p }
        | where event == "lease"
        | where work_key == "shared"
    )
    # Timestamps are diagnostic and may tie on a fast filesystem. Select the
    # durable waiter transition by its state instead of relying on sort order.
    let waiter_events = $lease_events | where {|event| (($event.waiters? | default []) | length) == 1 }
    assert equal ($waiter_events | length) 1 "exactly one contender must be recorded as a waiter"
    let latest = $waiter_events | first
    assert ($latest.owner in ["owner-a" "owner-b"]) "lease owner must be one of the contenders"
    assert equal ($latest.epoch) 1 "contenders must not create two active epochs"
    assert equal ($latest.waiters | length) 1 "second contender must be recorded as a waiter"
    assert ($latest.waiters.0 != $latest.owner) "owner must differ from the recorded waiter"
    print "concurrent lease contenders: passed"
}

def test-concurrent-lease [] {
    for _ in 1..5 { test-concurrent-lease-once }
}

def test-fence-and-authority [] {
    let repo = (make-repo)
    let policy = (^mktemp --suffix .json | str trim)
    let roles = {
        terra-academic: {
            contractDigest: "terra-contract"
            tools: {
                lease-acquire: "cutover-lease-acquire"
                lease-renew: "cutover-lease-renew"
                lease-expire: "cutover-lease-expire"
                candidate-admit: "cutover-candidate-admit"
                evidence-admit: "cutover-receipt-admit"
                context-delta: "cutover-context-delta"
                cell-id: "cutover-id"
                work-key: "cutover-work-key"
            }
        }
        terra-reviewer: {
            contractDigest: "review-contract"
            tools: {evaluation: "cutover-evaluate", evidence-admit: "cutover-receipt-admit", context-delta: "cutover-context-delta"}
        }
        sol-integrator: {
            contractDigest: "sol-contract"
            tools: {decision: "cutover-decide", evidence-admit: "cutover-receipt-admit", context-delta: "cutover-context-delta"}
        }
    }
    {policyRootDigest: $test_policy_root, roles: $roles} | to json | save -f $policy
    $env.BACKEND_CONTROL_PLANE = $policy
    cd $repo
    let lease = (
        (as-role
            terra-academic
            lease-acquire
            { main cutover lease acquire work owner --ttl-ms 1 }
        )
    )
    let old_fence = $lease.fence
    sleep 10ms
    let takeover = (
        (as-role
            terra-academic
            lease-acquire
            { main cutover lease acquire work replacement --ttl-ms 10000 }
        )
    )
    assert equal $takeover.epoch 2 "expiry must advance the lease epoch"
    let work_id = (
        (as-role
            terra-academic
            work-key
            { main cutover work-key cell model medium toolchain input-digest }
        )
    )
    assert (($work_id | str length) == 64) "checked AgentWorkKey must be content addressed"
    (expect-failure
        "invalid work-key effort"
        {
            (as-role
                terra-academic
                work-key
                { main cutover work-key cell model invalid toolchain input-digest }
            )
        }
        "invalid-work-key-effort"
    )
    (expect-failure
        "stale fence"
        {
            (as-role
                terra-academic
                lease-renew
                { main cutover lease renew work $old_fence }
            )
        }
        "stale-or-foreign-fence"
    )
    (expect-failure
        "wrong role"
        {
            (as-role
                terra-reviewer
                lease-acquire
                { main cutover lease acquire other reviewer }
            )
        }
        "authority-capability-mismatch"
    )
    with-env {BACKEND_AGENT_ROLE: "terra-academic", BACKEND_AGENT_CONTRACT_DIGEST: "wrong", BACKEND_AGENT_TOOL: "lease-acquire"} {
        expect-failure "wrong contract" { main cutover lease acquire other owner } "role-contract-digest-mismatch"
    }
    with-env {BACKEND_AGENT_ROLE: "terra-academic", BACKEND_AGENT_CONTRACT_DIGEST: "terra-contract", BACKEND_AGENT_TOOL: "evaluation"} {
        expect-failure "wrong tool" { main cutover lease acquire other owner } "role-tool-mismatch"
    }
    print "fence and authority checks: passed"
}

def test-ledger-recovery [] {
    let policy = (^mktemp --suffix .json | str trim)
    let roles = {
        terra-academic: {
            contractDigest: "terra-contract"
            tools: {lease-acquire: "cutover-lease-acquire", cell-id: "cutover-id"}
        }
    }
    {policyRootDigest: $test_policy_root, roles: $roles} | to json | save -f $policy
    $env.BACKEND_CONTROL_PLANE = $policy
    # Tamper and missing-link checks use a real appended event.
    let repo = (make-repo)
    cd $repo
    let event = (
        (as-role
            terra-academic
            lease-acquire
            { main cutover lease acquire tamper owner }
        )
    )
    let path = $repo | path join ".local" "cutover" "events" $"($event.id).json"
    '{"event":"tampered"}' | save -f $path
    expect-failure "tampered event" { read-events } "ledger-hash-mismatch"
    (expect-failure
        "immutable event collision"
        { ensure-immutable-event $path $event.id }
        "immutable-event-collision"
    )
    let repo_missing = (make-repo)
    cd $repo_missing
    let missing = (
        (as-role
            terra-academic
            lease-acquire
            { main cutover lease acquire missing owner }
        )
    )
    rm ($repo_missing | path join ".local" "cutover" "events" $"($missing.id).json")
    expect-failure "missing link" { read-events } "ledger-missing-link"
    # A syntactically cyclic pointer is rejected before hash traversal.
    let repo_cycle = (make-repo)
    cd $repo_cycle
    let cycle_dir = $repo_cycle | path join ".local" "cutover" "events"
    mkdir $cycle_dir
    '{"event":"cycle","previous":"cycle"}' | save -f ($cycle_dir | path join "cycle.json")
    "cycle" | save -f ($repo_cycle | path join ".local" "cutover" "HEAD")
    expect-failure "cyclic ledger" { read-events } "ledger-cycle"
    # An independently valid event that is not reachable from HEAD is an orphan.
    let repo_orphan = (make-repo)
    cd $repo_orphan
    let first = (
        (as-role
            terra-academic
            lease-acquire
            { main cutover lease acquire orphan owner }
        )
    )
    let orphan = {
        event: "orphan"
        previous: $first.id
        at: (date now | into int)
    }
    let orphan_id = (id event $orphan)
    $orphan | to json | save -f ($repo_orphan | path join ".local" "cutover" "events" $"($orphan_id).json")
    expect-failure "orphan event" { read-events } "ledger-orphan"
    print "ledger tamper, missing, cycle, and orphan recovery: passed"
}

def test-publication-boundaries [] {
    let points = [
        "event-temp-sync"
        "event-rename"
        "events-dir-sync"
        "head-temp-sync"
        "head-rename"
        "root-sync"
    ]
    for point in $points {
        let repo = (make-repo)
        cd $repo
        let probe = {event: "publication-probe"}
        if $point in ["head-rename" "root-sync"] {
            let pending = (with-env {BACKEND_CUTOVER_FAULT: $point} {
                append-event $probe GENESIS
            })
            assert equal $pending.publication_status "sync-pending" $"post-HEAD fault ($point) must be typed as pending"
            assert $pending.sync_pending $"post-HEAD fault ($point) must retain the acknowledged append"
            let retry = (append-event $probe GENESIS)
            assert equal $retry.id $pending.id $"exact retry changed the event identity at ($point)"
            assert equal $retry.publication_status "committed" $"exact retry did not reconcile ($point)"
            assert (not $retry.sync_pending) $"exact retry remained pending at ($point)"
            (expect-failure
                $"conflicting retry ($point)"
                { append-event {event: "conflicting-publication-probe"} GENESIS }
                "append-idempotency-conflict"
            )
        } else {
            with-env {BACKEND_CUTOVER_FAULT: $point} {
                (expect-failure
                    $"fault hook ($point)"
                    { append-event $probe (head) }
                    $"fault-injected-($point)"
                )
            }
            # Before HEAD is visible, recovery may discard the prepared
            # orphan.  Retrying the same intent then appends one committed
            # event and never acknowledges a phantom record.
            let retry = (append-event $probe GENESIS)
            assert equal $retry.publication_status "committed" $"pre-HEAD retry did not commit ($point)"
            assert (not $retry.sync_pending) $"pre-HEAD retry remained pending at ($point)"
            (expect-failure
                $"conflicting retry ($point)"
                { append-event {event: "conflicting-publication-probe"} GENESIS }
                "append-idempotency-conflict"
            )
        }
        let recovered = (reconcile-ledger)
        assert equal ($recovered | where event == "publication-probe" | length) 1 $"recovery outcome drift at ($point)"
    }
    let repo = (make-repo)
    let barrier = (^mktemp -d | str trim)
    let config_root = $root
    let failure = $config_root | path join "nu" "core" "failure.nu"
    let root = $config_root | path join "nu" "core" "root.nu"
    let main = $config_root | path join "nu" "cutover" "main.nu"
    let writer = $barrier | path join "writer.nu"
    let reader = $barrier | path join "reader.nu"
    $"source ($failure)
source ($root)
source ($main)
with-env {BACKEND_CUTOVER_PAUSE_AT: "event-rename", BACKEND_CUTOVER_PAUSE_READY: "($barrier)/ready", BACKEND_CUTOVER_PAUSE_RELEASE: "($barrier)/release"} { append-event {event: "race-probe"} GENESIS | to json }
"done" | save -f "($barrier)/writer-done"
" | save -f $writer
    let reader_script = $"source ($failure)
source ($root)
source ($main)
while not __READY__ { sleep 1ms }
try { read-events | ignore } catch { }
let count = glob (events-root | path join "*.json") | length
$count | save -f "($barrier)/reader-count"
"
    let ready_expr = "('" + ($barrier | path join "ready") + "' | path exists)"
    $reader_script | str replace "__READY__" $ready_expr | save -f $reader
    let launcher = $barrier | path join "race.sh"
    $"#!/bin/sh
cd '($repo)'
nu --no-config-file '($writer)' > writer.out 2>&1 & writer=\$!
nu --no-config-file '($reader)' > reader.out 2>&1 & reader=\$!
while [ ! -f '($barrier)/ready' ]; do sleep 0.01; done
touch '($barrier)/release'
wait \$writer
wait \$reader
" | save -f $launcher
    ^chmod +x $launcher
    let race_status = (^sh $launcher | complete)
    assert equal $race_status.exit_code 0 "reader/writer publication race must complete"
    cd $repo
    assert equal ((open ($barrier | path join "reader-count")) | into int) 1 "reader must not delete a live prepared event"
    let final_events = read-events | where event == "race-probe"
    assert equal ($final_events | length) 1 "writer must publish a valid event after racing reader"
    print "publication ordering fault recovery: passed"
}

def test-lock-liveness-and-fence [] {
    let repo = (make-repo)
    cd $repo
    let lock = lock-path
    mkdir $lock
    "live-owner" | save -f ($lock | path join "owner")
    ($nu.pid) | save -f ($lock | path join "pid")
    (process-identity $nu.pid) | save -f ($lock | path join "process")
    "7" | save -f ($lock | path join "epoch")
    "1" | save -f ($lock | path join "heartbeat")
    # An ancient heartbeat cannot reclaim a live, paused owner.
    (expect-failure "paused live lock" { acquire-lock } "lock-held")
    rm -r $lock

    mkdir $lock
    "dead-owner" | save -f ($lock | path join "owner")
    "999999999" | save -f ($lock | path join "pid")
    "dead-process" | save -f ($lock | path join "process")
    "7" | save -f ($lock | path join "epoch")
    "1" | save -f ($lock | path join "heartbeat")
    let acquired = (acquire-lock)
    assert ($acquired.owner != "dead-owner") "dead owner fence must be replaced"
    assert ($acquired.epoch > 7) "lock epoch must advance across stale recovery"
    assert (($lock | path join "heartbeat" | path exists)) "lock must persist a heartbeat"
    (expect-failure "foreign lock release" { release-lock "forged-owner" } "lock-owner-mismatch")
    touch-lock-heartbeat
    release-lock $acquired.owner

    # Missing metadata is conservatively held.  It is never reclaimed by a
    # wall-clock heuristic that could race a paused writer.
    mkdir $lock
    "partial-owner" | save -f ($lock | path join "owner")
    (expect-failure "partial lock metadata" { acquire-lock } "lock-held")
    rm -r $lock
    print "lock liveness, heartbeat, epoch, and fence: passed"
}

def test-candidate-evaluation-and-cas [] {
    let repo = (make-repo)
    let policy = (^mktemp --suffix .json | str trim)
    let roles = {
        terra-academic: {
            contractDigest: "terra-contract"
            tools: {
                lease-acquire: "cutover-lease-acquire"
                candidate-admit: "cutover-candidate-admit"
                cell-id: "cutover-id"
                work-key: "cutover-work-key"
                context-delta: "cutover-context-delta"
            }
        }
        terra-reviewer: {
            contractDigest: "review-contract"
            tools: {evaluation: "cutover-evaluate", evidence-admit: "cutover-receipt-admit"}
        }
        sol-integrator: {
            contractDigest: "sol-contract"
            tools: {decision: "cutover-decide", evidence-admit: "cutover-receipt-admit"}
        }
    }
    let evaluation_contracts = {
        evaluation-1: {
            cell: "work-cell"
            evaluator_root: "evaluator-1"
            holdout: "holdout-1"
            environment_id: "environment-1"
            command: "true"
            cwd: "current-repo"
            wall_ms: 1000
            stdout_bytes: 1024
            stderr_bytes: 1024
            coverage: ["declared-command" "result-root" "holdout-oracle"]
            holdout_command: "printf holdout-1"
            environment: {}
        }
        forged-evaluation: {
            cell: "work-cell"
            evaluator_root: "evaluator-1"
            holdout: "holdout-1"
            environment_id: "environment-1"
            command: "false"
            cwd: "current-repo"
            wall_ms: 1000
            stdout_bytes: 1024
            stderr_bytes: 1024
            coverage: ["declared-command" "result-root" "holdout-oracle"]
            holdout_command: "printf holdout-1"
            environment: {}
        }
        failed-evaluation: {
            cell: "work-cell"
            evaluator_root: "evaluator-1"
            holdout: "holdout-1"
            environment_id: "environment-1"
            command: "false"
            cwd: "current-repo"
            wall_ms: 1000
            stdout_bytes: 1024
            stderr_bytes: 1024
            coverage: ["declared-command" "result-root" "holdout-oracle"]
            holdout_command: "printf holdout-1"
            environment: {}
        }
    }
    {
        policyRootDigest: $test_policy_root
        roles: $roles
        evaluation: {contracts: $evaluation_contracts}
        observability: {
            process: {maximumCaptureBytes: 1048576}
        }
    } | to json | save -f $policy
    $env.BACKEND_CONTROL_PLANE = $policy
    cd $repo
    let work_key = (
        (as-role
            terra-academic
            work-key
            { main cutover work-key work-cell model medium toolchain input-digest }
        )
    )
    let lease = (
        (as-role
            terra-academic
            lease-acquire
            { main cutover lease acquire $work_key owner --ttl-ms 120000 }
        )
    )
    let fence = $lease.fence
    "candidate" | save -f owned.txt
    let tree = (candidate-index-tree [owned.txt])
    let candidate = (
        (as-role
            terra-academic
            candidate-admit
            { main cutover candidate admit $work_key $fence $tree '["owned.txt"]' --cell work-cell --model-class model --effort medium --toolchain-root toolchain --input-digest input-digest }
        )
    )
    assert equal $candidate.cell "work-cell" "candidate must bind its cell"
    assert equal $candidate.parent_head (^git rev-parse HEAD | str trim) "candidate parent head must be current"
    assert ($candidate.contract_roots | is-not-empty) "candidate must emit contract roots"
    assert (($candidate.controller_checks | describe | str starts-with "list")) "candidate must emit controller checks"
    (expect-failure
        "candidate cell forgery"
        {
            (as-role
                terra-academic
                candidate-admit
                { main cutover candidate admit $work_key $fence $tree '["owned.txt"]' --cell other --model-class model --effort medium --toolchain-root toolchain --input-digest input-digest }
            )
        }
        "work-key-mismatch"
    )
    (expect-failure
        "candidate parent forgery"
        {
            (as-role
                terra-academic
                candidate-admit
                { main cutover candidate admit $work_key $fence $tree '["owned.txt"]' --parent-head wrong-parent --cell work-cell --model-class model --effort medium --toolchain-root toolchain --input-digest input-digest }
            )
        }
        "candidate-parent-mismatch"
    )
    "outside" | save -f outside.txt
    (expect-failure
        "out-of-scope candidate"
        {
            (as-role
                terra-academic
                candidate-admit
                { main cutover candidate admit $work_key $fence $tree '["owned.txt"]' --cell work-cell --model-class model --effort medium --toolchain-root toolchain --input-digest input-digest }
            )
        }
        "out-of-scope-edit"
    )
    rm outside.txt
    (expect-failure
        "evaluation key evaluator-root forgery"
        {
            (as-role
                terra-reviewer
                evaluation
                { main cutover evaluate evaluation-1 $tree forged-evaluator holdout-1 environment-1 "forged" }
            )
        }
        "evaluation-key-mismatch"
    )
    (expect-failure
        "evaluation key holdout forgery"
        {
            (as-role
                terra-reviewer
                evaluation
                { main cutover evaluate evaluation-1 $tree evaluator-1 forged-holdout environment-1 "forged" }
            )
        }
        "evaluation-key-mismatch"
    )
    let original_policy = open $policy
    let base_contract = $evaluation_contracts | get evaluation-1
    let invalid_contracts = (
        $evaluation_contracts
        | upsert invalid-timeout ($base_contract | upsert wall_ms 0)
        | upsert invalid-output ($base_contract | upsert command "printf 123" | upsert stdout_bytes 1)
        | upsert invalid-environment ($base_contract | upsert environment {SECRET: "forbidden"})
        | upsert invalid-holdout ($base_contract | upsert holdout_command "printf forged-holdout")
        | upsert invalid-coverage ($base_contract | upsert coverage ["declared-command" "forged-coverage"])
    )
    $original_policy | upsert evaluation {contracts: $invalid_contracts} | to json | save -f $policy
    (expect-failure
        "evaluation timeout contract mutation"
        {
            (as-role terra-reviewer evaluation { main cutover evaluate invalid-timeout $tree evaluator-1 holdout-1 environment-1 "forged" })
        }
        "evaluation-contract-invalid"
    )
    (expect-failure
        "evaluation output contract mutation"
        {
            (as-role terra-reviewer evaluation { main cutover evaluate invalid-output $tree evaluator-1 holdout-1 environment-1 "forged" })
        }
        "evaluation-stdout-overflow"
    )
    (expect-failure
        "evaluation environment contract mutation"
        {
            (as-role terra-reviewer evaluation { main cutover evaluate invalid-environment $tree evaluator-1 holdout-1 environment-1 "forged" })
        }
        "evaluation-environment-forbidden"
    )
    let eval_execution = {exit_code: 0, stdout: "", stderr: ""}
    let eval_root = (id "evaluation-result" $eval_execution)
    (expect-failure
        "coverage label forgery"
        {
            (as-role terra-reviewer evaluation { main cutover evaluate invalid-coverage $tree evaluator-1 holdout-1 environment-1 $eval_root })
        }
        "evaluation-coverage-unproven"
    )
    (expect-failure
        "independent holdout oracle mutation"
        {
            (as-role terra-reviewer evaluation { main cutover evaluate invalid-holdout $tree evaluator-1 holdout-1 environment-1 $eval_root })
        }
        "evaluation-holdout-mismatch"
    )
    $original_policy | to json | save -f $policy
    let eval_artifact = (id "evaluation-artifact" {result_root: $eval_root, stdout: "", stderr: ""})
    let evaluation = (
        (as-role
            terra-reviewer
            evaluation
            { main cutover evaluate evaluation-1 $tree evaluator-1 holdout-1 environment-1 $eval_root }
        )
    )
    assert equal $evaluation.outcome "verified" "successful evaluator command must verify"
    assert ($evaluation.holdout_evidence.digest | str trim | is-not-empty) "evaluation must bind independent holdout evidence"
    assert equal ($evaluation.coverage_evidence | length) ($evaluation.coverage | length) "every coverage claim must carry independent evidence"
    (expect-failure
        "forged evaluator result"
        {
            (as-role
                terra-reviewer
                evaluation
                { main cutover evaluate forged-evaluation $tree evaluator-1 holdout-1 environment-1 $eval_root }
            )
        }
        "evaluation-result-mismatch"
    )
    let failed_root = (id "evaluation-result" {exit_code: 1, stdout: "", stderr: ""})
    let failed_evaluation = (
        (as-role
            terra-reviewer
            evaluation
            { main cutover evaluate failed-evaluation $tree evaluator-1 holdout-1 environment-1 $failed_root }
        )
    )
    assert equal $failed_evaluation.outcome "failed" "failing evaluator command must remain failed"
    let candidate_id = $candidate.id
    let claim = (
        {
            candidate_tree: $tree
            cell: "work-cell",
            candidate_receipt: $candidate_id
            evaluation_key: "evaluation-1",
            evaluator_root: "evaluator-1",
            holdout: "holdout-1",
            environment: "environment-1",
            result_root: $eval_root,
            fence: $fence
            outcome: "verified",
            artifact_digests: [$eval_artifact]
            coverage: $evaluation.coverage
            holdout_evidence: $evaluation.holdout_evidence
            coverage_evidence: $evaluation.coverage_evidence
        }
        | to json
    )
    let receipt = (
        (as-role
            terra-reviewer
            evidence-admit
            { main cutover receipt admit evaluator $work_key $fence $claim ([$eval_artifact] | to json) }
        )
    )
    (expect-failure
        "coverage evidence forgery"
        {
            let forged_claim = ($claim | from json | upsert coverage_evidence [] | to json)
            (as-role
                terra-reviewer
                evidence-admit
                { main cutover receipt admit evaluator $work_key $fence $forged_claim ([$eval_artifact] | to json) }
            )
        }
        "evaluation-receipt-binding"
    )
    (expect-failure
        "receipt authority forgery"
        {
            (as-role
                terra-reviewer
                evidence-admit
                { main cutover receipt admit sol $work_key $fence $claim '["artifact-digest"]' }
            )
        }
        "receipt-authority-mismatch"
    )
    (expect-failure
        "reviewer authority relabel"
        {
            (as-role
                terra-reviewer
                evidence-admit
                { main cutover receipt admit reviewer $work_key $fence $claim ([$eval_artifact] | to json) }
            )
        }
        "receipt-authority-mismatch"
    )
    (expect-failure
        "unbound evaluation receipt"
        {
            (as-role
                terra-reviewer
                evidence-admit
                { main cutover receipt admit evaluator $work_key $fence '{"candidate_tree":"other"}' '["artifact-digest"]' }
            )
        }
        "evaluation-receipt-binding"
    )
    let decision_lease = (
        (as-role
            terra-academic
            lease-acquire
            { main cutover lease acquire $work_key owner --ttl-ms 120000 }
        )
    )
    let decision_fence = $decision_lease.fence
    let expected = (^git rev-parse HEAD | str trim)
    ^git branch protected $expected
    ^git add owned.txt
    ^git commit -qm accepted
    let accepted = (^git rev-parse HEAD | str trim)
    let receipt_id = $receipt.id
    (expect-failure
        "decision requires evaluation receipt"
        {
            (as-role
                sol-integrator
                decision
                { main cutover decide $candidate_id '[]' $expected $accepted $decision_fence refs/heads/protected }
            )
        }
        "evaluation-receipt-incomplete"
    )
    (expect-failure
        "decision fence must bind to candidate work"
        {
            (as-role
                sol-integrator
                decision
                { main cutover decide $candidate_id $"[\"($receipt_id)\"]" $expected $accepted forged-fence refs/heads/protected }
            )
        }
        "stale-or-foreign-fence"
    )
    with-env {BACKEND_CUTOVER_FAULT: "decision-after-update-ref"} {
        (expect-failure
            "decision crash after protected ref update"
            {
                (as-role
                    sol-integrator
                    decision
                    { main cutover decide $candidate_id $"[\"($receipt_id)\"]" $expected $accepted $decision_fence refs/heads/protected }
                )
            }
            "fault-injected-decision-after-update-ref"
        )
    }
    assert equal (^git rev-parse refs/heads/protected | str trim) $accepted "faulted decision must leave the CAS update durable"
    let decision = (
        (as-role
            sol-integrator
            decision
            { main cutover decide $candidate_id $"[\"($receipt_id)\"]" $expected $accepted $decision_fence refs/heads/protected }
        )
    )
    assert equal (^git rev-parse refs/heads/protected | str trim) $accepted "decision must atomically advance protected ref"
    assert equal $decision.decision "verified" "decision receipt must record verification"
    let delta = (
        (as-role
            terra-academic
            context-delta
            { main cutover context delta unrelated-cell unrelated-cell }
        )
    )
    assert ($delta.facts | is-empty) "context delta must exclude events from unrelated cells"
    let context = (
        (as-role
            terra-academic
            context-delta
            { main cutover context delta consumer work-cell }
        )
    )
    let acknowledged = $context.facts | each {|e| id event $e} | to json
    let replay = (
        (as-role
            terra-academic
            context-delta
            { main cutover context delta consumer work-cell --since-root $context.new_context_root --acknowledged-evidence $acknowledged }
        )
    )
    assert ($replay.facts | is-empty) "context cursor acknowledgement must suppress replay"
    assert equal ($replay.cursor.acknowledged_evidence | length) ($context.facts | length) "context cursor must record acknowledgements"
    (expect-failure
        "context cursor root forgery"
        {
            (as-role
                terra-academic
                context-delta
                { main cutover context delta consumer work-cell --since-root forged-root --acknowledged-evidence $acknowledged }
            )
        }
        "context-cursor-mismatch"
    )
    let superseding = (append-event {
        event: "receipt",
        cell: "work-cell",
        supersedes: [$receipt_id]
        authority: "reviewer",
        work_key: $work_key,
        fence: $fence
        claim: "replacement",
        artifact_digests: []
        coverage: []
    } (head))
    let after_supersession = (
        (as-role
            terra-academic
            context-delta
            { main cutover context delta consumer work-cell --acknowledged-evidence $acknowledged }
        )
    )
    let after_ids = $after_supersession.facts | each {|e| id event $e}
    assert ($receipt_id not-in $after_ids) "context delta must apply supersession links"
    assert ($superseding.id in $after_ids) "context delta must expose the replacement evidence"
    ^git update-ref refs/heads/protected $expected
    (expect-failure
        "protected ref CAS race"
        {
            (as-role
                sol-integrator
                decision
                { main cutover decide $candidate_id $"[\"($receipt_id)\"]" $accepted $accepted $decision_fence refs/heads/protected }
            )
        }
        "decision-cas-precondition"
    )
    print "candidate index, evaluation binding, and protected CAS: passed"
}

test-concurrent-lease
test-fence-and-authority
test-ledger-recovery
test-publication-boundaries
test-lock-liveness-and-fence
test-candidate-evaluation-and-cas
print "cutover integration scenarios: passed"
