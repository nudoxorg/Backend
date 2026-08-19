# CPU-only ONNX Runtime 1.28 built from source as static archives.
#
# Used for x86_64-darwin where upstream publishes no 1.28 prebuilt dylib.
# ort-sys links statically when ORT_LIB_PATH points at the Release lib dir.
{
  pkgs,
  version ? "1.28.0",
}:

let
  inherit (pkgs) lib stdenv fetchFromGitHub cmake python3 git;
in
stdenv.mkDerivation {
  pname = "onnxruntime-static";
  inherit version;

  src = fetchFromGitHub {
    owner = "microsoft";
    repo = "onnxruntime";
    tag = "v${version}";
    hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
  };

  nativeBuildInputs = [
    cmake
    python3
    git
  ];

  buildInputs = [
    pkgs.zlib
  ];

  # ONNX's build.sh drives cmake; keep the graph minimal (CPU, no python/tests).
  buildPhase = ''
    runHook preBuild
    osxArch=""
    if [ "$(uname -m)" = "arm64" ] && [ "${stdenv.hostPlatform.config}" = "x86_64-apple-darwin" ]; then
      osxArch="--osx_arch x86_64"
    elif [ "${stdenv.hostPlatform.config}" = "aarch64-apple-darwin" ]; then
      osxArch="--osx_arch arm64"
    elif [ "${stdenv.hostPlatform.config}" = "x86_64-apple-darwin" ]; then
      osxArch="--osx_arch x86_64"
    fi
    ./build.sh \
      --config Release \
      --parallel \
      --skip_tests \
      --compile_no_warning_as_error \
      --build_dir "$NIX_BUILD_TOP/build" \
      $osxArch \
      --cmake_extra_defines \
        onnxruntime_BUILD_SHARED_LIB=OFF \
        onnxruntime_ENABLE_PYTHON=OFF \
        onnxruntime_BUILD_UNIT_TESTS=OFF \
        CMAKE_OSX_DEPLOYMENT_TARGET=14.0
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p "$out/lib"
    libdir="$(find "$NIX_BUILD_TOP/build" -name 'libonnxruntime*.a' -print -quit | xargs dirname)"
    if [ -z "$libdir" ] || [ ! -d "$libdir" ]; then
      echo "no static onnxruntime archives under $NIX_BUILD_TOP/build" >&2
      find "$NIX_BUILD_TOP/build" -name 'libonnxruntime*.a' >&2 || true
      exit 1
    fi
    cp -a "$libdir"/libonnxruntime*.a "$out/lib/"
    runHook postInstall
  '';

  # The upstream build is large; allow long builds in CI/sandbox.
  __darwinAllowLocalNetworking = true;
  dontFixCmake = true;

  meta = {
    description = "Static ONNX Runtime ${version} CPU libraries (source build)";
  };
}
