# Tests the tooling's own privilege, rule, fixture, and snapshot contracts.
# Uses deterministic file joins and role matrices rather than product code.
# Fails whenever a policy can exist without an executable adversarial example.

use std/assert

let config = ($env.BACKEND_CONFIG? | default ($env.FILE_PWD | path dirname))
let rules = (glob ($config | path join "ast-grep/rules/rust/*.yml") | each {|path| $path | path parse | get stem } | sort)
let tests = (glob ($config | path join "ast-grep/tests/*.yml") | each {|path| $path | path parse | get stem } | sort)
let snapshots = (glob ($config | path join "ast-grep/tests/snapshots/*-snapshot.yml") | each {|path| $path | path parse | get stem | str replace "-snapshot" "" } | sort)

assert equal $rules $tests "every ast-grep rule must own valid and invalid fixtures"
assert equal $rules $snapshots "every ast-grep rule must own a frozen diagnostic snapshot"

let policy = (open ($config | path join "lint/policy.nuon"))
let ast_policy = ($policy | where engine == "ast-grep" | get id | sort)
assert equal $rules $ast_policy "every ast-grep rule must have exactly one policy owner"

let roles = (glob ($config | path join "agents/roles/*.nuon") | each {|path| open $path })
let luna = ($roles | where id == "luna-pair" | first)
let academic = ($roles | where id == "terra-academic" | first)
assert (not ($luna.allowed_commands | any {|command| $command | str starts-with "main observe" })) "Luna must not receive measurement commands"
assert (not ($academic.allowed_commands | any {|command| $command | str starts-with "main observe" })) "Terra academic must not receive measurement commands"
assert ($luna.handoff | str contains "`terra-academic`") "Luna red work must use the stable Terra academic role id before review"

let capability_snapshot = (open ($config | path join "agents/snapshots/role-capabilities.nuon"))
for role in $roles {
  let expected = ($capability_snapshot | get $role.id | get commands | sort)
  assert equal ($role.allowed_commands | sort) $expected $"role ($role.id) commands must match the reviewed capability snapshot"
}
