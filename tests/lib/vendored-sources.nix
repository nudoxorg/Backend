# Materialize the four build inputs a checkout deliberately does not track, so
# a Nix check builds the same crate graph a developer does.
#
# # Why this is a shared function
#
# Every Nix check that compiles the root workspace needs all four, and the
# Cargo manifests reference them by path, so a check that skips one does not
# build a smaller workspace — it fails to evaluate. `tests/workspace-tests`
# grew this block inline first; it is factored here so the second consumer
# (`tests/index-tests`) cannot drift from it, which is the failure mode that
# matters: two copies of a vendoring recipe silently pinning different
# versions produce two different crate graphs under one flake.
#
# Each source is fetched by pinned hash, so this adds no network trust beyond
# what the lockfile already asserts.
#
# `smolvmSource` is passed in rather than fetched here because the flake
# already resolves it as an input.
{
  pkgs,
  smolvmSource,
}:

''
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

  # The Cargo patch intentionally points at a local fork that is not
  # source-controlled. Materialize the exact upstream crate and the one-line
  # fork at build time instead of depending on an untracked working tree.
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
  # checkout, but it is the real default catalog engine — the store every
  # `index` test reads and writes.
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
  # crate. This one is load-bearing for the `server` feature specifically:
  # the fork exists so `zstd-seekable` links `zstd-sys`'s libzstd instead of
  # bundling a second copy, whose duplicate `ZSTD_*` symbols macOS `ld`
  # resolves arbitrarily and silently corrupts `index::pack` at runtime.
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
''
