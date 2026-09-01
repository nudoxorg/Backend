# Loads the immutable Nix-owned command, lint, role, and evaluation declaration.
# Rejects ambient or missing policy paths before any capability is interpreted.
# Gives Nushell one structured source while retaining live-command verification.

# Returns the complete Nix-materialized control-plane record.
def control-plane []: nothing -> record<formatting: record, roles: record, lint: record, rubric: record, evaluation: record> {
    let source = $env.BACKEND_CONTROL_PLANE? | default ""
    if ($source | is-empty) {
        tooling-fail "missing-control-plane" "BACKEND_CONTROL_PLANE is not set" "enter through the pinned Nix application or shell"
    }
    if not ($source | path exists) {
        tooling-fail "missing-control-plane" $"declared control plane does not exist: ($source)"
    }
    open $source
}

# Returns declared roles as stable identifier-bearing rows.
def role-declarations []: nothing -> table {
    control-plane
    | get roles
    | transpose id contract
    | each {|row|
      $row.contract
      | merge {
          id: $row.id
          allowed: ($row.contract.tools | values | uniq)
        }
    }
    | sort-by id
}

# Returns the complete structured lint registry.
def lint-declarations []: nothing -> table {
    control-plane | get lint.rules
}
