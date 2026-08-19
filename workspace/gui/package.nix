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
  ortStaticLink ? false,
}:

let
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };

  inherit (pkgs) lib;

  # Single source for the version stamped into Info.plist below — was two
  # independently hardcoded "0.1.0" literals inside the installPhase heredoc,
  # decoupled from this derivation's own `version` and from Cargo.toml.
  version = "0.1.0";
in
rustPlatform.buildRustPackage {
  pname = "lindsey";
  inherit version;

  inherit src;

  cargoRoot = "workspace/gui";
  buildAndTestSubdir = "workspace/gui";

  # cargoLock.outputHashes collides on `trustfall-0.8.1` (crates.io + git).
  # `cargo vendor` via cargoHash names those two copies apart.
  cargoHash = "sha256-mX3Uz5s88eX2HrF8Yg6PjXbsYfQvX5382eVaaj2hEWg=";

  nativeBuildInputs = [
    pkgs.pkg-config
    pkgs.cmake
    pkgs.libclang
    pkgs.resvg
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
    # x86_64-darwin has no upstream 1.28 dylib tarball — use ortStaticLink and
    # a source-built static archive tree instead (ORT_LIB_PATH).
    ORT_LIB_PATH = lib.optionalString ortStaticLink "${onnxruntimeLib}/lib";
    ORT_LIB_LOCATION = lib.optionalString (!ortStaticLink) "${onnxruntimeLib}";
    ORT_PREFER_DYNAMIC_LINK = lib.optionalString (!ortStaticLink) "1";
    DOTNET_CLI_TELEMETRY_OPTOUT = "1";
    # rustc strip of the symbol table. The 73MB unstripped Mach-O next to a
    # 612MB embed model is what makes Get Info look like a debug build; this
    # is still the release profile (thin LTO in workspace/gui/Cargo.toml).
    CARGO_PROFILE_RELEASE_STRIP = "symbols";
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
        <string>${version}</string>
        <key>CFBundleShortVersionString</key>
        <string>${version}</string>
        <key>CFBundleExecutable</key>
        <string>lindsey</string>
        <key>CFBundleIconFile</key>
        <string>lindsey</string>
        <key>LSMinimumSystemVersion</key>
        <string>14.0</string>
        <key>NSHighResolutionCapable</key>
        <true/>
      </dict>
      </plist>
      PLIST
      svg=workspace/gui/assets/logo.svg
      if [ -f "$svg" ]; then
        # Same iconset cargo-bundle's create_icns_from_svg writes
        # (16/16@2x/32/32@2x/128/128@2x/256/256@2x/512/512@2x).
        iconset="$TMPDIR/lindsey.iconset"
        mkdir -p "$iconset"
        render() {
          resvg -w "$1" -h "$1" "$svg" "$iconset/$2"
        }
        render 16 icon_16x16.png
        render 32 icon_16x16@2x.png
        render 32 icon_32x32.png
        render 64 icon_32x32@2x.png
        render 128 icon_128x128.png
        render 256 icon_128x128@2x.png
        render 256 icon_256x256.png
        render 512 icon_256x256@2x.png
        render 512 icon_512x512.png
        render 1024 icon_512x512@2x.png
        /usr/bin/iconutil -c icns "$iconset" -o "$app/Contents/Resources/lindsey.icns"
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
