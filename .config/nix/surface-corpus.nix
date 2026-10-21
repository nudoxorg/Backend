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
  projectPin =
    coordinate:
    let
      selected = builtins.filter (candidate: candidate.coordinate == coordinate) pins;
    in
    assert builtins.length selected == 1;
    builtins.head selected;
  tarProject =
    name: coordinate: requiredFiles:
    let
      selected = projectPin coordinate;
      source = pkgs.fetchurl { inherit (selected) url hash; };
    in
    pkgs.runCommand "nudox-surface-${name}"
      {
        nativeBuildInputs = [
          pkgs.gnutar
          pkgs.gzip
        ];
      }
      ''
        mkdir "$out"
        tar -xzf ${source} --strip-components=1 -C "$out"
        ${pkgs.lib.concatMapStringsSep "\n" (
          file: "test -f \"$out\"/" + pkgs.lib.escapeShellArg file
        ) requiredFiles}
      '';
  goProject =
    name: coordinate: requiredFiles:
    let
      selected = projectPin coordinate;
      source = pkgs.fetchurl { inherit (selected) url hash; };
      archiveRoot = pkgs.lib.removePrefix "golang:" coordinate;
    in
    pkgs.runCommand "nudox-surface-${name}" { nativeBuildInputs = [ pkgs.unzip ]; } ''
      mkdir unpack
      unzip -q ${source} -d unpack
      mv "unpack/${archiveRoot}" "$out"
      ${pkgs.lib.concatMapStringsSep "\n" (
        file: "test -f \"$out\"/" + pkgs.lib.escapeShellArg file
      ) requiredFiles}
    '';
  zipProject =
    name: coordinate: requiredFiles:
    let
      selected = projectPin coordinate;
      source = pkgs.fetchurl { inherit (selected) url hash; };
    in
    pkgs.runCommand "nudox-surface-${name}" { nativeBuildInputs = [ pkgs.unzip ]; } ''
      mkdir "$out"
      unzip -q ${source} -d "$out"
      ${pkgs.lib.concatMapStringsSep "\n" (
        file: "test -f \"$out\"/" + pkgs.lib.escapeShellArg file
      ) requiredFiles}
    '';
in
rec {
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
  python-cachetools = tarProject "python-cachetools-7.2.0" "pypi:cachetools@7.2.0" [
    "pyproject.toml"
    "src/cachetools/__init__.py"
  ];

  python-tomli = tarProject "python-tomli-2.2.1" "pypi:tomli@2.2.1" [
    "pyproject.toml"
    "src/tomli/_parser.py"
  ];

  python-itsdangerous = tarProject "python-itsdangerous-2.2.0" "pypi:itsdangerous@2.2.0" [
    "pyproject.toml"
    "src/itsdangerous/url_safe.py"
    "src/itsdangerous/signer.py"
  ];

  rust-itoa = tarProject "rust-itoa-1.0.14" "cargo:itoa@1.0.14" [
    "Cargo.toml"
    "src/lib.rs"
  ];

  rust-ryu = tarProject "rust-ryu-1.0.18" "cargo:ryu@1.0.18" [
    "Cargo.toml"
    "src/buffer/mod.rs"
  ];

  go-uuid = goProject "go-uuid-1.6.0" "golang:github.com/google/uuid@v1.6.0" [
    "go.mod"
    "version4.go"
    "uuid.go"
  ];

  rust-itoa-vendor = pkgs.rustPlatform.importCargoLock {
    lockFile = ../../tests/surface/cargo-locks/rust-itoa.lock;
  };
  rust-ryu-vendor = pkgs.rustPlatform.importCargoLock {
    lockFile = ../../tests/surface/cargo-locks/rust-ryu.lock;
  };

  go-sync = goProject "go-sync-0.11.0" "golang:golang.org/x/sync@v0.11.0" [
    "go.mod"
    "semaphore/semaphore.go"
    "singleflight/singleflight.go"
  ];

  c-rxi-map = tarProject "c-rxi-map-d6c355f" "rxi-map" [
    "src/map.c"
    "src/map.h"
  ];
  cpp-pugixml = tarProject "cpp-pugixml-1.14" "pugixml" [
    "src/pugixml.cpp"
    "src/pugixml.hpp"
  ];
  java-jsr305 = zipProject "java-jsr305-3.0.2" "maven:com.google.code.findbugs:jsr305@3.0.2" [
    "javax/annotation/Nonnull.java"
  ];
  csharp-tinyioc = zipProject "csharp-tinyioc-1.3.0" "nuget:tinyioc@1.3.0" [
    "TinyIoC.nuspec"
    "Content/TinyIoC.cs"
  ];

  typescript-query-core =
    tarProject "typescript-query-core-5.62.8" "npm:@tanstack/query-core@5.62.8"
      [
        "package.json"
        "src/queryClient.ts"
        "src/retryer.ts"
        "LICENSE"
      ];

}
