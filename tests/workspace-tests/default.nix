{
  pkgs,
  mkNuCheck,
  nuLib,
  projectRoot,
  rustToolchain,
  smolvmSource,
  corpus,
}:

mkNuCheck {
  name = "workspace-tests";
  script = ./check.nu;
  src = ../..;
  inherit nuLib;

  runtimeInputs =
    with pkgs;
    [
      cargo-nextest
      git
      nix
      pkg-config
    ]
    ++ [
      rustToolchain
      jdk21_headless
    ];

  env = {
    NUDOX_CORPUS_ROOT = corpus;
    JAVA_HOME = "${pkgs.jdk21_headless}";
    NUDOX_JAVAC = "${pkgs.jdk21_headless}/bin/javac";
  };

  # Cargo's `mod` declarations make every Rust file under this package's
  # source tree a declared input. Keep the dirty-worktree overlay scoped to
  # that tree and to `.rs` files; builtins.path also fails evaluation if the
  # declared tree is absent.
  extraSrcTrees = [
    {
      relPath = "workspace/nudox-engine/src";
      source = builtins.path {
        path = "${projectRoot}/workspace/nudox-engine/src";
        name = "workspace-test-nudox-engine-rust-src";
        filter = path: type: type == "directory" || builtins.match ".*\\.rs$" path != null;
      };
    }
  ];

  extraSrcFiles =
    map
      (relPath: {
        inherit relPath;
        source = builtins.path {
          path = "${projectRoot}/${relPath}";
          name = "workspace-test-${builtins.replaceStrings [ "/" ] [ "-" ] relPath}";
        };
      })
      [
        "workspace/nudox-engine/tests/archive_cache.rs"
        "workspace/nudox-engine/tests/engine/non_rust_producer.rs"
        "workspace/nudox-engine/tests/engine/semantic_unconfigured_is_actionable.rs"
        "workspace/nudox-engine/tests/mcp/address_adversarial.rs"
        "workspace/nudox-engine/tests/mcp/address_resolution.rs"
        "workspace/nudox-engine/tests/mcp/diagnose_source_format.rs"
        "workspace/nudox-engine/tests/mcp/endpoint_stability.rs"
        "workspace/nudox-engine/tests/mcp/index_locals_and_dependencies.rs"
        "workspace/nudox-engine/tests/mcp/refs_non_rust_languages.rs"
        "workspace/nudox-engine/tests/mcp/schema_card_queries.rs"
        "workspace/nudox-engine/tests/mcp/search_member_noise.rs"
        "workspace/compiler/languages/src/oracle.rs"
        "workspace/compiler/languages/src/java/invoke.rs"
        "workspace/compiler/languages/tests/go/oracle_staleness.rs"
        "workspace/compiler/languages/tests/java/oracle_staleness.rs"
        "workspace/compiler/languages/tests/csharp/oracle_staleness.rs"
        "workspace/compiler/languages/tests/python/pyrefly_feature_path.rs"
        "workspace/compiler/languages/tests/rust/cfg_on_enum_variants.rs"
        "workspace/compiler/languages/tests/rust/serde_json_diagnosis.rs"
      ];

  preBuild = ''
    export CARGO_HOME="$TMPDIR/cargo-home"
    export HOME="$TMPDIR/home"
    export GIT_CONFIG_GLOBAL="$HOME/.gitconfig"
    export SSL_CERT_FILE="${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
    export GIT_SSL_CAINFO="$SSL_CERT_FILE"
    export CARGO_HTTP_CAINFO="$SSL_CERT_FILE"
    mkdir -p "$CARGO_HOME"
    mkdir -p "$HOME"
    git config --file "$GIT_CONFIG_GLOBAL" url."https://github.com/".insteadOf git@github.com:
    git config --file "$GIT_CONFIG_GLOBAL" url."https://github.com/".insteadOf ssh://git@github.com/
    git config --file "$GIT_CONFIG_GLOBAL" url."https://github.com/smol-machines/libkrun.git".insteadOf git@github.com:smol-machines/libkrun.git
    git config --file "$GIT_CONFIG_GLOBAL" url."https://github.com/smol-machines/libkrun.git".insteadOf ssh://git@github.com/smol-machines/libkrun.git
    mkdir -p .cargo
    cat > .cargo/config.toml <<'EOF'
    [net]
    git-fetch-with-cli = true

    [url."https://github.com/"]
    insteadOf = "git@github.com:"
    EOF
    cp -R ${smolvmSource} "$TMPDIR/smolvm"
    chmod -R u+w "$TMPDIR/smolvm"
    substituteInPlace workspace/compiler/sandbox/Cargo.toml \
      --replace-fail \
        'smolvm = { git = "https://github.com/smol-machines/smolvm", rev = "56bb13b99dfe65a58df158bf241077a5024d3a64" }' \
        "smolvm = { path = \"$TMPDIR/smolvm\" }"

    # The Cargo patch intentionally points at a local fork, but that fork is
    # not source-controlled in this checkout. Materialize the exact upstream
    # crate and the one-line fork at build time instead of depending on an
    # untracked working-tree directory.
    rm -rf workspace/vendor/pyroscope
    mkdir -p workspace/vendor/pyroscope
    ${pkgs.gnutar}/bin/tar -xzf ${
      pkgs.fetchurl {
        url = "https://crates.io/api/v1/crates/pyroscope/0.5.8/download";
        hash = "sha256-06X2Ow0nJwldtZBF5qDvMlmyi5DUga6I8OPYZtAjTOg=";
      }
    } --strip-components=1 -C workspace/vendor/pyroscope
    substituteInPlace workspace/vendor/pyroscope/Cargo.toml \
      --replace-fail 'version = "0.5.8"' 'version = "0.5.8+nudox.1"'
    substituteInPlace workspace/vendor/pyroscope/src/session.rs \
      --replace-fail \
        'let client = reqwest::blocking::Client::new();' \
        'static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
        let client = CLIENT.get_or_init(reqwest::blocking::Client::new);'

    # DoltLite's generated amalgamation is intentionally ignored in the
    # checkout, but it is the real default catalog engine. Restore only the
    # pinned upstream release asset; fetchurl verifies the archive before this
    # exact implementation is copied into the filtered source.
    mkdir -p "$TMPDIR/doltlite"
    ${pkgs.unzip}/bin/unzip -q ${
      pkgs.fetchurl {
        url = "https://github.com/dolthub/doltlite/releases/download/v0.11.41/doltlite-amalgamation-0.11.41.zip";
        hash = "sha256-fnlrlVeUXEKMqISlboSfXBHEPUxWsP+LF299+cfR6Fg=";
      }
    } -d "$TMPDIR/doltlite"
    cp "$TMPDIR/doltlite/doltlite-amalgamation-0.11.41/doltlite.c" \
      workspace/vendor/doltlite/doltlite.c

    # The tracked fork retains only build.rs and the Rust wrapper. Restore the
    # exact upstream seekable-format C implementation from the pinned 0.1.23
    # crate; do not broaden the source snapshot to the ignored vendor tree.
    mkdir -p "$TMPDIR/zstd-seekable"
    ${pkgs.gnutar}/bin/tar -xzf ${
      pkgs.fetchurl {
        url = "https://crates.io/api/v1/crates/zstd-seekable/0.1.23/download";
        hash = "sha256-V0oRfFzbiNHxM4HuOhmmpF+2ygyYQ206ld+FK3ymw8I=";
      }
    } --strip-components=1 -C "$TMPDIR/zstd-seekable"
    mkdir -p workspace/vendor/zstd-seekable/zstd/contrib
    cp -R "$TMPDIR/zstd-seekable/zstd/contrib/seekable_format" \
      workspace/vendor/zstd-seekable/zstd/contrib/seekable_format
    cp "$TMPDIR/zstd-seekable/xxh64.c" workspace/vendor/zstd-seekable/xxh64.c

    # The unpacked Nix source has no checkout metadata, but the completeness
    # tests intentionally ask Git which files a clean checkout contains. Build
    # a real index from this exact filtered source after all declared overlays
    # have been materialized; do not create an empty repository that could make
    # the tests pass vacuously.
    git init --quiet .
    git add --all
  '';

  noChroot = true;
  preferLocalBuild = true;
  allowSubstitutes = false;

  resultLines = [
    "workspace-tests: ok"
    "suite: cargo nextest -P default --workspace --locked"
  ];
}
