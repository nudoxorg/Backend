# Nix2container image for the compiler daemon seam.
{
  mkServiceImage,
  buildImage,
  compiler,
}:

mkServiceImage {
  inherit buildImage;
  name = "compiler";
  package = compiler;
  entrypoint = [ "/bin/compiler-daemon" ];
  imageName = "nudox/compiler";
  labels = {
    "org.nudox.component" = "compiler";
    "org.nudox.description" = "NuDox compiler daemon";
  };
}
