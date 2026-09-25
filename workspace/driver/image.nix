# Nix2container image for the backend serving service.
{
  mkServiceImage,
  buildImage,
  server,
  pkgs,
}:

let
  qdrantBinary = import ../../nix/qdrant-binary.nix {
    inherit pkgs;
    system = pkgs.stdenv.hostPlatform.system;
  };

  dataRoot = "/var/lib/nudox";

  backendRunner = pkgs.writeShellScriptBin "nudox-backend-run" ''
    set -eu

    mkdir -p /tmp \
      "${dataRoot}/catalog" \
      "${dataRoot}/data" \
      "${dataRoot}/blobs" \
      "${dataRoot}/qdrant" \
      "${dataRoot}/home" \
      "${dataRoot}/cargo"
    chmod 1777 /tmp

    # No guest rootfs: Rust packages are indexed in-process, which runs
    # `cargo metadata` and build-script `cargo check`. Those need cargo,
    # rustc, and a `cc` linker, plus a writable cargo home.
    export HOME="${dataRoot}/home"
    export CARGO_HOME="${dataRoot}/cargo"
    export PATH="${pkgs.cargo}/bin:${pkgs.rustc}/bin:${pkgs.stdenv.cc}/bin:''${PATH:-/bin}"

    export NUDOX_DEFINITIVE__ENDPOINTS__CATALOG_DIRECTORY="${dataRoot}/catalog"
    export NUDOX_DEFINITIVE__DATA_DIRECTORY="${dataRoot}/data"
    export NUDOX_DEFINITIVE__ENDPOINTS__OBJECT_STORE="file://${dataRoot}/blobs"

    ${qdrantBinary}/bin/qdrant --disable-telemetry &
    qdrant_pid=$!

    i=0
    while [ "$i" -lt 120 ]; do
      if ${pkgs.curl}/bin/curl -sf http://127.0.0.1:6333/readyz >/dev/null 2>&1; then
        break
      fi
      if ! kill -0 "$qdrant_pid" 2>/dev/null; then
        echo "qdrant exited before ready" >&2
        exit 1
      fi
      i=$((i + 1))
      sleep 1
    done

    if ! ${pkgs.curl}/bin/curl -sf http://127.0.0.1:6333/readyz >/dev/null 2>&1; then
      echo "qdrant did not become ready" >&2
      exit 1
    fi

    exec ${server}/bin/nudox-serve "$@"
  '';
in

mkServiceImage {
  inherit buildImage;
  name = "backend";
  package = backendRunner;
  entrypoint = [ "/bin/nudox-backend-run" ];
  imageName = "nudox/backend";
  ports = [ "8080/tcp" ];
  extraPaths = [
    server
    qdrantBinary
    pkgs.curl
    pkgs.coreutils
    pkgs.cargo
    pkgs.rustc
    pkgs.stdenv.cc
  ];
  env = [
    "NUDOX_SERVING_ADDRESS=0.0.0.0:8080"
    "NUDOX_ROLE=all"
    "OTEL_SERVICE_NAME=nudox-backend"
    "OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318"
    "RUST_LOG=info"
    "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
    "PATH=/bin:/usr/bin"
    "QDRANT__SERVICE__HTTP_PORT=6333"
    "QDRANT__SERVICE__GRPC_PORT=6334"
    "QDRANT__STORAGE__STORAGE_PATH=/var/lib/nudox/qdrant"
  ];
  labels = {
    "org.nudox.component" = "backend";
    "org.nudox.description" = "NuDox serving service";
  };
}
