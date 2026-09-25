# Prebuilt Qdrant release binary. nixpkgs' qdrant derivation builds from
# source and hits the same AVX-512 codegen failure as our Rust services on
# this builder; the upstream GitHub release tarball is pinned instead.
{ pkgs, system }:

let
  artifactBySystem = {
    x86_64-linux = {
      # Static musl build: the gnu tarball needs glibc, which the minimal
      # nix2container root does not ship.
      url = "https://github.com/qdrant/qdrant/releases/download/v1.18.2/qdrant-x86_64-unknown-linux-musl.tar.gz";
      hash = "sha256-QKavRPikllYMnSNStrKgragWqkjQeBxo9gJYLmezrqA=";
    };
    aarch64-linux = {
      url = "https://github.com/qdrant/qdrant/releases/download/v1.18.2/qdrant-aarch64-unknown-linux-musl.tar.gz";
      hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    };
  };
  artifact = artifactBySystem.${system} or null;
in
if artifact == null then
  null
else
  pkgs.stdenvNoCC.mkDerivation {
    pname = "qdrant";
    version = "1.18.2";
    src = pkgs.fetchurl {
      inherit (artifact) url hash;
    };
    nativeBuildInputs = [ pkgs.gnutar pkgs.gzip ];
    dontUnpack = true;
    installPhase = ''
      mkdir -p "$out/bin"
      tar -xzf "$src" -C "$out/bin"
      chmod +x "$out/bin/qdrant"
    '';
    meta.mainProgram = "qdrant";
  }
