{
  lib,
  buildImage,
  buildEnv,
  server,
  cacert,
  stdenv,
  imageName ? "nudox/backend",
  imageTag ? "latest",
}:

let
  # Merge runtime dependencies into a single root filesystem.
  # pathsToLink = [ "/" ] ensures every input path is linked into the
  # container root (bin/, etc/, lib/, share/, …).
  rootEnv = buildEnv {
    name = "backend-image-root";
    paths = [
      server
      cacert
      stdenv.cc.cc
    ];
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
    Entrypoint = [ "/bin/server" ];

    ExposedPorts = {
      "8080/tcp" = { };
    };

    Env = [
      # Container default: listen on all interfaces (host default is 127.0.0.1).
      "NUDOX_SERVING_ADDRESS=0.0.0.0:8080"
      "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      "RUST_LOG=info"
      "PATH=/bin:/usr/bin"
    ];

    Labels = {
      "org.nudox.component" = "backend";
      "org.nudox.description" = "NuDox backend HTTP server";
    };
  };
}
