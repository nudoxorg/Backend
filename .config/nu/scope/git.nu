# Computes bounded change sets directly from Git's index and worktree facts.
# Treats staged, unstaged, committed-range, and untracked paths uniformly.
# Produces stable path selections reused by format, lint, test, and observation.

# Returns the comparison base for the current branch without requiring an upstream.
def comparison-base []: nothing -> string {
  let upstream = (process-result "git" ["rev-parse" "--abbrev-ref" "--symbolic-full-name" "@{upstream}"])
  if $upstream.status == 0 {
    process-require "git" ["merge-base" "HEAD" ($upstream.stdout | str trim)] | get stdout | str trim
  } else {
    process-require "git" ["rev-parse" "HEAD^"] | get stdout | str trim
  }
}

# Returns unique repository-relative paths changed across every local Git surface.
def changed-paths [--base: string]: nothing -> list<string> {
  let selected_base = if ($base | is-empty) { comparison-base } else { $base }
  let committed = (process-require "git" ["diff" "-z" "--name-only" "--diff-filter=ACMRTUXB" $"($selected_base)..HEAD"] | get stdout | split row (char nul))
  let staged = (process-require "git" ["diff" "-z" "--cached" "--name-only" "--diff-filter=ACMRTUXB"] | get stdout | split row (char nul))
  let working = (process-require "git" ["diff" "-z" "--name-only" "--diff-filter=ACMRTUXB"] | get stdout | split row (char nul))
  let untracked = (process-require "git" ["ls-files" "-z" "--others" "--exclude-standard"] | get stdout | split row (char nul))
  $committed | append $staged | append $working | append $untracked | where {|path| not ($path | is-empty) } | uniq | sort
}

# Returns changed Rust source paths that still exist in the worktree.
def changed-rust-paths [--base: string]: nothing -> list<string> {
  changed-paths --base $base
  | where {|path| ($path | str ends-with ".rs") and ($path | path exists) }
}

# Hashes the exact changed-file selection for durable local evidence names.
def change-fingerprint [--base: string]: nothing -> string {
  changed-paths --base $base | str join "\n" | hash sha256
}
