# nix2container image for packages.backend — entrypoint /bin/server.
{
  mkServiceImage,
  buildImage,
  server,
}:

mkServiceImage {
  inherit buildImage;
  name = "backend";
  package = server;
  entrypoint = [ "/bin/server" ];
  imageName = "nudox/backend";
  ports = [ "8080/tcp" ];
  env = [
    # Container default: listen on all interfaces (host default is 127.0.0.1).
    "NUDOX_SERVING_ADDRESS=0.0.0.0:8080"
    "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
    "RUST_LOG=info"
    "PATH=/bin:/usr/bin"
  ];
  labels = {
    "org.nudox.component" = "backend";
    "org.nudox.description" = "NuDox backend HTTP server";
  };
}
