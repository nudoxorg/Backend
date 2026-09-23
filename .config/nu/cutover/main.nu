# Custody-separated bootstrap evidence ledger and cutover oracle.
#
# `backend-control` owns typed scheduling, leases, the complete
# candidate/evaluation/review/decision custody chain, and reusable work state.
# This append-only path is the Git promotion/audit adapter: it loads stored
# artifact IDs, binds policy and fences, verifies the actual Git graph, and
# performs `git update-ref <ref> <new> <expected>` before recording a committed
# external-effect decision. It cannot authorize the Rust dispatch lifecycle.
def cutover-root [] {
    let p = local-root | path join "cutover"
    mkdir $p
    mkdir ($p | path join "events")
    $p
}
def events-root [] {
    cutover-root | path join "events"
}
def head-path [] {
    cutover-root | path join "HEAD"
}
def lock-path [] {
    cutover-root | path join "LOCK"
}
def lock-epoch-path [] {
    cutover-root | path join "LOCK-EPOCH"
}
def now [] {
    date now | into int
}
def require-b3sum [] {
    if (which b3sum | is-empty) { error make {msg: "missing-b3sum"} }
}
def blake3-bytes [bytes: string] {
    require-b3sum
    let result = $bytes | ^b3sum | complete
    if $result.exit_code != 0 { error make {msg: "blake3-failed"} }
    $result.stdout | lines | first | split row " " | first
}
def frame [domain: string, bytes: string] {
    let length = $bytes | encode utf-8 | length
    $"($domain):($length):($bytes)"
}
def canonical [value]: any -> any {
    let kind = $value | describe | str replace --regex '<.*>$' ''
    if $kind == "record" {
        mut out = {}
        for key in ($value | columns | sort) { $out = ($out | insert $key (canonical ($value | get $key))) }
        $out
    } else if $kind == "list" {
        $value | each {|v| canonical $v}
    } else { $value }
}
def id [domain: string, value] {
    let bytes = canonical $value | to json --indent 0
    blake3-bytes (frame $domain $bytes)
}
# Pinned b3sum is the sole semantic digest implementation.
def digest-algorithm [] { "blake3-b3sum-length-domain-framed-v1" }
def cutover-fault [point: string] {
    let requested = $env.BACKEND_CUTOVER_FAULT? | default ""
    if $requested == $point { error make {msg: $"fault-injected-($point)"} }
    let paused = $env.BACKEND_CUTOVER_PAUSE_AT? | default ""
    if $paused == $point {
        let ready = $env.BACKEND_CUTOVER_PAUSE_READY? | default ""
        let release = $env.BACKEND_CUTOVER_PAUSE_RELEASE? | default ""
        if ($ready | is-not-empty) {
            "ready" | save -f $ready
        }
        if ($release | is-empty) { error make {msg: "pause-release-unbound"} }
        while not ($release | path exists) { sleep 1ms }
    }
}
def process-identity [pid: int] {
    let result = (^ps -p ($pid | into string) -o lstart= | complete)
    if $result.exit_code != 0 { "" } else {
        $result.stdout | str trim
    }
}
def process-live [pid: int, identity: string] {
    if ($pid <= 0) or ($identity | is-empty) { return false }
    let result = (^kill -0 ($pid | into string) | complete)
    if $result.exit_code != 0 { false } else { (process-identity $pid) == $identity }
}
def next-lock-epoch [] {
    let p = lock-epoch-path
    let current = if ($p | path exists) {
        try {
            open --raw $p | str trim | into int
        } catch { 0 }
    } else { 0 }
    if $current < 0 { error make {msg: "lock-epoch-corrupt"} }
    $current + 1
}
def persist-lock-epoch [epoch: int] {
    let temp = cutover-root | path join $".lock-epoch.(random uuid).tmp"
    $epoch | save -f $temp
    ^sync $temp
    mv $temp (lock-epoch-path)
    ^sync (cutover-root)
}
def touch-lock-heartbeat [] {
    let p = lock-path
    if not ($p | path exists) { error make {msg: "lock-held"} }
    let owner_path = $p | path join "owner"
    if not ($owner_path | path exists) { error make {msg: "lock-held"} }
    (now) | save -f ($p | path join "heartbeat")
    ^sync ($p | path join "heartbeat")
}
def acquire-lock [] {
    let p = lock-path
    if ($p | path exists) {
        let owner_path = $p | path join "owner"
        let pid_path = $p | path join "pid"
        let process_path = $p | path join "process"
        let epoch_path = $p | path join "epoch"
        let heartbeat_path = $p | path join "heartbeat"
        if not ($owner_path | path exists) or not ($pid_path | path exists) or not ($process_path | path exists) or not ($epoch_path | path exists) or not ($heartbeat_path | path exists) {
            # A partially written lock is unsafe to reclaim.  An operator or
            # recovery tool must inspect it rather than deleting by age.
            error make {msg: "lock-held"}
        }
        let lock_data = (try {
            {
                stamp: (open --raw $owner_path | str trim)
                pid: (open --raw $pid_path | str trim | into int)
                process: (open --raw $process_path | str trim)
                epoch: (open --raw $epoch_path | str trim | into int)
                heartbeat: (open --raw $heartbeat_path | str trim | into int)
            }
        } catch { error make {msg: "lock-held"} })
        if ($lock_data.stamp | is-empty) or ($lock_data.process | is-empty) or ($lock_data.epoch <= 0) or ($lock_data.heartbeat <= 0) {
            error make {msg: "lock-held"}
        }
        if (process-live $lock_data.pid $lock_data.process) {
            # A live owner may be paused at a publication boundary.  Its
            # heartbeat age is diagnostic only and never authorizes reclaim.
            error make {msg: "lock-held"}
        }
        let durable_epoch = if ((lock-epoch-path) | path exists) {
            try {
                open --raw (lock-epoch-path) | str trim | into int
            } catch { 0 }
        } else { 0 }
        if $durable_epoch < $lock_data.epoch {
            # A crash may have happened before the global epoch mirror was
            # synced.  Preserve the stale lock's fence before moving it out
            # of the acquisition path, so the replacement epoch is greater.
            persist-lock-epoch $lock_data.epoch
        }
        # Reclaim only a lock whose recorded process identity is gone.  Rename
        # the directory into a unique quarantine name first so a contender
        # that acquired a fresh lock cannot be removed by this stale check.
        let quarantine = $"($p).stale.($lock_data.epoch).(random uuid)"
        let moved = (^mv $p $quarantine | complete)
        if $moved.exit_code != 0 { error make {msg: "lock-held"} }
        try { rm -r $quarantine } catch { error make {msg: "lock-held"} }
    }
    let created = (^mkdir $p | complete)
    if $created.exit_code != 0 { error make {msg: "lock-held"} }
    let token = (random uuid)
    let epoch = (next-lock-epoch)
    $token | save -f ($p | path join "owner")
    ($nu.pid) | save -f ($p | path join "pid")
    (process-identity $nu.pid) | save -f ($p | path join "process")
    $epoch | save -f ($p | path join "epoch")
    (now) | save -f ($p | path join "heartbeat")
    ^sync ($p | path join "owner")
    ^sync ($p | path join "pid")
    ^sync ($p | path join "process")
    persist-lock-epoch $epoch
    ^sync ($p | path join "epoch")
    ^sync ($p | path join "heartbeat")
    {
        path: $p
        owner: $token
        epoch: $epoch
        pid: $nu.pid
        at: (now)
    }
}
def acquire-dispatch-lock [] {
    mut attempts = 0
    loop {
        let result = (
            try { {
                ok: true
                lock: (acquire-lock)
            } } catch {|failure| {
                ok: false
                msg: (try { $failure.msg } catch { "lock-held" })
            } }
        )
        if $result.ok { return $result.lock }
        if $result.msg != "lock-held" { error make {msg: $result.msg} }
        # Test harnesses may use this marker to release a deliberately
        # paused owner only after a real contender has observed the lock.
        # It carries no authority and is ignored unless explicitly bound by
        # the harness environment.
        let waiting = $env.BACKEND_CUTOVER_LOCK_WAITING? | default ""
        if ($waiting | is-not-empty) {
            try {
                "waiting" | save -f $waiting
            } catch { }
        }
        $attempts = $attempts + 1
        if $attempts > 5000 { error make {msg: "lock-timeout"} }
        sleep 1ms
    }
}
def release-lock [token: string] {
    let p = lock-path
    if ($p | path exists) {
        let actual = open --raw ($p | path join "owner") | str trim
        if $actual != $token { error make {msg: "lock-owner-mismatch"} }
        rm -r $p
    }
}
def head [] {
    let p = head-path
    if ($p | path exists) {
        open --raw $p | str trim
    } else { "GENESIS" }
}
def read-events [--recover-prepared] {
    mut current = (head)
    let ledger_head = $current
    mut seen = []
    mut out = []
    while $current != "GENESIS" {
        if $current in $seen { error make {msg: "ledger-cycle"} }
        let p = events-root | path join $"($current).json"
        if not ($p | path exists) { error make {msg: "ledger-missing-link"} }
        let item = (open $p)
        if ($item.previous? | default "") == $current { error make {msg: "ledger-cycle"} }
        if (id "event" $item) != $current { error make {msg: "ledger-hash-mismatch"} }
        $seen = ($seen | append $current)
        $out = ($out | append $item)
        $current = $item.previous
    }
    # A crash can leave an event durable before its HEAD update. Reject an
    # unreachable event instead of silently appending onto incomplete history.
    for p in (glob (events-root | path join "*.json")) {
        let name = $p | path basename | str replace ".json" ""
        if $name not-in $seen {
            let item = (try { open $p } catch { error make {msg: "ledger-orphan"} })
            if (id "event" $item) != $name { error make {msg: "ledger-hash-mismatch"} }
            if $recover_prepared and ($item.publication? | default "") == "prepared" and ($item.previous? | default "") == $ledger_head {
                rm $p
            } else {
                error make {msg: "ledger-orphan"}
            }
        }
    }
    $out | reverse
}
def event-work [item: record] {
    try {
        $item | get work_key
    } catch { "" }
}
def latest-work [work: string] {
    read-events | where {|e| ($e.event? | default "") == "lease" and (event-work $e) == $work} | last
}
def ensure-immutable-event [destination: path, digest: string] {
    if ($destination | path exists) {
        let old = (open $destination)
        if (id "event" $old) != $digest { error make {msg: "immutable-event-collision"} }
    }
}
def event-intent [event: record] {
    # Publication metadata is generated by the custody owner.  It must not
    # turn a retry of the same caller intent into a second logical event.
    $event | reject previous? at? publication? policy_root_digest? id? publication_status? sync_pending?
}
def retry-visible-event [event: record, expected: string, current: string] {
    if $current == "GENESIS" { error make {msg: "head-cas-conflict"} }
    let destination = events-root | path join $"($current).json"
    if not ($destination | path exists) { error make {msg: "head-cas-conflict"} }
    let existing = (try { open $destination } catch { error make {msg: "ledger-missing-link"} })
    if (id "event" $existing) != $current { error make {msg: "ledger-hash-mismatch"} }
    if ($existing.previous? | default "") != $expected { error make {msg: "head-cas-conflict"} }
    if (event-intent $existing) != (event-intent $event) {
        error make {msg: "append-idempotency-conflict"}
    }
    # Once HEAD names this event, the logical append is already committed.
    # A redundant directory sync may still be pending; expose that state as a
    # typed result so callers can retry the exact intent without appending a
    # second event.
    let durability = (try {
        ^sync (cutover-root)
        {publication_status: "committed", sync_pending: false}
    } catch {
        {publication_status: "sync-pending", sync_pending: true}
    })
    $existing | merge {id: $current} | merge $durability
}
def append-event-locked [event: record, expected: string] {
    touch-lock-heartbeat
    let current = (head)
    if $current != $expected { return (retry-visible-event $event $expected $current) }
    let body = $event | merge {previous: $current, at: (now), publication: "prepared", policy_root_digest: (policy-root-digest)}
    let digest = (id "event" $body)
    let destination = events-root | path join $"($digest).json"
    ensure-immutable-event $destination $digest
    if not ($destination | path exists) {
        let temp = events-root | path join $".$(random uuid).tmp"
        $body | to json --indent 2 | save -f $temp
        ^sync $temp
        cutover-fault "event-temp-sync"
        mv $temp $destination
        cutover-fault "event-rename"
        ^sync (events-root)
        cutover-fault "events-dir-sync"
    }
    let htemp = cutover-root | path join $".$(random uuid).head.tmp"
    $digest | save -f $htemp
    ^sync $htemp
    cutover-fault "head-temp-sync"
    mv $htemp (head-path)
    # HEAD has become visible at this point.  A fault in the post-rename
    # hook or redundant root sync is therefore an acknowledged append with a
    # recoverable sync state, matching the Rust selected-head contract.
    let durability = (try {
        cutover-fault "head-rename"
        ^sync (cutover-root)
        cutover-fault "root-sync"
        {publication_status: "committed", sync_pending: false}
    } catch {|failure|
        let observed = (try { head } catch { "" })
        if $observed == $digest {
            {publication_status: "sync-pending", sync_pending: true}
        } else {
            error make {msg: ($failure.msg? | default "head-publication-failed")}
        }
    })
    $body | merge {id:$digest} | merge $durability
}
def append-event [event: record, expected: string] {
    let g = (acquire-lock)
    try {
        read-events --recover-prepared | ignore
        let result = (append-event-locked $event $expected)
        release-lock $g.owner
        $result
    } catch {|failure|
        try { release-lock $g.owner } catch { }
        error make {
            msg: ($failure.msg? | default "ledger-write-failed")
        }
    }
}
def reconcile-ledger [] {
    let g = (acquire-lock)
    try {
        let result = (read-events --recover-prepared)
        release-lock $g.owner
        $result
    } catch {|failure|
        try { release-lock $g.owner } catch { }
        error make {
            msg: ($failure.msg? | default "ledger-recovery-failed")
        }
    }
}
def role [] {
    $env.BACKEND_AGENT_ROLE? | default ""
}
def role-contract [] {
    $env.BACKEND_AGENT_CONTRACT_DIGEST? | default ""
}
def policy-root-digest [] {
    try {
        control-plane | get policyRootDigest
    } catch { "" }
}
def require-policy-root [] {
    let expected = policy-root-digest
    if ($expected | is-empty) { error make {msg: "policy-root-missing"} }
    let observed = $env.BACKEND_POLICY_ROOT_DIGEST? | default ""
    if ($observed | is-empty) { error make {msg: "policy-root-digest-missing"} }
    if $observed != $expected { error make {msg: "policy-root-digest-mismatch"} }
}
def require-authority [authority: string] {
    let allowed = {
        controller: ["terra-academic"]
        evaluator: ["terra-reviewer"]
        reviewer: ["terra-reviewer"]
        sol: ["sol-integrator"]
    }
    let active = (role)
    if ($authority not-in ($allowed | columns)) or ($active not-in ($allowed | get $authority)) {
        error make {msg: "authority-capability-mismatch"}
    }
    let declarations = role-declarations | where id == $active
    if ($declarations | length) != 1 {
        error make {msg: "unknown-active-role"}
    }
    let expected = $declarations | first | get contractDigest
    if (role-contract) != $expected {
        error make {msg: "role-contract-digest-mismatch"}
    }
}
def require-tool [tool: string, expected_authority?: string] {
    require-policy-root
    let active = (role)
    let authority = (if $expected_authority == null { match $active {
        "terra-academic" => "controller"
        "terra-reviewer" => "reviewer"
        "sol-integrator" => "sol"
        _ => ""
    } } else { $expected_authority })
    require-authority $authority
    let declared = $env.BACKEND_AGENT_TOOL? | default ""
    if ($declared | is-empty) { error make {msg: "unbound-role-tool"} }
    let row = role-declarations | where id == $active | first
    if $tool not-in ($row.tools | columns) { error make {msg: "undeclared-role-tool"} }
    if $declared != $tool { error make {msg: "role-tool-mismatch"} }
}
def require-current-fence [work: string, fence: string] {
    let item = (latest-work $work)
    if ($item | is-empty) or (($item.fence? | default "") != $fence) { error make {msg: "stale-or-foreign-fence"} }
    let claimed_owner = $env.BACKEND_AGENT_OWNER? | default ""
    if ($claimed_owner | is-empty) { error make {msg: "lease-owner-unbound"} }
    if ($item.owner? | default "") != $claimed_owner { error make {msg: "lease-owner-mismatch"} }
    if ($item.state? | default "") in ["expired" "quarantined" "cancelled" "frozen"] { error make {msg: "terminal-lease"} }
    if ($item.expires_at? | default 0) <= (now) { error make {msg: "lease-expired"} }
    $item
}
def git-tree-digest [paths: list<any>] {
    let files = (
        $paths
        | each {|p| let q = (owned-path $p); if ($q | path exists) { {path:$p, digest:(blake3-bytes (open --raw $q))} } else { {path:$p, digest:"DELETED"} }}
    )
    id "tree" {
        root: (^git rev-parse HEAD | str trim)
        files: $files
    }
}
def cutover-changed-paths [] {
    let tracked = (^git diff --name-only HEAD | lines)
    let untracked = (^git ls-files --others --exclude-standard | lines)
    (
        $tracked
        | append $untracked
        | where {|p| ($p | str trim) != ""}
        | uniq
    )
}
def normalized-paths [paths: list<any>] {
    $paths | each {|p| $p | str trim | str replace --regex '/+' '/' | str trim -c '/' } | sort | uniq
}

# Builds the explicit Rust control-plane invocation used by the new dispatch
# commands.  The binary path is supplied by the pinned environment; the
# Cargo fallback is useful during a local checkout before a shell package has
# materialized the executable.  No ambient executable or shell interpolation
# is accepted.
def typed-control-invocation [command: string, arguments: list<string>] {
    let configured = $env.BACKEND_CONTROL_BIN? | default ""
    if ($configured | is-not-empty) {
        if not ($configured | path exists) { error make {msg: "backend-control-binary-missing"} }
        {
            program: $configured
            arguments: ([$command] | append $arguments)
        }
    } else {
        let cargo = $env.BACKEND_STABLE_CARGO? | default ""
        let source = $env.BACKEND_CONTROL_SOURCE? | default ""
        if ($cargo | is-empty) or ($source | is-empty) or not ($source | path exists) {
            error make {msg: "backend-control-unbound"}
        }
        {
            program: $cargo
            arguments: (
                [
                    "run"
                    "--quiet"
                    "--offline"
                    "--manifest-path" ($source | path join "Cargo.toml")
                    "--package" "backend-control"
                    "--bin" "backend-control"
                    "--" $command
                ]
                | append $arguments
            )
        }
    }
}

# Invokes the typed JSON adapter and admits exactly one bounded response.
def typed-control [command: string, arguments: list<string>]: nothing -> any {
    let invocation = (typed-control-invocation $command $arguments)
    let result = run-external $invocation.program ...$invocation.arguments | complete
    if $result.exit_code != 0 {
        let detail = if (($result.stderr | str trim) | is-not-empty) {
            $result.stderr | str trim
        } else {
            $result.stdout | str trim
        }
        error make {
            msg: ($detail | default $"backend-control-($command)-failed")
        }
    }
    let output = $result.stdout | str trim
    if ($output | is-empty) { error make {msg: $"backend-control-($command)-empty-response"} }
    try {
        $output | from json
    } catch { error make {msg: $"backend-control-($command)-invalid-response"} }
}

def typed-control-ledger [ledger: path] {
    if ($ledger | is-empty) { [] } else {
        [
            "--ledger"
            ($ledger | path expand)
        ]
    }
}

# Returns the selected typed control-plane root and rows.
# @class control-plane
def "main cutover control status" [--after: string = "", --limit: int = 256, --ledger: path = ""]: nothing -> record {
    require-tool "control-status" "controller"
    let arguments = typed-control-ledger $ledger
    let arguments = if ($after | is-empty) { $arguments } else {
        $arguments | append ["--after" $after]
    }
    let arguments = $arguments | append [
        "--limit"
        ($limit | into string)
    ]
    typed-control "status" $arguments
}

# Initializes the durable typed control-plane ledger.
# @class control-plane
def "main cutover control init" [--ledger: path = ""]: nothing -> record {
    require-tool "control-init" "controller"
    typed-control "init" (typed-control-ledger $ledger)
}

# Derives an AgentWorkKey from a complete immutable JSON WorkSpec.
# @class control-plane
def "main cutover control key" [spec: string]: nothing -> record {
    require-tool "control-key" "controller"
    let body = if ($spec | path exists) { open --raw $spec } else { $spec }
    typed-control "key" ["--json" $body]
}

# Plans and durably admits one immutable WorkSpec.
# @class control-plane
def "main cutover control plan" [spec: string, --ledger: path = ""]: nothing -> record {
    require-tool "control-plan" "controller"
    let body = if ($spec | path exists) { open --raw $spec } else { $spec }
    typed-control "plan" ((typed-control-ledger $ledger) | append ["--json" $body])
}

# Applies the plan alias through the same durable exact-base publication path.
# @class control-plane
def "main cutover control apply" [spec: string, --ledger: path = ""]: nothing -> record {
    require-tool "control-apply" "controller"
    let body = if ($spec | path exists) { open --raw $spec } else { $spec }
    typed-control "apply" ((typed-control-ledger $ledger) | append ["--json" $body])
}

# Acquires or coalesces one bounded typed work attempt.
# @class control-plane
def "main cutover control admit" [work_key: string, --owner: string = "cli", --ledger: path = ""]: nothing -> record {
    require-tool "control-admit" "controller"
    typed-control "admit" ((typed-control-ledger $ledger) | append ["--key" $work_key "--owner" $owner])
}

# Renews one exact typed attempt fence.
# @class control-plane
def "main cutover control renew" [work_key: string, fence: string, --ledger: path = ""]: nothing -> record {
    require-tool "control-renew" "controller"
    typed-control "renew" ((typed-control-ledger $ledger) | append ["--key" $work_key "--fence" $fence])
}

# Freezes one exact typed attempt into an evaluator-owned candidate state.
# @class control-plane
def "main cutover control candidate" [
    work_key: string
    fence: string
    output: string
    evidence: string
    --ledger: path = ""
]: nothing -> record {
    require-tool "control-candidate" "controller"
    typed-control "candidate" (
        (typed-control-ledger $ledger)
        | append [
            "--key" $work_key
            "--fence" $fence
            "--output" $output
            "--evidence" $evidence
        ]
    )
}

# Records an independent evaluator verdict over the exact candidate receipt.
# @class control-plane
def "main cutover control evaluation" [
    work_key: string
    fence: string
    evaluator: string
    verdict: string
    evidence: string
    --ledger: path = ""
]: nothing -> record {
    require-tool "control-evaluation" "evaluator"
    typed-control "evaluation" (
        (typed-control-ledger $ledger)
        | append [
            "--key" $work_key
            "--fence" $fence
            "--evaluator" $evaluator
            "--verdict" $verdict
            "--evidence" $evidence
        ]
    )
}

# Records an independent reviewer verdict over the exact evaluator receipt.
# @class control-plane
def "main cutover control review" [
    work_key: string
    evaluation_receipt: string
    reviewer: string
    verdict: string
    evidence: string
    --ledger: path = ""
]: nothing -> record {
    require-tool "control-review" "reviewer"
    typed-control "review" (
        (typed-control-ledger $ledger)
        | append [
            "--key" $work_key
            "--evaluation" $evaluation_receipt
            "--reviewer" $reviewer
            "--verdict" $verdict
            "--evidence" $evidence
        ]
    )
}

# Records Sol's final decision over the exact reviewer receipt.
# @class control-plane
def "main cutover control decision" [
    work_key: string
    review_receipt: string
    sol: string
    verdict: string
    evidence: string
    --ledger: path = ""
]: nothing -> record {
    require-tool "control-decision" "sol"
    typed-control "decision" (
        (typed-control-ledger $ledger)
        | append [
            "--key" $work_key
            "--review" $review_receipt
            "--sol" $sol
            "--verdict" $verdict
            "--evidence" $evidence
        ]
    )
}

# Releases one typed attempt for bounded retry.
# @class control-plane
def "main cutover control fail" [work_key: string, fence: string, --ledger: path = ""]: nothing -> record {
    require-tool "control-fail" "controller"
    let arguments = typed-control-ledger $ledger | append ["--key" $work_key "--fence" $fence]
    typed-control "fail" $arguments
}

# Cancels one active typed attempt.
# @class control-plane
def "main cutover control cancel" [work_key: string, fence: string, --ledger: path = ""]: nothing -> record {
    require-tool "control-cancel" "controller"
    let arguments = typed-control-ledger $ledger | append ["--key" $work_key "--fence" $fence]
    typed-control "cancel" $arguments
}

# Reaps expired typed attempts through the expiry projection.
# @class control-plane
def "main cutover control recover" [--ledger: path = ""]: nothing -> record {
    require-tool "control-recover" "controller"
    typed-control "recover" (typed-control-ledger $ledger)
}

# Invalidates direct dependents through the reverse dependency projection.
# @class control-plane
def "main cutover control invalidate" [work_key: string, --ledger: path = ""]: nothing -> record {
    require-tool "control-invalidate" "controller"
    let arguments = typed-control-ledger $ledger | append ["--key" $work_key]
    typed-control "invalidate" $arguments
}

# The commands below own bootstrap evidence custody. Keep their wire shapes
# stable until the versioned store imports their append-only records.
def candidate-index-tree [paths: list<any>] {
    let index = (^mktemp --tmpdir backend-index.XXXXXX | str trim)
    rm -f $index
    let result = (try {
        with-env {GIT_INDEX_FILE:$index} {
            ^git read-tree HEAD
            let added = (^git add --all -- ...$paths | complete)
            if $added.exit_code != 0 { error make {msg:"candidate-index-failed"} }
            ^git write-tree | str trim
        }
    } catch {|failure|
        rm -f $index
        error make {msg: ($failure.msg? | default "candidate-index-failed")}
    })
    rm -f $index
    $result
}

# Computes a domain-separated canonical BLAKE3 identifier for a JSON payload.
# @class control-plane
def "main cutover id" [domain: string, payload: string]: nothing -> string {
    require-tool "cell-id" "controller"
    id $domain ($payload | from json)
}
# Computes the checked content address for one complete agent work key.
# @class control-plane
def "main cutover work-key" [
    cell: string
    model_class: string
    effort: string
    toolchain_root: string
    input_digest: string
]: nothing -> string {
    require-tool "work-key" "controller"
    if ($cell | is-empty) or ($model_class | is-empty) or ($toolchain_root | is-empty) or ($input_digest | is-empty) { error make {msg: "invalid-work-key"} }
    if $effort not-in [
        "low"
        "medium"
        "high"
        "xhigh"
        "max"
        "ultra"
    ] { error make {msg: "invalid-work-key-effort"} }
    id "work-key" {
        cell: $cell
        model_class: $model_class
        effort: $effort
        toolchain_root: $toolchain_root
        input_digest: $input_digest
    }
}
# Acquires or coalesces an exclusive lease for one work key.
# @class control-plane
def "main cutover lease acquire" [
    work_key: string
    owner: string
    --ttl-ms: int = 30000
    --waiter-limit: int = 8
]: nothing -> record {
    require-tool "lease-acquire" "controller"
    let g = (acquire-dispatch-lock)
    try {
        let current = (latest-work $work_key)
        if not ($current | is-empty) and (($current.state? | default "") in ["held" "renewed"]) and (($current.expires_at? | default 0) > (now)) {
            if ($current.owner? | default "") == $owner {
                release-lock $g.owner
                return $current
            }
            let waiters = $current.waiters? | default []
            if ($waiters | length) >= $waiter_limit { error make {msg: "waiter-limit"} }
            let result = (
                append-event-locked {
                    event: "lease",
                    work_key: $work_key
                    owner: $current.owner
                    epoch: $current.epoch
                    fence: $current.fence
                    state: $current.state
                    expires_at: $current.expires_at
                    waiter_limit: $waiter_limit
                    waiters: ($waiters | append $owner)
                    base_tree: ($current.base_tree? | default (^git rev-parse HEAD^{tree} | str trim))
                } (head)
            )
            release-lock $g.owner
            return ($result | merge {fence: "", waiter: true})
        }
        let epoch = (
            if ($current | is-empty) { 1 } else { (($current.epoch? | default 0) + 1) }
        )
        let fence = (id "fence" {work_key: $work_key, owner: $owner, epoch: $epoch})
        let result = (
            append-event-locked {
                event: "lease",
                work_key: $work_key
                owner: $owner
                epoch: $epoch
                fence: $fence
                state: "held",
                expires_at: ((now) + ($ttl_ms * 1000000))
                waiter_limit: $waiter_limit
                waiters: []
                base_tree: (^git rev-parse HEAD^{tree} | str trim)
            } (head)
        )
        release-lock $g.owner
        $result
    } catch {|failure|
        try { release-lock $g.owner } catch { }
        error make {
            msg: ($failure.msg? | default "lease-acquire-failed")
        }
    }
}
# Renews the active lease while preserving its owner fence.
# @class control-plane
def "main cutover lease renew" [work_key: string, fence: string, --ttl-ms: int = 30000]: nothing -> record {
    require-tool "lease-renew" "controller"
    let l = (require-current-fence $work_key $fence)
    append-event (
        $l
        | merge {event:"lease", state:"renewed", expires_at:((now) + ($ttl_ms * 1000000))}
    ) (head)
}
def lease-terminal [work_key: string, fence: string, state: string] {
    let lease = (require-current-fence $work_key $fence)
    append-event {
        event: "lease"
        work_key: $work_key
        owner: $lease.owner
        epoch: $lease.epoch
        fence: $fence
        state: $state
        expires_at: $lease.expires_at
        waiter_limit: ($lease.waiter_limit? | default 0)
        waiters: ($lease.waiters? | default [])
    } (head)
}
# Freezes the active lease and rejects subsequent writes with its fence.
# @class control-plane
def "main cutover lease freeze" [work_key: string, fence: string]: nothing -> record {
    require-tool "lease-freeze" "controller"
    lease-terminal $work_key $fence "frozen"
}
# Marks the active lease expired for stale output recovery.
# @class control-plane
def "main cutover lease expire" [work_key: string, fence: string]: nothing -> record {
    require-tool "lease-expire" "controller"
    lease-terminal $work_key $fence "expired"
}
# Quarantines output associated with the active lease.
# @class control-plane
def "main cutover lease quarantine" [work_key: string, fence: string]: nothing -> record {
    require-tool "lease-quarantine" "controller"
    lease-terminal $work_key $fence "quarantined"
}
# Cancels the active lease and its pending work.
# @class control-plane
def "main cutover lease cancel" [work_key: string, fence: string]: nothing -> record {
    require-tool "lease-cancel" "controller"
    lease-terminal $work_key $fence "cancelled"
}
# Admits a controller candidate after Git scope and parent checks.
# @class control-plane
def "main cutover candidate admit" [
    work_key: string
    fence: string
    candidate_tree: string
    allowed_paths: string
    --cell: string = ""
    --parent-head: string = ""
    --contract-roots: string = "[]"
    --toolchain-root: string = "unknown"
    --model: string = "unknown"
    --controller-checks: string = "[]"
    --model-class: string = ""
    --effort: string = ""
    --input-digest: string = ""
]: nothing -> record {
    require-tool "candidate-admit" "controller"
    require-current-fence $work_key $fence | ignore
    let paths = normalized-paths ($allowed_paths | from json)
    let lease = (require-current-fence $work_key $fence)
    let pre_tree = $lease.base_tree? | default (^git rev-parse HEAD^{tree} | str trim)
    let changed = (cutover-changed-paths)
    let outside = $changed | where {|p| $p not-in $paths}
    if not ($outside | is-empty) { error make {msg: "out-of-scope-edit"} }
    let computed = (candidate-index-tree $paths)
    if $candidate_tree != $computed { error make {msg: "candidate-tree-mismatch"} }
    let post_tree = (^git rev-parse HEAD^{tree} | str trim)
    let current_parent = ^git rev-parse HEAD | str trim
    if ($parent_head | is-not-empty) and $parent_head != $current_parent { error make {msg: "candidate-parent-mismatch"} }
    let parent = if ($parent_head | is-empty) { $current_parent } else { $parent_head }
    if ($cell | is-empty) { error make {msg: "candidate-cell-required"} }
    if ($model_class | is-empty) or ($effort | is-empty) or ($input_digest | is-empty) or ($toolchain_root == "unknown") { error make {msg: "work-key-material-required"} }
    let material_key = (id "work-key" {
        cell: $cell
        model_class: $model_class
        effort: $effort
        toolchain_root: $toolchain_root
        input_digest: $input_digest
    })
    if $material_key != $work_key { error make {msg: "work-key-mismatch"} }
    let roots = $contract_roots | from json
    let effective_roots = if ($roots | is-empty) {
        [
            (role-contract)
        ]
    } else { $roots }
    let checks = $controller_checks | from json
    let effective_checks = if ($checks | is-empty) { ["scope" "candidate-tree"] } else { $checks }
    let candidate_cell = if ($cell | is-empty) { $work_key } else { $cell }
    require-current-fence $work_key $fence | ignore
    append-event {
        event: "candidate"
        work_key: $work_key
        fence: $fence
        candidate_tree: $candidate_tree
        computed_tree: $computed
        pre_tree: $pre_tree
        post_tree: $post_tree
        pre_paths: []
        post_paths: $changed
        allowed_paths: $paths
        digest_algorithm: (digest-algorithm)
        role_contract: (role-contract)
        authority: "controller"
        cell: $candidate_cell
        parent_head: $parent
        contract_roots: $effective_roots
        toolchain_root: $toolchain_root
        model_class: $model_class
        effort: $effort
        input_digest: $input_digest
        model: {id: $model}
        controller_checks: $effective_checks
    } (head)
}
# Admits an authority-bound immutable evidence receipt.
# @class control-plane
def "main cutover receipt admit" [
    authority: string
    work_key: string
    fence: string
    claim: string
    artifact_digests: string
]: nothing -> record {
    let allowed_authorities = (match (role) {
        "terra-academic" => ["controller"]
        "terra-reviewer" => ["evaluator"]
        "sol-integrator" => ["sol"]
        _ => []
    })
    if $authority not-in $allowed_authorities { error make {msg: "receipt-authority-mismatch"} }
    require-tool "evidence-admit" (if $authority == "evaluator" { "evaluator" } else { "sol" })
    require-current-fence $work_key $fence | ignore
    let artifacts = $artifact_digests | from json
    let parsed = (
        try {
            $claim | from json
        } catch { null }
    )
    let receipt_cell = if $parsed == null { "" } else {
        $parsed.cell? | default ""
    }
    if $authority in ["reviewer" "evaluator"] and $parsed != null {
        let candidate = (
            read-events
            | where {|e| ($e.event? | default "") == "candidate" and ($e.work_key? | default "") == $work_key}
            | last
        )
        let evaluation = (
            read-events
            | where {|e| ($e.event? | default "") == "evaluation" and ($e.evaluation_key? | default "") == ($parsed.evaluation_key? | default "") and ($e.candidate_tree? | default "") == ($parsed.candidate_tree? | default "")}
            | last
        )
        if ($candidate | is-empty) or ($parsed.candidate_receipt? | default "") != (id "event" $candidate) { error make {msg: "evaluation-receipt-binding"} }
        if ($evaluation | is-empty) or ($parsed.evaluator_root? | default "") != ($evaluation.evaluator_root? | default "") or ($parsed.holdout? | default "") != ($evaluation.holdout? | default "") or ($parsed.environment? | default "") != ($evaluation.environment? | default "") or ($parsed.result_root? | default "") != ($evaluation.result_root? | default "") { error make {msg: "evaluation-receipt-binding"} }
        if ($parsed.fence? | default "") != $fence or ($parsed.outcome? | default "") != ($evaluation.outcome? | default "") or ($parsed.outcome? | default "") not-in ["verified" "failed"] { error make {msg: "evaluation-receipt-binding"} }
        if ($parsed.artifact_digests? | default []) != ($evaluation.artifact_digests? | default []) or ($parsed.coverage? | default []) != ($evaluation.coverage? | default []) or ($parsed.holdout_evidence? | default null) != ($evaluation.holdout_evidence? | default null) or ($parsed.coverage_evidence? | default []) != ($evaluation.coverage_evidence? | default []) { error make {msg: "evaluation-receipt-binding"} }
    }
    append-event {
        event: "receipt"
        authority: $authority
        work_key: $work_key
        fence: $fence
        claim: $claim
        artifact_digests: $artifacts
        role_contract: (role-contract)
        candidate_tree: ($parsed.candidate_tree? | default "")
        evaluation_key: ($parsed.evaluation_key? | default "")
        candidate_receipt: ($parsed.candidate_receipt? | default "")
        evaluator_root: ($parsed.evaluator_root? | default "")
        result_root: ($parsed.result_root? | default "")
        holdout: ($parsed.holdout? | default "")
        holdout_evidence: ($parsed.holdout_evidence? | default null)
        environment: ($parsed.environment? | default "")
        outcome: ($parsed.outcome? | default "")
        coverage: ($parsed.coverage? | default [])
        coverage_evidence: ($parsed.coverage_evidence? | default [])
        cell: $receipt_cell
    } (head)
}
# Records or reuses an evaluator run for a frozen candidate.
# @class control-plane
def "main cutover evaluate" [
    evaluation_key: string
    candidate_tree: string
    evaluator_root: string
    holdout: string
    environment: string
    result_root: string
]: nothing -> record {
    require-tool "evaluation"
    if ($result_root | is-empty) { error make {msg: "evaluation-result-unbound"} }
    let candidate = read-events | where {|e| ($e.event? | default "") == "candidate" and ($e.candidate_tree? | default "") == $candidate_tree} | last
    if ($candidate | is-empty) { error make {msg: "evaluation-candidate-unbound"} }
    let contracts = (control-plane).evaluation.contracts? | default {}
    let contract = (
        try {
            $contracts | get $evaluation_key
        } catch { null }
    )
    if $contract == null { error make {msg: "evaluation-contract-unbound"} }
    if ($contract.cell? | default "") != ($candidate.cell? | default "") or ($contract.evaluator_root? | default "") != $evaluator_root or ($contract.holdout? | default "") != $holdout or ($contract.environment_id? | default "") != $environment { error make {msg: "evaluation-key-mismatch"} }
    let command = $contract.command? | default ""
    let cwd = $contract.cwd? | default ""
    let wall_ms = $contract.wall_ms? | default 0
    let stdout_limit = $contract.stdout_bytes? | default 0
    let stderr_limit = $contract.stderr_bytes? | default 0
    let expected_coverage = $contract.coverage? | default []
    let holdout_command = $contract.holdout_command? | default ""
    if ($command | is-empty) or ($cwd not-in ["repository" "current-repo"]) or ($wall_ms <= 0) or ($stdout_limit <= 0) or ($stderr_limit <= 0) or ($expected_coverage | is-empty) or ($holdout_command | is-empty) { error make {msg: "evaluation-contract-invalid"} }
    if ($expected_coverage | uniq | length) != ($expected_coverage | length) { error make {msg: "evaluation-coverage-duplicate"} }
    let contract_environment = $contract.environment? | default {}
    let allowed_environment = ["CARGO_TARGET_DIR" "RUST_BACKTRACE" "RUSTFLAGS"]
    let forbidden_environment = $contract_environment | columns | where {|name| $name not-in $allowed_environment}
    if ($forbidden_environment | is-not-empty) { error make {msg: "evaluation-environment-forbidden"} }
    let timeout = $"($wall_ms / 1000)s"
    let run = with-env $contract_environment { process-result "timeout" ["--signal=KILL", $timeout, "sh", "-c", $command] }
    if $run.stdout_bytes > $stdout_limit { error make {msg: "evaluation-stdout-overflow"} }
    if $run.stderr_bytes > $stderr_limit { error make {msg: "evaluation-stderr-overflow"} }
    let execution = {exit_code: $run.status, stdout: $run.stdout, stderr: $run.stderr}
    let observed_root = (id "evaluation-result" $execution)
    if $result_root != $observed_root { error make {msg: "evaluation-result-mismatch"} }
    let outcome = if $run.status == 0 { "verified" } else { "failed" }
    let artifact_digests = [
        (
            id "evaluation-artifact" {result_root: $observed_root, stdout: $run.stdout, stderr: $run.stderr}
        )
    ]
    # The holdout oracle is an independent bounded process invocation.  Its
    # exact output is bound into the evaluation evidence instead of allowing
    # a caller to copy arbitrary coverage labels from the policy contract.
    let holdout_run = with-env $contract_environment { process-result "timeout" ["--signal=KILL", $timeout, "sh", "-c", $holdout_command] }
    if $holdout_run.stdout_bytes > $stdout_limit { error make {msg: "evaluation-holdout-stdout-overflow"} }
    if $holdout_run.stderr_bytes > $stderr_limit { error make {msg: "evaluation-holdout-stderr-overflow"} }
    if $holdout_run.status != 0 { error make {msg: "evaluation-holdout-failed"} }
    if ($holdout_run.stdout | str trim) != $holdout { error make {msg: "evaluation-holdout-mismatch"} }
    let holdout_evidence = {
        digest: (id "evaluation-holdout" {
            evaluation_key: $evaluation_key
            candidate_tree: $candidate_tree
            evaluator_root: $evaluator_root
            holdout: $holdout
            environment: $environment
            status: $holdout_run.status
            stdout: $holdout_run.stdout
            stderr: $holdout_run.stderr
        })
        status: $holdout_run.status
        stdout: $holdout_run.stdout
        stderr: $holdout_run.stderr
    }
    let observed_coverage = ["declared-command" "result-root" "holdout-oracle"]
    let unproven_coverage = $expected_coverage | where {|item| $item not-in $observed_coverage}
    if ($unproven_coverage | is-not-empty) { error make {msg: "evaluation-coverage-unproven"} }
    let coverage = $observed_coverage | where {|item| $item in $expected_coverage}
    let coverage_evidence = $coverage | each {|item|
        {
            claim: $item
            digest: (id "evaluation-coverage" {
                claim: $item
                evaluation_key: $evaluation_key
                candidate_tree: $candidate_tree
                result_root: $observed_root
                holdout_evidence: $holdout_evidence.digest
            })
            holdout_evidence: $holdout_evidence.digest
            result_root: $observed_root
        }
    }
    let prior = (
        read-events
        | where {|e| ($e.event? | default "") == "evaluation" and ($e.evaluation_key? | default "") == $evaluation_key and ($e.candidate_tree? | default "") == $candidate_tree and ($e.evaluator_root? | default "") == $evaluator_root and ($e.environment? | default "") == $environment and ($e.holdout? | default "") == $holdout and ($e.result_root? | default "") == $result_root and ($e.outcome? | default "") in ["verified" "failed"]}
        | last
    )
    if not ($prior | is-empty) { return ($prior | merge {reused:true}) }
    append-event {
        event: "evaluation"
        evaluation_key: $evaluation_key
        candidate_tree: $candidate_tree
        evaluator_root: $evaluator_root
        holdout: $holdout
        environment: $environment
        result_root: $result_root
        reused: false
        outcome: $outcome
        execution: $execution
        artifact_digests: $artifact_digests
        coverage: $coverage
        holdout_evidence: $holdout_evidence
        coverage_evidence: $coverage_evidence
        authority: "evaluator"
        role_contract: (role-contract)
        cell: ($candidate.cell? | default "")
    } (head)
}
# Promotes a fully evidenced candidate with protected-ref compare-and-swap.
# @class control-plane
def "main cutover decide" [
    candidate_receipt: string
    evaluation_receipts: string
    expected_head: string
    accepted_head: string
    fence: string
    protected_ref: string
    --decision: string = "verified"
]: nothing -> record {
    require-tool "decision"
    if $decision not-in ["verified" "rejected"] { error make {msg: "invalid-decision"} }
    let approved_refs = $env.BACKEND_PROTECTED_REFS? | default '["refs/heads/protected"]' | from json
    if $protected_ref not-in $approved_refs { error make {msg: "protected-ref-not-approved"} }
    let candidate = (
        read-events
        | where {|e| ($e.event? | default "") == "candidate" and (id "event" $e) == $candidate_receipt}
        | last
    )
    if ($candidate | is-empty) { error make {msg: "candidate-receipt-unbound"} }
    if ($candidate.superseded_by? | default "") != "" { error make {msg: "candidate-superseded"} }
    if ($candidate.cell? | default "") == "" or ($candidate.candidate_tree? | default "") == "" or ($candidate.parent_head? | default "") == "" or ($candidate.contract_roots? | default [] | is-empty) or ($candidate.toolchain_root? | default "") == "" or ($candidate.role_contract? | default "") == "" or ($candidate.model? | default {} | is-empty) or ($candidate.controller_checks? | default [] | is-empty) { error make {msg: "candidate-receipt-incomplete"} }
    let candidate_work_key = $candidate.work_key? | default ""
    if ($candidate_work_key | is-empty) { error make {msg: "candidate-work-unbound"} }
    let active = (require-current-fence $candidate_work_key $fence)
    if ($active.work_key? | default "") != $candidate_work_key { error make {msg: "decision-fence-unbound"} }
    if ($candidate.fence? | default "") != $fence { error make {msg: "decision-fence-unbound"} }
    let receipt_ids = $evaluation_receipts | from json
    if ($receipt_ids | is-empty) { error make {msg: "evaluation-receipt-incomplete"} }
    if ($receipt_ids | uniq | length) != ($receipt_ids | length) { error make {msg: "evaluation-receipt-duplicate"} }
    for receipt_id in $receipt_ids {
        let receipt = (
            read-events
            | where {|e| ($e.event? | default "") == "receipt" and (id "event" $e) == $receipt_id}
            | last
        )
        if ($receipt | is-empty) { error make {msg: "evaluation-receipt-unbound"} }
        if ($receipt.superseded_by? | default "") != "" { error make {msg: "evaluation-receipt-superseded"} }
        if ($receipt.candidate_tree? | default "") != ($candidate.candidate_tree? | default "") { error make {msg: "evaluation-receipt-binding"} }
        if ($receipt.authority? | default "") not-in ["evaluator" "reviewer"] or ($receipt.candidate_receipt? | default "") != $candidate_receipt { error make {msg: "evaluation-receipt-binding"} }
        if ($receipt.cell? | default "") != ($candidate.cell? | default "") { error make {msg: "evaluation-receipt-binding"} }
        if ($receipt.evaluation_key? | default "") == "" { error make {msg: "evaluation-receipt-incomplete"} }
        let evaluation = (
            read-events
            | where {|e| ($e.event? | default "") == "evaluation" and ($e.evaluation_key? | default "") == $receipt.evaluation_key and ($e.candidate_tree? | default "") == ($candidate.candidate_tree? | default "")}
            | last
        )
        if ($evaluation | is-empty) { error make {msg: "evaluation-receipt-unbound"} }
        if ($receipt.evaluator_root? | default "") != ($evaluation.evaluator_root? | default "") or ($receipt.holdout? | default "") != ($evaluation.holdout? | default "") or ($receipt.environment? | default "") != ($evaluation.environment? | default "") or ($receipt.result_root? | default "") != ($evaluation.result_root? | default "") or ($receipt.artifact_digests? | default []) != ($evaluation.artifact_digests? | default []) or ($receipt.coverage? | default []) != ($evaluation.coverage? | default []) or ($receipt.holdout_evidence? | default null) != ($evaluation.holdout_evidence? | default null) or ($receipt.coverage_evidence? | default []) != ($evaluation.coverage_evidence? | default []) { error make {msg: "evaluation-receipt-binding"} }
        if ($evaluation.superseded_by? | default "") != "" { error make {msg: "evaluation-superseded"} }
        if ($receipt.outcome? | default "") != "verified" or ($receipt.coverage? | default [] | is-empty) or ($receipt.artifact_digests? | default [] | is-empty) { error make {msg: "evaluation-receipt-incomplete"} }
    }
    if $decision == "rejected" {
        return (
        append-event {
            event: "decision",
            phase: "committed",
            authority: "sol",
            decision: $decision,
            candidate_receipt: $candidate_receipt
            evaluation_receipts: $receipt_ids
            expected_head: $expected_head
            accepted_head: $accepted_head
            protected_ref: $protected_ref
            fence: $fence
            cell: ($candidate.cell? | default "")
        } (head)
        )
    }
    let accepted_tree = (^git rev-parse $"($accepted_head)^{tree}" | str trim)
    if $accepted_tree != ($candidate.candidate_tree? | default "") { error make {msg: "decision-tree-mismatch"} }
    let ancestor = (^git merge-base $candidate.parent_head $accepted_head | str trim)
    if $ancestor != $candidate.parent_head { error make {msg: "decision-parent-mismatch"} }
    let current = (^git rev-parse $protected_ref | str trim)
    let matching_prepared = (
        read-events
        | where {|e| ($e.event? | default "") == "decision" and ($e.phase? | default "") == "prepared" and ($e.candidate_receipt? | default "") == $candidate_receipt and ($e.evaluation_receipts? | default []) == $receipt_ids and ($e.expected_head? | default "") == $expected_head and ($e.accepted_head? | default "") == $accepted_head and ($e.protected_ref? | default "") == $protected_ref and ($e.fence? | default "") == $fence}
        | last
    )
    let matching_committed = (
        read-events
        | where {|e| ($e.event? | default "") == "decision" and ($e.phase? | default "") == "committed" and ($e.candidate_receipt? | default "") == $candidate_receipt and ($e.evaluation_receipts? | default []) == $receipt_ids and ($e.expected_head? | default "") == $expected_head and ($e.accepted_head? | default "") == $accepted_head and ($e.protected_ref? | default "") == $protected_ref and ($e.fence? | default "") == $fence}
        | last
    )
    if not ($matching_prepared | is-empty) {
        if $current == $accepted_head {
            if not ($matching_committed | is-empty) { return $matching_committed }
            cutover-fault "decision-before-committed"
            let committed = (append-event {
                event: "decision",
                phase: "committed",
                authority: "sol",
                decision: $decision
                candidate_receipt: $candidate_receipt
                evaluation_receipts: $receipt_ids
                expected_head: $expected_head
                accepted_head: $accepted_head
                protected_ref: $protected_ref
                fence: $fence
                prepared_receipt: (id "event" $matching_prepared)
                cell: ($candidate.cell? | default "")
            } (head))
            cutover-fault "decision-after-committed"
            return $committed
        }
        if $current != $expected_head { error make {msg: "decision-reconciliation-conflict"} }
    } else if $current != $expected_head {
        error make {msg: "decision-cas-precondition"}
    }
    # Persist the intended promotion before touching Git. If the process dies
    # after update-ref, this prepared record is the durable reconciliation key.
    let prepared = if ($matching_prepared | is-empty) {
        cutover-fault "decision-before-prepared"
        let record = (append-event {
            event: "decision",
            phase: "prepared",
            authority: "sol",
            decision: $decision
            candidate_receipt: $candidate_receipt
            evaluation_receipts: $receipt_ids
            expected_head: $expected_head
            accepted_head: $accepted_head
            protected_ref: $protected_ref
            fence: $fence
            cell: ($candidate.cell? | default "")
        } (head))
        cutover-fault "decision-after-prepared"
        $record
    } else { $matching_prepared }
    let current = (^git rev-parse $protected_ref | str trim)
    if $current != $expected_head { error make {msg: "decision-cas-precondition"} }
    cutover-fault "decision-before-update-ref"
    let result = (^git update-ref $protected_ref $accepted_head $expected_head | complete)
    if $result.exit_code != 0 { error make {msg: "protected-ref-cas-failed"} }
    cutover-fault "decision-after-update-ref"
    cutover-fault "decision-before-committed"
    let committed = (append-event {
        event: "decision",
        phase: "committed",
        authority: "sol",
        decision: $decision
        candidate_receipt: $candidate_receipt
        evaluation_receipts: $receipt_ids
        expected_head: $expected_head
        accepted_head: $accepted_head
        protected_ref: $protected_ref
        fence: $fence
        prepared_receipt: $prepared.id
        cell: ($candidate.cell? | default "")
    } (head))
    cutover-fault "decision-after-committed"
    $committed
}
# Returns acknowledged, supersession-filtered evidence for one cell.
# @class control-plane
def "main cutover context delta" [
    consumer: string
    cell: string
    --since-root: string = ""
    --acknowledged-evidence: string = "[]"
]: nothing -> record {
    require-tool "context-delta"
    if ($cell | is-empty) { error make {msg: "context-cell-required"} }
    let acknowledged = $acknowledged_evidence | from json
    if not (($acknowledged | describe | str starts-with "list")) { error make {msg: "context-cursor-invalid"} }
    let events = (
        read-events
            | where {|e| ($e.event? | default "") in ["receipt" "candidate" "evaluation" "decision"] and (($e.cell? | default "") == $cell)}
    )
    mut superseded = []
    for e in $events {
        let links = $e.supersedes? | default []
        let ids = if ($links | describe | str starts-with "list") { $links } else if ($links | is-empty) { [] } else { [$links] }
        $superseded = ($superseded | append $ids)
    }
    let active_events = (
        $events
        | where {|e| ($e.superseded_by? | default "") == "" and (id "event" $e) not-in $superseded}
    )
    let known_ids = $events | each {|e| id "event" $e}
    let all_ids = $active_events | each {|e| id "event" $e}
    let current_root = (id "context" {consumer: $consumer, cell: $cell, facts: $active_events})
    if ($acknowledged | any {|e| $e not-in $known_ids}) { error make {msg: "context-acknowledgement-unbound"} }
    if ($since_root | is-not-empty) {
        # The cursor root describes the facts the consumer acknowledged at
        # the previous read.  Reconstruct that exact prefix from immutable
        # event identities, including facts that were later superseded, so a
        # changed graph can return only its delta while a forged root still
        # fails closed.
        let prior_events = $events | where {|e| (id "event" $e) in $acknowledged}
        let prior_root = (id "context" {consumer: $consumer, cell: $cell, facts: $prior_events})
        if $since_root != $prior_root { error make {msg: "context-cursor-mismatch"} }
    }
    let facts = if ($since_root | is-not-empty) and $since_root == $current_root {
        []
    } else {
        $active_events | enumerate | where {|row| (($all_ids | get $row.index) not-in $acknowledged)} | get item
    }
    let new_root = (id "context" {consumer: $consumer, cell: $cell, facts: $active_events})
    {
        cell: $cell
        cursor: {
            consumer: $consumer
            context_root: $new_root
            # The returned cursor acknowledges the complete active projection,
            # including facts delivered by this delta.  Retaining the input
            # acknowledgement set would make the next since-root request
            # fail or replay newly delivered evidence.
            acknowledged_evidence: $all_ids
            at: (now)
        }
        facts: $facts
        new_context_root: $new_root
        omitted_artifacts: []
    }
}
# Verifies the pinned control-plane digest implementation and schema presence.
# @class control-plane
def "main cutover self-test" []: nothing -> record {
    let schema = configuration-root | path join "contracts/schema.json"
    if not ($schema | path exists) { error make {msg: "missing-canonical-schema"} }
    let vector = (blake3-bytes (frame "backend.control-plane.v1" "abc"))
    if $vector != "723e08299e6efde568e8d58b65106b66d19d388d971265bd7c9de723ba371162" { error make {msg: "blake3-self-test-failed"} }
    {
        schema_version: (open $schema).schema_version
        digest: (digest-algorithm)
        algorithm_vector: $vector
        ledger: (cutover-root)
        status: "ready"
        no_tests_semantics: "--no-tests=fail rejects an empty selection"
    }
}
