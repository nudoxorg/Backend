# NuDox registry library package (no main binary).
{
  mkRustService,
  src,
  version,
}:

(mkRustService {
  pname = "nudox-registry";
  inherit version src;
  cargoPackage = "registry";
  mainProgram = "";
  description = "NuDox registry library";
}).overrideAttrs
  {
    postFixup = "";
  }
