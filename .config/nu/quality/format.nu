# Formats only files and packages selected by the current Git change set.
# Dispatches each language to its pinned formatter with central configuration.
# Refuses to turn a narrow edit into an unbounded workspace rewrite.

# Formats changed Rust packages and changed Nix or Nushell configuration files.
def "main format changed" [--base: string, --check]: nothing -> record {
  let root = (repository-root)
  let packages = (changed-packages --base $base)
  let rustfmt_arguments = if $check {
    ["fmt" "--check" "--manifest-path" ($root | path join "Cargo.toml")]
  } else {
    ["fmt" "--manifest-path" ($root | path join "Cargo.toml")]
  }
  for package in $packages {
    process-require $env.BACKEND_STABLE_CARGO ($rustfmt_arguments | append ["--package" $package.name "--" "--config-path" ($root | path join ".config/rustfmt.toml")]) | ignore
  }
  let configuration_paths = (changed-paths --base $base | where {|path| ($path | path exists) and (($path | str ends-with ".nix") or ($path | str ends-with ".nu") or ($path | str ends-with ".toml")) })
  for relative in $configuration_paths {
    let path = ($root | path join $relative)
    if ($relative | str ends-with ".nix") {
      let arguments = if $check { ["--check" $path] } else { [$path] }
      process-require "nixfmt" $arguments | ignore
    } else if ($relative | str ends-with ".nu") {
      let arguments = if $check { ["--check" $path] } else { [$path] }
      process-require "nufmt" $arguments | ignore
    } else {
      let arguments = if $check { ["format" "--check" $path] } else { ["format" $path] }
      process-require "taplo" $arguments | ignore
    }
  }
  { packages: ($packages | get name), configuration: $configuration_paths, mode: (if $check { "check" } else { "write" }) }
}
