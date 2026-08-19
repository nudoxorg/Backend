# ONNX Runtime 1.28 CPU dylib for Intel macOS, compiled on Apple Silicon.
#
# Upstream ships no osx-x86_64 tarball for 1.28.0. nixpkgs 26.11 also dropped
# the x86_64-darwin stdenv, so this is a host (aarch64-darwin) cmake build
# with `--osx_arch x86_64`, not `pkgsCross`. CMake FetchContent pulls
# protobuf/onnx/abseil during configure — Darwin Nix already reaches the
# network for the GUI's iconutil/Xcode path; this derivation needs the same.
{
  pkgs,
  version ? "1.28.0",
}:

pkgs.stdenv.mkDerivation {
  pname = "onnxruntime-osx-x86_64";
  inherit version;

  src = pkgs.fetchFromGitHub {
    owner = "microsoft";
    repo = "onnxruntime";
    rev = "v${version}";
    hash = "sha256-SjNUG4pCVHIDxmOQkZ/nWYcdC+CBRLogpwb1AtAKf+M=";
  };

  nativeBuildInputs = [
    pkgs.cmake
    pkgs.python3
    pkgs.git
    pkgs.cacert
    pkgs.nasm
  ];

  # Don't let the cmake setup hook treat this as a regular cmake project;
  # onnxruntime's build.sh drives the generator.
  dontUseCmakeConfigure = true;

  env = {
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    CMAKE_OSX_ARCHITECTURES = "x86_64";
  };

  buildPhase = ''
    runHook preBuild
    patchShebangs build.sh
    ./build.sh \
      --config Release \
      --parallel \
      --skip_tests \
      --skip_submodule_sync \
      --compile_no_warning_as_error \
      --build_dir "$NIX_BUILD_TOP/build" \
      --osx_arch x86_64 \
      --build_shared_lib \
      --cmake_extra_defines \
        onnxruntime_ENABLE_PYTHON=OFF \
        onnxruntime_BUILD_UNIT_TESTS=OFF \
        CMAKE_OSX_DEPLOYMENT_TARGET=14.0 \
        CMAKE_OSX_ARCHITECTURES=x86_64
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p "$out/lib"
    libdir="$NIX_BUILD_TOP/build/Release"
    if [ ! -e "$libdir/libonnxruntime.1.dylib" ]; then
      echo "no libonnxruntime.1.dylib under $libdir" >&2
      ls -la "$libdir" >&2 || true
      exit 1
    fi
    cp -a "$libdir/libonnxruntime.1.dylib" "$out/lib/"
    if [ -L "$libdir/libonnxruntime.1.dylib" ]; then
      target="$(readlink "$libdir/libonnxruntime.1.dylib")"
      case "$target" in
        /*) cp -a "$target" "$out/lib/" ;;
        *) cp -a "$libdir/$target" "$out/lib/" ;;
      esac
    fi
    runHook postInstall
  '';

  __darwinAllowLocalNetworking = true;

  meta = {
    description = "ONNX Runtime ${version} CPU dylib (x86_64-apple-darwin, source build)";
    platforms = [ "aarch64-darwin" ];
  };
}
