# Compatibility package name for the serving binary.
#
# The former driver crate was folded into `index::server`; keep the public
# flake attribute `packages.server` stable for the integration checks.
{
  mkRustService,
  src,
  version,
}:

mkRustService {
  pname = "nudox-serve";
  inherit version src;
  cargoPackage = "index";
  mainProgram = "nudox-serve";
  description = "NuDox backend HTTP serving service";
}
