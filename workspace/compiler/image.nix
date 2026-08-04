# nix2container image for packages.compilerImage — entrypoint /bin/compiler-daemon.
{
  lib,
  mkServiceImage,
  buildImage,
  compiler,
  bwrap ? null,
}:

mkServiceImage {
  inherit buildImage;
  name = "compiler";
  package = compiler;
  entrypoint = [ "/bin/compiler-daemon" ];
  imageName = "nudox/compiler-daemon";
  ports = [ "8080/tcp" ];
  extraPaths = lib.optional (bwrap != null) bwrap;
  env = [
    "NUDOX_COMPILER_ADDR=0.0.0.0:8080"
    "NUDOX_ROLE=forge"
    "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
    "RUST_LOG=info"
    "PATH=/bin:/usr/bin"
  ];
  labels = {
    "org.nudox.component" = "compiler-daemon";
    "org.nudox.description" = "NuDox multi-language compiler daemon";
  };
}
