# Exposes cheap structural checks as ordinary flake checks.
# Uses nuenv derivations so validation never falls back to shell scripting.
# Reserves expensive product proof for explicitly scoped Nushell commands.
{
  pkgs,
  tools,
  commands,
}:
{
  nushell-command = commands.backend;

  ast-grep-rules = pkgs.nuenv.mkDerivation {
    name = "backend-ast-grep-rules";
    src = pkgs.writeTextDir "empty/.keep" "";
    packages = tools.qualityTools;
    build = ''
      cd ${../.}
      ast-grep test --config ./ast-grep/sgconfig.yml
      mkdir ($env.out | path join "share")
      "validated" | save ($env.out | path join "share" "ast-grep")
    '';
  };

  tooling-contracts = pkgs.nuenv.mkDerivation {
    name = "backend-tooling-contracts";
    src = pkgs.writeTextDir "empty/.keep" "";
    packages = [
      commands.backend
      pkgs.nushell
    ];
    BACKEND_CONFIG = toString ../.;
    build = ''
      backend agents verify
      nu --no-config-file ${../nu/tests.nu}
      mkdir ($env.out | path join "share")
      "validated" | save ($env.out | path join "share" "tooling-contracts")
    '';
  };
}
