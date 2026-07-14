{
  lib,
  buildImage,
  buildEnv,
  compiler,
  cacert,
  stdenv,
  bwrap ? null,
  imageName ? "nudox/compiler-daemon",
  imageTag ? "latest",
}:

let
  # Merge all runtime dependencies into a single root filesystem.
  # pathsToLink = [ "/" ] ensures every input path is linked into the
  # container root (bin/, etc/, lib/, share/, …).
  rootEnv = buildEnv {
    name = "compiler-image-root";
    paths = [
      compiler
      cacert
      stdenv.cc.cc
    ] ++ lib.optional (bwrap != null) bwrap;
    pathsToLink = [ "/" ];
    ignoreCollisions = true;
  };
in

buildImage {
  name = imageName;
  tag = imageTag;
  maxLayers = 60;

  copyToRoot = rootEnv;

  config = {
    Entrypoint = [ "/bin/compiler-daemon" ];

    ExposedPorts = {
      "8080/tcp" = { };
    };

    Env = [
      "NUDOX_COMPILER_ADDR=0.0.0.0:8080"
      "NUDOX_ROLE=forge"
      "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      "RUST_LOG=info"
      "PATH=/bin:/usr/bin"
    ];

    Labels = {
      "org.nudox.component" = "compiler-daemon";
      "org.nudox.description" = "NuDox multi-language compiler daemon";
    };
  };
}
