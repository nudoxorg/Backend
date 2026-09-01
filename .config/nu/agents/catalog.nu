# Extracts public CLI metadata from comments immediately above Nu definitions.
# Derives stable identifiers and privilege classes beside the executable signature.
# Rejects undocumented commands and detached metadata before skill generation.

# Returns documented public command definitions from every owned Nu source file.
def command-documentation []: nothing -> table {
    let source_root = configuration-root | path join "nu"
    glob ($source_root | path join "**/*.nu")
    | each {|path|
      let lines = open --raw $path | lines
      $lines
      | enumerate
      | where {|row| ($row.item | str trim) =~ '^def ("main( [^"]+)?"|main)\s*\[' }
      | each {|definition|
          let line = $definition.item | str trim
          let name = if ($line | str starts-with 'def "') {
            $line | split row '"' | get 1
          } else {
            "main"
          }
          mut cursor = ($definition.index - 1)
          mut comments = []
          while $cursor >= 0 {
            let candidate = $lines | get $cursor | str trim
            if not ($candidate | str starts-with "# ") { break }
            $comments = ($comments | prepend ($candidate | str substring 2..))
            $cursor = $cursor - 1
          }
          if ($comments | is-empty) {
            tooling-fail "undocumented-command" $"($name) lacks an immediately preceding Nu comment" $"add concise # documentation above ($path):($definition.index + 1)"
          }
          let class_lines = $comments | where {|comment| $comment | str starts-with "@class " }
          if ($class_lines | length) != 1 {
            tooling-fail "command-class-drift" $"($name) needs exactly one # @class annotation above ($path):($definition.index + 1)"
          }
          let prose = $comments | where {|comment| not ($comment | str starts-with "@") }
          if ($prose | is-empty) {
            tooling-fail "undocumented-command" $"($name) lacks concise prose above ($path):($definition.index + 1)"
          }
          {
            nu: $name
            id: ($name | str replace --regex '^main ?' '' | str replace --all ' ' '-' | default "catalog")
            cli: ($name | str replace --regex '^main' 'backend')
            class: ($class_lines | first | str replace '@class ' '')
            summary: ($prose | first)
            documentation: ($prose | str join " ")
            source: $".config/(($path | path relative-to $source_root))"
            line: ($definition.index + 1)
          }
        }
    }
    | flatten
    | sort-by nu
}
