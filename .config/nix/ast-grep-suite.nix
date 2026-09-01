# Materializes the Nix-authored syntax-lint suite for ast-grep.
# Keeps rule and matrix YAML generated while linking immutable expected snapshots.
# Gives checks and interactive commands the identical evaluated directory.
{ pkgs, rules }:
let
  yaml = pkgs.formats.yaml { };
  entries = builtins.concatLists (
    builtins.attrValues (
      builtins.mapAttrs (
        id: specification:
        let
          rule = {
            inherit id;
            inherit (specification)
              language
              message
              note
              severity
              ;
            rule = specification.matcher;
          }
          // pkgs.lib.optionalAttrs (specification ? files) { inherit (specification) files; }
          // pkgs.lib.optionalAttrs (specification ? constraints) { inherit (specification) constraints; };
          test = {
            inherit id;
            inherit (specification) invalid valid;
          };
          snapshot = {
            inherit id;
            inherit (specification) severity;
            file = if specification ? files then "lib.rs" else "case.rs";
            cases = pkgs.lib.zipListsWith (source: count: {
              inherit count source;
            }) specification.invalid specification.snapshotCounts;
          };
        in
        [
          {
            target = "rules/${id}.yml";
            source = yaml.generate "${id}-rule.yml" rule;
          }
          {
            target = "tests/${id}.yml";
            source = yaml.generate "${id}-test.yml" test;
          }
          {
            target = "metadata/${id}.json";
            source = pkgs.writeText "${id}-snapshot.json" (builtins.toJSON snapshot);
          }
        ]
      ) rules
    )
  );
in
pkgs.nuenv.mkDerivation {
  name = "backend-ast-grep-suite";
  src = pkgs.writeTextDir "empty/.keep" "";
  packages = [ pkgs.ast-grep ];
  build = ''
    let suite = $env.TMPDIR | path join "suite"
    mkdir $suite
    for entry in ('${builtins.toJSON entries}' | from json) {
      let destination = ($suite | path join $entry.target)
      mkdir ($destination | path dirname)
      open --raw $entry.source | save --raw $destination
    }
    {
      ruleDirs: [($suite | path join "rules")]
      testConfigs: [{ testDir: ($suite | path join "tests"), snapshotDir: "__snapshots__" }]
    } | to json --indent 2 | save --raw ($suite | path join "sgconfig.yml")
    let snapshots = $suite | path join "tests" "__snapshots__"
    mkdir $snapshots
    ast-grep test --config ($suite | path join "sgconfig.yml") --test-dir ($suite | path join "tests") --update-all
    ast-grep test --config ($suite | path join "sgconfig.yml") --test-dir ($suite | path join "tests")
    mkdir $env.out
    cp --recursive ($suite | path join "rules") $env.out
    cp --recursive ($suite | path join "tests") $env.out
    cp --recursive ($suite | path join "metadata") $env.out
    {
      ruleDirs: [($env.out | path join "rules")]
      testConfigs: [{ testDir: ($env.out | path join "tests"), snapshotDir: "__snapshots__" }]
    } | to json --indent 2 | save --raw ($env.out | path join "sgconfig.yml")
  '';
}
