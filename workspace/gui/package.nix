# lindsey.app, built with the same fenix toolchain as the rest of the flake
# and wrapped so the subprocess oracles, embed model, and toolchains that
# cargo-bundle does not know about actually ship.
#
# cargo-bundle only copies the GUI binary and icon metadata in Cargo.toml.
# This derivation is the full release path: `cargo build --release --bin lindsey`,
# `cargo bundle --format osx`, then `lindsey-install-wrapper`.
{
  pkgs,
  rustToolchain,
  cargoBundle,
  src,
  installWrapper,
  onnxruntimeLib,
}:

let
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };

  inherit (pkgs) lib;
in
rustPlatform.buildRustPackage {
  pname = "lindsey";
  version = "0.1.0";

  inherit src;

  cargoRoot = "workspace/gui";
  buildAndTestSubdir = "workspace/gui";

  # cargoLock.outputHashes collides on `trustfall-0.8.1` (crates.io + git).
  # `cargo vendor` via cargoHash names those two copies apart.
  cargoHash = "sha256-BSOvxwCMneDIlTuRHjToa5gyVIcap07JfJJd5vwoAEo=";

  nativeBuildInputs = [
    pkgs.pkg-config
    pkgs.cmake
    pkgs.libclang
    installWrapper
  ];

  buildInputs = [
    pkgs.libiconv
    pkgs.openssl
  ];

  cargoBuildFlags = [
    "--bin"
    "lindsey"
  ];

  doCheck = false;

  env = {
    LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
    OPENSSL_DIR = "${pkgs.openssl.dev}";
    OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
    OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
    # Official ONNX Runtime 1.28 (ort-sys's requested version). nixpkgs ships
    # 1.26; pyke's dist is a raw LZMA2 stream the sandbox cannot unpack.
    ORT_LIB_LOCATION = "${onnxruntimeLib}";
    ORT_PREFER_DYNAMIC_LINK = "1";
    DOTNET_CLI_TELEMETRY_OPTOUT = "1";
  };

  installPhase = ''
    runHook preInstall
    ${lib.optionalString pkgs.stdenv.isDarwin ''
      # cargo-bundle re-invokes `cargo build` in installPhase, where cc-rs
      # cannot see libc++ headers (`esaxx-rs` then fails on <cstdint>). The
      # release binary is already here; assemble the .app around it and let
      # lindsey-install-wrapper inject oracles + embed model.
      bin="$(find . -name lindsey -type f -path '*/release/lindsey' ! -path '*/deps/*' | head -n 1)"
      if [ -z "$bin" ]; then
        echo "no release lindsey binary after cargoBuildHook" >&2
        find . -name lindsey -type f >&2 || true
        exit 1
      fi
      app="$out/lindsey.app"
      mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
      cp "$bin" "$app/Contents/MacOS/lindsey"
      chmod +x "$app/Contents/MacOS/lindsey"
      cat > "$app/Contents/Info.plist" <<'PLIST'
      <?xml version="1.0" encoding="UTF-8"?>
      <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
      <plist version="1.0">
      <dict>
        <key>CFBundlePackageType</key>
        <string>APPL</string>
        <key>CFBundleName</key>
        <string>lindsey</string>
        <key>CFBundleDisplayName</key>
        <string>lindsey</string>
        <key>CFBundleIdentifier</key>
        <string>com.nudox.lindsey</string>
        <key>CFBundleVersion</key>
        <string>0.1.0</string>
        <key>CFBundleShortVersionString</key>
        <string>0.1.0</string>
        <key>CFBundleExecutable</key>
        <string>lindsey</string>
        <key>LSMinimumSystemVersion</key>
        <string>14.0</string>
        <key>NSHighResolutionCapable</key>
        <true/>
      </dict>
      </plist>
      PLIST
      if [ -f workspace/gui/assets/logo.svg ]; then
        cp workspace/gui/assets/logo.svg "$app/Contents/Resources/logo.svg"
      fi
      chmod -R u+w "$app"
      lindsey-install-wrapper "$app"
    ''}
    ${lib.optionalString (!pkgs.stdenv.isDarwin) ''
      install -Dm755 target/release/lindsey "$out/bin/lindsey"
    ''}
    runHook postInstall
  '';

  meta = {
    description = "lindsey desktop app, wrapped with subprocess oracles and the embed model";
    mainProgram = "lindsey";
  };
}
