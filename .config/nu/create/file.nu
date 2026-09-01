# Creates repository files through a validated purpose and ownership contract.
# Converts a bounded purpose into the required language-appropriate file header.
# Refuses overwrite, traversal, vague scope, and unsupported file categories.

# Splits prose into nonempty sentence-like units for purpose validation.
def purpose-sentences [purpose: string]: nothing -> list<string> {
    $purpose
    | str trim
    | split row --regex '(?<=[.!?])\s+'
    | each {|sentence| $sentence | str trim }
    | where {|sentence| not ($sentence | is-empty) }
}

# Validates a purpose against an exact sentence count and maximum byte budget.
def validated-purpose [purpose: string, sentence_count: int, maximum_bytes: int]: nothing -> list<string> {
    let sentences = (purpose-sentences $purpose)
    if ($sentences | length) != $sentence_count {
        tooling-fail "invalid-purpose" $"purpose must contain exactly ($sentence_count) concise sentences"
    }
    if ($purpose | encode utf-8 | length) > $maximum_bytes {
        tooling-fail "purpose-too-large" $"purpose exceeds the ($maximum_bytes)-byte limit"
    }
    $sentences
}

# Formats the mandatory opening documentation for a supported file extension.
def purpose-header [path: path, sentences: list<string>]: nothing -> string {
    let extension = $path | path parse | get extension
    let prefix = match $extension {
        rs => "//!"
        "nu" | "nix" | "toml" | "yml" | "yaml" => "#"
        _ => { tooling-fail "unsupported-file" $"cannot derive a documented header for .($extension)" }
    }
    $sentences | each {|sentence| $"($prefix) ($sentence)" } | str join "\n"
}

# Creates one relative file and writes its validated purpose as the opening header.
# @class repository-write
def "main create file" [path: path, --purpose: string]: nothing -> record {
    require-command "create-file"
    let destination = (owned-path $path)
    if ($destination | path exists) {
        tooling-fail "path-exists" $"refusing to overwrite ($path)"
    }
    let sentences = (validated-purpose $purpose 3 360)
    let transaction = local-root | path join "transactions" $"file-(random uuid)"
    mkdir ($transaction | path dirname)
    let header = (purpose-header $path $sentences)
    $"($header)\n\n" | save --raw $transaction
    mkdir ($destination | path dirname)
    mv --no-clobber $transaction $destination
    {
        path: (relative-owned-path $destination)
        purpose: ($sentences | str join " ")
    }
}
