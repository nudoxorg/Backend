# Small whole upstream projects for artifact-driven T2. Reuse reviewed fleet
# archive pins, but fetch only the selected project, never the whole fleet.
{ pkgs }:
let
  pins = builtins.fromJSON (builtins.readFile ./corpus-pins.json);
  matches = builtins.filter (
    pin: pin.coordinate == "golang:github.com/cespare/xxhash/v2@v2.3.0"
  ) pins;
  pin =
    assert builtins.length matches == 1;
    builtins.head matches;
  archive = pkgs.fetchurl {
    inherit (pin) url hash;
  };
in
{
  go-xxhash =
    pkgs.runCommand "nudox-surface-go-xxhash-2.3.0" { nativeBuildInputs = [ pkgs.unzip ]; }
      ''
        mkdir unpack
        unzip -q ${archive} -d unpack
        mv unpack/github.com/cespare/xxhash/v2@v2.3.0 "$out"
        test -f "$out/go.mod"
        test -f "$out/xxhash.go"
        test -f "$out/xxhash_unsafe.go"
      '';
}
