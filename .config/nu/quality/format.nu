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
    let formatter_paths = $configuration_paths | each {|path| $root | path join $path }
    if not ($formatter_paths | is-empty) {
        let mode = if $check { ["--fail-on-change"] } else { [] }
        process-require $env.BACKEND_TREEFMT (
            ["--working-dir" (configuration-root)]
            | append $mode
            | append "--no-cache"
            | append $formatter_paths
        ) | ignore
    }
    {
        packages: ($packages | get name)
        configuration: $configuration_paths
        mode: (if $check { "check" } else { "write" })
    }
}
