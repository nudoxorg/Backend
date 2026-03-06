{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:
let
  rust-nightly = inputs.fenix.packages.${pkgs.system}.complete.withComponents [
    "cargo"
    "clippy"
    "rust-src"
    "rust-docs"
    "rustc"
    "rustfmt"
    "rustc-codegen-cranelift-preview"
  ];

  # Separate nightly for checks (no rust-src/docs needed there)
  rust-nightly-check = inputs.fenix.packages.${pkgs.system}.complete.withComponents [
    "cargo"
    "clippy"
    "rustc"
    "rustfmt"
    "rustc-codegen-cranelift-preview"
  ];
in
{
  packages = [
    rust-nightly
    pkgs.git
    pkgs.cargo-bump
    pkgs.rust-analyzer
    pkgs.flock
    pkgs.nixfmt
    pkgs.tombi
    pkgs.typos
    pkgs.hongdown
    pkgs.radicle-node
    pkgs.radicle-tui
    pkgs.kittysay
    pkgs.marksman
    pkgs.taplo
    pkgs.cargo-nextest
    pkgs.libiconv
    pkgs.nil
    pkgs.jsonfmt
    pkgs.dotacat
    pkgs.goreleaser
    pkgs.cuelsp
    pkgs.b3sum
  ];

  env = {
    # TODO: find a more reliable way to avoid this linker issue
    LIBRARY_PATH = "$(nix eval --raw nixpkgs#libiconv.outPath)/lib";
    MAIN_PACKAGE = "nudox";
    OUTPUT_DIRECTORY = "dist";
    OPENSSL_DIR = "${pkgs.openssl.dev}";
    OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
    OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
  };

  enterShell = ''
    export RUST_TARGET=$(rustc --version --verbose | grep '^host:' | awk '{print $2}')
    (
      flock -n 9 || exit 1
    ) 9>/tmp/nunu_sync.lock &

    # MOTD
    $(type -p kittysay) --think "the nu is the now" | dotacat
  '';

  scripts = {
    # Build & Check
    check.exec = "cd $DEVENV_ROOT && nu .config/scripts/check.nu \"$@\"";
    build.exec = "cd $DEVENV_ROOT && nu .config/scripts/build.nu \"$@\"";
    build-release.exec = "cd $DEVENV_ROOT && nu .config/scripts/build-release.nu \"$@\"";

    # Packaging
    release.exec = "cd $DEVENV_ROOT && nu .config/scripts/release.nu";

    # Execution
    run.exec = "cd $DEVENV_ROOT && nu .config/scripts/run.nu \"$@\"";
    run-release.exec = "cd $DEVENV_ROOT && nu .config/scripts/run-release.nu \"$@\"";

    # Testing
    test.exec = "cd $DEVENV_ROOT && nu .config/scripts/test.nu \"$@\"";
    test-with.exec = "cd $DEVENV_ROOT && nu .config/scripts/test-with.nu \"$@\"";

    # Code Quality
    fmt.exec = "cd $DEVENV_ROOT && nu .config/scripts/fmt.nu \"$@\"";
    fmt-check.exec = "cd $DEVENV_ROOT && nu .config/scripts/fmt-check.nu \"$@\"";
    lint.exec = "cd $DEVENV_ROOT && nu .config/scripts/lint.nu \"$@\"";
    lint-fix.exec = "cd $DEVENV_ROOT && nu .config/scripts/lint-fix.nu \"$@\"";

    # Documentation
    doc.exec = "cd $DEVENV_ROOT && nu .config/scripts/doc.nu \"$@\"";
    doc-open.exec = "cd $DEVENV_ROOT && nu .config/scripts/doc-open.nu \"$@\"";

    # Maintenance
    create-notes.exec = "cd $DEVENV_ROOT && nu .config/scripts/create-notes.nu \"$@\"";
    update.exec = "cd $DEVENV_ROOT && nu .config/scripts/update.nu \"$@\"";
    clean.exec = "cd $DEVENV_ROOT && nu .config/scripts/clean.nu \"$@\"";
    patch.exec = "cd $DEVENV_ROOT && nu .config/scripts/patch.nu \"$@\"";

    # Installation
    install.exec = "cd $DEVENV_ROOT && nu .config/scripts/install.nu \"$@\"";
    install-force.exec = "cd $DEVENV_ROOT && nu .config/scripts/install-force.nu \"$@\"";

    # Utilities
    rad-sync.exec = "cd $DEVENV_ROOT && nu .config/scripts/rad-sync.nu \"$@\"";
  };

  # git-hooks input must be declared in devenv.yaml
  git-hooks = {
    default_stages = [ "pre-push" ];

    hooks = {
      # Enforce conventional commits on push; doesn't block flow during dev
      convco = {
        enable = true;
        pass_filenames = false;
        entry = toString (
          pkgs.writeShellScript "convco-pre-push" ''
            while read local_ref local_sha remote_ref remote_sha; do
              ${pkgs.convco}/bin/convco check "$remote_sha..$local_sha"
            done
          ''
        );
        stages = [ "pre-push" ];
      };

      # Formatting — purely for consistency, runs on push not commit
      nixfmt.enable = true;

      rustfmt = {
        enable = true;
        packageOverrides.cargo = rust-nightly-check;
        packageOverrides.rustfmt = rust-nightly-check;
      };

      markdownfmt = {
        enable = true;
        name = "hongdown";
        entry = "hongdown --write";
        files = "\\.md$";
        language = "system";
      };

      # Main branch must stay green — tests + clippy on merge only
      testrust = {
        enable = true;
        name = "testrust";
        entry = "cargo nextest run";
        language = "system";
        pass_filenames = false;
        stages = [ "pre-merge-commit" ];
      };

      clippy = {
        enable = true;
        packageOverrides.cargo = rust-nightly-check;
        packageOverrides.clippy = rust-nightly-check;
        stages = [
          "pre-merge-commit"
          "pre-push"
        ];
      };
    };
  };

  enterTest = ''
    echo "Running devenv self-check..."
    rustc --version
    cargo --version
    git --version
  '';
}
