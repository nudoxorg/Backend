# Resolve a real compiler-daemon binary for integration checks.
#
# Preference order:
#   1. NUDOX_COMPILER_DAEMON_PATH (env) — store path or bare binary
#   2. packaged $NUDOX_COMPILER_PACKAGE/bin/compiler-daemon (if not placeholder)
#   3. buck2 build from NUDOX_PROJECT_ROOT / PRJ_ROOT

# Return true if the file looks like the snowydeer placeholder stub.
def is-placeholder [path: string] {
  if not ($path | path exists) { return true }
  try {
    let text = open --raw $path
    $text | str contains "FATAL: compiler-daemon not built"
  } catch {
    false
  }
}

# Normalize env/store path that may be a dir with bin/ or a bare executable.
def normalize-daemon [src: string] {
  if ($src | path join "bin/compiler-daemon" | path exists) {
    $src | path join "bin/compiler-daemon"
  } else if ($src | path exists) and (($src | path type) == "file") {
    $src
  } else {
    null
  }
}

# Resolve compiler binary path. Writes under $work when buck2 builds.
export def resolve-compiler [
  work: string
  packaged: string = ""
  project_root: string = ""
] {
  # 1. Explicit env path
  let from_env = ($env.NUDOX_COMPILER_DAEMON_PATH? | default "")
  if $from_env != "" {
    let bin = normalize-daemon $from_env
    if $bin != null and not (is-placeholder $bin) {
      return $bin
    }
  }

  # 2. Packaged derivation
  if $packaged != "" {
    let bin = $packaged | path join "bin/compiler-daemon"
    if ($bin | path exists) and not (is-placeholder $bin) {
      return $bin
    }
  }

  # 3. buck2 from checkout
  let root = if $project_root != "" {
    $project_root
  } else {
    $env.PRJ_ROOT? | default ($env.PWD? | default "")
  }

  if $root == "" or not ($root | path exists) {
    print --stderr "compiler-daemon is a placeholder and projectRoot is unset/missing."
    print --stderr "Import a real binary:"
    print --stderr "  buck2 build //workspace/compiler:compiler-daemon --out /tmp/compiler-daemon"
    print --stderr "  NUDOX_COMPILER_DAEMON_PATH=/tmp nix build --impure .#checks.$system.backendImage"
    print --stderr "Or set PRJ_ROOT to the repo checkout and re-run with --impure so buck2 can build it."
    error make { msg: "compiler-daemon unavailable (placeholder, no projectRoot)" }
  }

  print --stderr $"==> packages.compiler-daemon is a stub; building via buck2 in ($root)"
  let out = $work | path join "compiler-daemon"
  let isolation = $"backend-image-check-($nu.pid)"
  cd $root
  ^buck2 build //workspace/compiler:compiler-daemon --isolation-dir $isolation --out $out
  ^chmod +x $out
  $out
}
