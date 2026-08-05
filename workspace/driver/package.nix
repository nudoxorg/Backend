# Nix package for the Cargo-only driver service, which replaced the deleted
# workspace/server crate while keeping the public flake package name stable.
{
  mkRustService,
  src,
  version,
}:

mkRustService {
  pname = "nudox-driver";
  inherit version src;
  cargoPackage = "driver";
  mainProgram = "driver";
  description = "NuDox backend HTTP driver and indexing service";
}
