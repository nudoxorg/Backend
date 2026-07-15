# NuDox backend HTTP server binary (cargo package "server").
{
  mkRustService,
  src,
  version,
}:

mkRustService {
  pname = "nudox-backend";
  inherit version src;
  cargoPackage = "server";
  mainProgram = "server";
  description = "NuDox backend server";
}
