# Guards frequent commits with staged scope, formatting, lint, and Koji policy.
# Lets Koji infer conventional scope only after deterministic checks succeed.
# Refuses empty staging and never stages files on the caller's behalf.

# Returns the closed conventional commit types admitted by repository policy.
def commit-types []: nothing -> list<string> {
    [
        "feat"
        "fix"
        "refactor"
        "perf"
        "test"
        "docs"
        "bench"
        "build"
        "ci"
        "style"
        "chore"
        "revert"
    ]
}

# Returns worktree-aware Git environment so child git processes resolve the
# shared object database and the correct worktree index from any checkout.
def commit-git-environment []: nothing -> record {
    if ((repository-root | path join ".git") | path type) == "file" {
        let common = (
            process-require "git" ["rev-parse" "--path-format=absolute" "--git-common-dir"]
            | get stdout
            | str trim
        )
        let index = (
            (
                process-require "git" ["rev-parse" "--path-format=absolute" "--git-dir"]
                | get stdout
                | str trim
            ) | path join "index"
        )
        {
            GIT_DIR: $common
            GIT_INDEX_FILE: $index
            GIT_WORK_TREE: (repository-root)
        }
    } else {
        {}
    }
}

# Returns the single declared scope owning every staged path.
def staged-scope [staged: list<string>]: nothing -> string {
    let scopes = open $env.BACKEND_KOJI_CONFIG | get commit_scopes
    let owning = $staged | each {|path|
        let anchored = $"/($path)"
        let matches = $scopes | where {|scope| ($scope.patterns | any {|pattern| $anchored =~ $pattern }) }
        if ($matches | length) != 1 {
            tooling-fail "commit-scope" $"staged path ($path) resolves to ($matches | length) declared scopes" "split the commit along the declared scope boundaries"
        }
        $matches | first | get name
    } | uniq
    if ($owning | length) != 1 {
        tooling-fail "commit-scope" $"staged paths span ($owning | length) scopes: ($owning | str join ', ')" "commit each scope boundary separately"
    }
    $owning | first
}

# Validates staged work and commits through Koji policy.
# @class repository-write
def "main commit" [--message: string]: nothing -> nothing {
    require-command "commit"
    let staged = (
        process-require "git" ["diff" "--cached" "--name-only" "--diff-filter=ACMRTUXB"]
        | get stdout
        | lines
        | where {|path| not ($path | is-empty) }
    )
    if ($staged | is-empty) {
        tooling-fail "empty-index" "no staged files are available for a commit"
    }
    let findings = lint-structural ($staged | where {|path| $path | str ends-with ".rs" })
    if not ($findings | is-empty) {
        $findings | to json --indent 2 | print
        tooling-fail "staged-lint" "staged source violates structural policy"
    }
    let environment = (commit-git-environment)
    if ($message | is-empty) {
        with-env $environment {
            process-require "koji" ["--config" $env.BACKEND_KOJI_CONFIG] | ignore
        }
        return
    }
    let scope = (staged-scope $staged)
    let admitted_types = (commit-types) | str join "|"
    let shape = $"^($admitted_types)\\(($scope)\\): .+$"
    if $message !~ $shape {
        tooling-fail "commit-message" $"message must match ($shape)" "the type set is closed and the scope is derived from the staged paths"
    }
    with-env $environment {
        process-require "git" ["commit" "--no-verify" "-m" $message] | ignore
    }
}
