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
  installWrapperLinux,
  onnxruntimeLib,
  ortStaticLink ? false,
  # When set (e.g. "x86_64-apple-darwin"), cargo cross-compiles and this
  # derivation only installs the Mach-O — the universal .app lipo lives in
  # nix/lindsey-universal.nix. Null is the native `lindsey-app` path.
  crossTarget ? null,
  # Stamped into Info.plist below and into this derivation's own version.
  # Defaults to "0.1.0" (same default ci-package-lindsey.sh's LINDSEY_VERSION
  # env var has) when the caller — a tagged release build — doesn't pass one.
  version ? "0.1.0",
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
    installWrapperLinux
  ];

  buildInputs = [
    pkgs.libiconv
    pkgs.openssl
  ]
  # GPUI's Linux backend needs libxcb/libxkbcommon (linked) and
  # wayland/vulkan-loader (dlopened by soname, invisible to the linker but
  # still resolved through this same pkg-config-built search path at build
  # time) plus dbus for the keyring's dbus-secret-service chain, and
  # fontconfig/freetype for font-kit's pkg-config build scripts
  # (yeslogic-fontconfig-sys, freetype-sys) -- same set flake.nix's devShell
  # already carries as guiGraphicsLibraries/guiCredentialLibraries/
  # guiFontLibraries. Without these, pkg-config falls through with "Package
  # fontconfig was not found in the pkg-config search path" (or the xcb/
  # xkbcommon equivalent) instead of resolving to the Nix store.
  ++ lib.optionals pkgs.stdenv.isLinux (
    with pkgs;
    [
      libxcb
      libxkbcommon
      wayland
      vulkan-loader
      dbus
      fontconfig
      freetype
    ]
  );

  cargoBuildFlags = [
    "--bin"
    "lindsey"
  ]
  ++ lib.optionals (crossTarget != null) [
    "--target"
    crossTarget
  ];

  doCheck = false;

  preConfigure = lib.optionalString (crossTarget == "x86_64-apple-darwin") ''
    sdk="$(/usr/bin/xcrun --sdk macosx --show-sdk-path)"
    export SDKROOT="$sdk"
    export BINDGEN_EXTRA_CLANG_ARGS_x86_64_apple_darwin="--target=x86_64-apple-darwin -isysroot $sdk"
    export BINDGEN_EXTRA_CLANG_ARGS="--target=x86_64-apple-darwin -isysroot $sdk"
    export CC_x86_64_apple_darwin="$(/usr/bin/xcrun --sdk macosx -f clang)"
    export CXX_x86_64_apple_darwin="$(/usr/bin/xcrun --sdk macosx -f clang++)"
    export CFLAGS_x86_64_apple_darwin="-target x86_64-apple-darwin -isysroot $sdk"
    export MACOSX_DEPLOYMENT_TARGET=14.0
    if [ -f /Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/libclang.dylib ]; then
      export LIBCLANG_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib
    fi
  '';

  env = {
    LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
    OPENSSL_DIR = "${pkgs.openssl.dev}";
    OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
    OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
    DOTNET_CLI_TELEMETRY_OPTOUT = "1";
    # rustc strip of the symbol table. The 73MB unstripped Mach-O next to a
    # 612MB embed model is what makes Get Info look like a debug build; this
    # is still the release profile (thin LTO in workspace/gui/Cargo.toml).
    CARGO_PROFILE_RELEASE_STRIP = "symbols";
  }
  # ort-sys's build.rs links openssl dynamically (its own HTTPS fetch path)
  # and runs as a plain host-arch executable during the build -- OPENSSL_DIR/
  # OPENSSL_LIB_DIR above only steer openssl-sys's *compile-time* linking of
  # the final target binary, they say nothing to the dynamic loader that
  # execs the already-built build-script-main. Without libssl.so.3 on
  # LD_LIBRARY_PATH that exec fails at the loader level (exit 127, "error
  # while loading shared libraries") before the script even runs far enough
  # to read ORT_LIB_LOCATION.
  // lib.optionalAttrs pkgs.stdenv.isLinux {
    LD_LIBRARY_PATH = "${pkgs.openssl.out}/lib";
  }
  # Official ONNX Runtime 1.28 (ort-sys's requested version). nixpkgs ships
  # 1.26; pyke's dist is a raw LZMA2 stream the sandbox cannot unpack.
  # x86_64-darwin has no upstream 1.28 dylib tarball — use ortStaticLink and
  # a source-built static archive tree instead (ORT_LIB_PATH).
  #
  # ort-sys's SYSTEM_LIB_PATH lookup checks ORT_LIB_PATH *before*
  # ORT_LIB_LOCATION (build/vars.rs: `&["ORT_LIB_PATH", "ORT_LIB_LOCATION"]`,
  # first Ok(..) wins regardless of content) — `lib.optionalString` on the
  # non-selected branch only empties the *value*, it still exports the key,
  # so an empty ORT_LIB_PATH silently pre-empted a perfectly good
  # ORT_LIB_LOCATION and produced `cargo:rustc-link-search=native=` (an
  # empty `-L` argument, rejected by the linker) on every non-static
  # platform. `lib.optionalAttrs` omits the key entirely instead of just
  # blanking it, so only one of the two is ever actually set.
  // (
    if ortStaticLink then
      { ORT_LIB_PATH = "${onnxruntimeLib}/lib"; }
    else
      {
        ORT_LIB_LOCATION = "${onnxruntimeLib}";
        ORT_PREFER_DYNAMIC_LINK = "1";
      }
  );

  installPhase = ''
    runHook preInstall
    ${lib.optionalString (crossTarget != null) ''
      bin="$(find . -name lindsey -type f -path '*${crossTarget}/release/lindsey' ! -path '*/deps/*' | head -n 1)"
      if [ -z "$bin" ]; then
        echo "no ${crossTarget} release lindsey after cargoBuildHook" >&2
        find . -name lindsey -type f >&2 || true
        exit 1
      fi
      install -Dm755 "$bin" "$out/bin/lindsey"
    ''}
    ${lib.optionalString (crossTarget == null && pkgs.stdenv.isDarwin) ''
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
    ${lib.optionalString (crossTarget == null && !pkgs.stdenv.isDarwin) ''
      # Same reasoning as the Darwin branch above: install the raw binary
      # first, then let lindsey-install-wrapper-linux bundle the oracles,
      # embed model, and ORT .so tree around it -- cargo-bundle has no
      # Linux packaging story to route around here, there just was no
      # equivalent wrap step for this platform before.
      #
      # A flat target/release/lindsey assumption broke here the same way the
      # equivalent Darwin assumption broke before it: nixpkgs' rustc/cargo
      # setup hook can build under a target-triple subdirectory
      # (target/<triple>/release/) even for a "native" build depending on how
      # the fenix toolchain reports its host, so search for the binary
      # instead of hardcoding the flat layout.
      bin="$(find . -name lindsey -type f -path '*/release/lindsey' ! -path '*/deps/*' | head -n 1)"
      if [ -z "$bin" ]; then
        echo "no release lindsey binary after cargoBuildHook" >&2
        find . -name lindsey -type f >&2 || true
        exit 1
      fi
      install -Dm755 "$bin" "$out/bin/lindsey"
      lindsey-install-wrapper-linux "$out"
    ''}
    runHook postInstall
  '';

  meta = {
    description = "lindsey desktop app, wrapped with subprocess oracles and the embed model";
    mainProgram = "lindsey";
  };
}
