# Executes tests at a scope explicitly bounded by actor and change impact.
# Makes nextest groups the concurrency and scarce-resource ownership authority.
# Prevents implementors and researchers from silently escalating to broad suites.

# Runs unit tests for packages selected by changed paths.
def "main test changed" [--base: string]: nothing -> record {
  let packages = (required-changed-package-names --base $base)
  let package_arguments = ($packages | each {|name| ["--package" $name] } | flatten)
  process-require $env.BACKEND_STABLE_CARGO (["nextest" "run" "--locked" "--config-file" (configuration-root | path join "nextest.toml") "--profile" "default" "-E" "kind(lib)"] | append $package_arguments) | ignore
  { level: "unit", packages: $packages }
}

# Runs all affected targets for changed packages after Terra owns closure.
def "main test affected" [--base: string]: nothing -> record {
  require-role "affected-tests" ["terra-academic" "terra-reviewer" "sol-integrator"]
  let packages = (required-changed-package-names --base $base)
  let package_arguments = ($packages | each {|name| ["--package" $name] } | flatten)
  process-require $env.BACKEND_STABLE_CARGO (["nextest" "run" "--locked" "--config-file" (configuration-root | path join "nextest.toml") "--profile" "affected"] | append $package_arguments) | ignore
  { level: "affected", packages: $packages, owner: (active-role) }
}

# Runs the complete workspace only for a short-lived Sol integration closure.
def "main test workspace" []: nothing -> record {
  require-role "workspace-tests" ["sol-integrator"]
  process-require $env.BACKEND_STABLE_CARGO ["nextest" "run" "--locked" "--workspace" "--config-file" (configuration-root | path join "nextest.toml") "--profile" "closure"] | ignore
  { level: "workspace", owner: (active-role) }
}
