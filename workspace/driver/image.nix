# nix2container image for the backend driver service.
{
  mkServiceImage,
  buildImage,
  server,
}:

mkServiceImage {
  inherit buildImage;
  name = "backend";
  package = server;
  entrypoint = [ "/bin/driver" ];
  imageName = "nudox/backend";
  ports = [ "8080/tcp" ];
  env = [
    "NUDOX_SERVING_ADDRESS=0.0.0.0:8080"
    "NUDOX_ROLE=all"
    "OTEL_SERVICE_NAME=nudox-backend"
    "OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318"
    "RUST_LOG=info"
    "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
    "PATH=/bin:/usr/bin"
  ];
  labels = {
    "org.nudox.component" = "backend";
    "org.nudox.description" = "NuDox backend driver service";
  };
}
