# Resolves the PR lane's compiled target into its existing bounded managed
# cache. Keep the target beneath the managed root that cap-build-cache owns,
# and bind that root to the lane's physical Cargo home rather than trusting an
# arbitrary environment path.
export def managed-pr-target-dir [workspace_root: string, cache_root: string, cargo_home: string]: nothing -> string {
    if ($cache_root | is-empty) or ($cargo_home | is-empty) or ($workspace_root | is-empty) {
        error make {msg: "managed PR target requires workspace, cache, and Cargo home roots"}
    }

    let cache = (^realpath $cache_root | complete)
    let home = (^realpath $cargo_home | complete)
    let workspace = (^realpath $workspace_root | complete)
    if ($cache.exit_code != 0) or ($home.exit_code != 0) or ($workspace.exit_code != 0) {
        error make {msg: "managed PR target roots must already exist and resolve physically"}
    }

    let physical_cache = $cache.stdout | str trim
    let physical_home = $home.stdout | str trim
    let physical_workspace = $workspace.stdout | str trim
    let roots_exist = (
        [$physical_cache $physical_home $physical_workspace]
        | all {|path| (^test -d $path | complete).exit_code == 0 }
    )
    if not $roots_exist {
        error make {msg: "managed PR target roots must already exist and be directories"}
    }
    let expected_cache = $physical_home | path dirname | path join "managed"
    if $physical_cache != $expected_cache {
        error make {msg: "managed PR target cache does not match the lane Cargo home"}
    }

    let target = $physical_cache | path join "target"
    if ($target == $physical_workspace) or ($target | str starts-with $"($physical_workspace)/") {
        error make {msg: "managed PR target must remain outside the source worktree"}
    }
    if (^test -L $target | complete).exit_code == 0 {
        error make {msg: "managed PR target cannot be a symlink"}
    }
    $target
}
