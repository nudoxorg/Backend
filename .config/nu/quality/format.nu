# Formats only files and packages selected by the current Git change set.
# Dispatches each language to its pinned formatter with central configuration.
# Refuses to turn a narrow edit into an unbounded workspace rewrite.

# Formats changed Rust packages and changed Nix or Nushell configuration files.
# @class repository-write
def "main format changed" [--base: string, --check]: nothing -> record {
    require-command "format-changed"
    let root = (repository-root)
    let packages = (changed-packages --base $base)
    let rustfmt_arguments = if $check {
        [
            "fmt"
            "--check"
            "--manifest-path"
            ($root | path join "Cargo.toml")
        ]
    } else {
        [
            "fmt"
            "--manifest-path"
            ($root | path join "Cargo.toml")
        ]
    }
    for package in $packages {
        process-require $env.BACKEND_STABLE_CARGO (
            $rustfmt_arguments
            | append [
                "--package" $package.name
                "--"
                "--config-path"
                ($root | path join ".config/rustfmt.toml")
            ]
        ) | ignore
    }
    let extensions = (
        control-plane
        | get formatting
        | values
        | get extensions
        | flatten
        | uniq
    )
    let configuration_paths = (changed-paths --base $base | where {|path|
    ($path | path exists) and (($path | path parse | get extension) in $extensions) and not ($path | str ends-with ".rs")
  })
    let declarations = control-plane | get formatting
    for path in $configuration_paths {
        let extension = $path | path parse | get extension
        let matches = $declarations | transpose name declaration | where {|row| $extension in $row.declaration.extensions }
        if ($matches | length) != 1 {
            tooling-fail "formatter-assignment" $"changed configuration path ($path) resolves to ($matches | length) declared formatters"
        }
        let declaration = $matches | first | get declaration
        let mode_arguments = if $check { $declaration.check } else { $declaration.write }
        process-require $declaration.program ($mode_arguments | append ($root | path join $path)) | ignore
    }
    {
        packages: ($packages | get name)
        configuration: $configuration_paths
        mode: (if $check { "check" } else { "write" })
    }
}
