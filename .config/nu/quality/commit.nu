# Guards frequent commits with staged scope, formatting, lint, and Koji policy.
# Lets Koji infer conventional scope only after deterministic checks succeed.
# Refuses empty staging and never stages files on the caller's behalf.

# Validates staged work and hands message composition to the pinned Koji tool.
def "main commit" []: nothing -> nothing {
  let staged = (process-require "git" ["diff" "--cached" "--name-only" "--diff-filter=ACMRTUXB"] | get stdout | lines | where {|path| not ($path | is-empty) })
  if ($staged | is-empty) {
    tooling-fail "empty-index" "no staged files are available for a commit"
  }
  let findings = (lint-structural ($staged | where {|path| $path | str ends-with ".rs" }))
  if not ($findings | is-empty) {
    $findings | to json --indent 2 | print
    tooling-fail "staged-lint" "staged source violates structural policy"
  }
  process-require "koji" ["--config" (configuration-root | path join "koji.toml")] | ignore
}
