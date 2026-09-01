# Builds tiny role-local command surfaces from capability aliases.
# Maps each alias to the AST-derived backend command identifier convention.
# Prevents agents from receiving the general backend command menu on PATH.
{
  pkgs,
  roleRunners,
  roles,
}:
pkgs.lib.mapAttrs (
  roleId: role:
  pkgs.buildEnv {
    name = "backend-${roleId}-tools";
    paths = pkgs.lib.mapAttrsToList (
      tool: commandId:
      pkgs.nuenv.writeShellApplication {
        name = tool;
        runtimeEnv = {
          BACKEND_AGENT_TOOL = tool;
          BACKEND_CONFIG_MODE = "immutable";
        };
        text = ''
          def --wrapped --env main [...extra: string] {
            if (($env.BACKEND_AGENT_RUN? | default "") | is-empty) {
              $env.BACKEND_AGENT_RUN = $"${roleId}-(random uuid)"
            }
            if (($env.BACKEND_AGENT_CARD? | default "") | is-empty) {
              $env.BACKEND_AGENT_CARD = "unscoped"
            }
            $env.BACKEND_AGENT_INVOCATION_ID = (random uuid)
            let root = (run-external "${pkgs.git}/bin/git" "rev-parse" "--show-toplevel" | complete)
            if $root.exit_code != 0 {
              error make {msg: "role tools require an existing assigned Git worktree to derive candidate identity"}
            }
            let repository = $root.stdout | str trim
            let status = (
              run-external "${pkgs.git}/bin/git" "-C" $repository "status" "--porcelain=v2" "-z"
              | complete
            )
            if $status.exit_code != 0 {
              error make {msg: "role tools could not inspect the assigned candidate worktree"}
            }
            let tracked = (
              run-external "${pkgs.git}/bin/git" "-C" $repository "diff" "--no-ext-diff" "--binary" "HEAD"
              | complete
            )
            if $tracked.exit_code != 0 {
              error make {msg: "role tools could not hash the tracked candidate delta"}
            }
            let separator = char --integer 0
            let untracked = (
              run-external "${pkgs.git}/bin/git" "-C" $repository "ls-files" "--others" "--exclude-standard" "-z"
              | complete
            )
            if $untracked.exit_code != 0 {
              error make {msg: "role tools could not enumerate the untracked candidate delta"}
            }
            let untracked_hashes = (
              $untracked.stdout
              | split row $separator
              | where {|path| $path != "" }
              | each {|path|
                  let identity = (
                    run-external "${pkgs.git}/bin/git" "-C" $repository "hash-object" "--no-filters" "--" $path
                    | complete
                  )
                  if $identity.exit_code != 0 {
                    error make {msg: $"role tools could not hash untracked candidate path: ($path)"}
                  }
                  {path: $path, identity: ($identity.stdout | str trim)}
                }
            )
            $env.BACKEND_AGENT_CHANGE_DIGEST = (
              {
                status: ($status.stdout | hash sha256)
                tracked: ($tracked.stdout | hash sha256)
                untracked: $untracked_hashes
              }
              | to json
              | hash sha256
            )
            let arguments = '${builtins.toJSON (pkgs.lib.splitString "-" commandId)}' | from json
            run-external "${roleRunners.${roleId}}/bin/backend" ...$arguments ...$extra
          }
        '';
      }
    ) role.tools;
  }
) roles
