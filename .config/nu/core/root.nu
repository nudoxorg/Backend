# Discovers and validates the repository's runtime ownership boundaries.
# Keeps generated state under `.local` and configuration under `.config`.
# Rejects commands launched outside the Git worktree they intend to mutate.

# Returns the canonical repository root containing the current working directory.
def repository-root []: nothing -> path {
  let result = (^git rev-parse --show-toplevel | complete)
  if $result.exit_code != 0 {
    tooling-fail "outside-repository" "the current directory is not inside a Git worktree" "change into the backend repository"
  }
  $result.stdout | str trim | path expand
}

# Returns the absolute centrally owned tool configuration directory.
def configuration-root []: nothing -> path {
  if ($env.BACKEND_CONFIG? | is-not-empty) {
    $env.BACKEND_CONFIG | path expand
  } else {
    repository-root | path join ".config"
  }
}

# Creates and returns the ignored local artifact directory.
def local-root []: nothing -> path {
  let directory = (repository-root | path join ".local")
  mkdir $directory
  $directory
}

# Resolves a relative path and proves that it remains beneath the repository root.
def owned-path [candidate: path]: nothing -> path {
  if ($candidate | path type) == "absolute" {
    tooling-fail "absolute-path" $"expected a repository-relative path, observed ($candidate)" "remove the leading root component"
  }
  let root = (repository-root)
  let resolved = ($root | path join $candidate | path expand)
  if not ($resolved | str starts-with $"($root)/") {
    tooling-fail "path-escape" $"($candidate) escapes the repository" "choose a path beneath the current repository"
  }
  $resolved
}

# Returns the path relative to the repository without accepting external paths.
def relative-owned-path [candidate: path]: nothing -> path {
  let root = (repository-root)
  let resolved = ($candidate | path expand)
  if not (($resolved == $root) or ($resolved | str starts-with $"($root)/")) {
    tooling-fail "path-escape" $"($candidate) is outside ($root)"
  }
  $resolved | path relative-to $root
}
