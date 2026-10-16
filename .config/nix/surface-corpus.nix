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
  blinkerPin = builtins.filter (candidate: candidate.coordinate == "pypi:blinker@1.9.0") pins;
  blinkerArchive =
    assert builtins.length blinkerPin == 1;
    pkgs.fetchurl { inherit (builtins.head blinkerPin) url hash; };
  percentEncodingPin = builtins.filter (
    candidate: candidate.coordinate == "cargo:percent-encoding@2.3.2"
  ) pins;
  percentEncodingArchive =
    assert builtins.length percentEncodingPin == 1;
    pkgs.fetchurl { inherit (builtins.head percentEncodingPin) url hash; };
  javaPin = builtins.filter (
    candidate: candidate.coordinate == "maven:com.google.code.findbugs:jsr305@3.0.2"
  ) pins;
  javaArchive =
    assert builtins.length javaPin == 1;
    pkgs.fetchurl { inherit (builtins.head javaPin) url hash; };
in
{
  java-jsr305 =
    pkgs.runCommand "nudox-surface-java-jsr305-3.0.2" { nativeBuildInputs = [ pkgs.unzip ]; }
      ''
        mkdir "$out"
        unzip -q ${javaArchive} -d "$out"
        test -f "$out/javax/annotation/Nonnull.java"
        test -f "$out/javax/annotation/Nullable.java"
      '';
  rust-percent-encoding =
    pkgs.runCommand "nudox-surface-rust-percent-encoding-2.3.2"
      {
        nativeBuildInputs = [
          pkgs.gnutar
          pkgs.gzip
        ];
      }
      ''
        mkdir "$out"
        tar -xzf ${percentEncodingArchive} --strip-components=1 -C "$out"
        test -f "$out/Cargo.toml"
        test -f "$out/src/lib.rs"
        test -f "$out/src/ascii_set.rs"
      '';
  python-blinker =
    pkgs.runCommand "nudox-surface-python-blinker-1.9.0"
      {
        nativeBuildInputs = [
          pkgs.gnutar
          pkgs.gzip
        ];
      }
      ''
        mkdir "$out"
        tar -xzf ${blinkerArchive} --strip-components=1 -C "$out"
        test -f "$out/pyproject.toml"
        test -f "$out/src/blinker/base.py"
        test -f "$out/src/blinker/_utilities.py"
      '';
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
