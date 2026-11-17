# Import from the pinned Backend source: pkgs.callPackage ./package.nix { }.
{ runCommand, makeWrapper, python3 }:
runCommand "nudox-dogfood-telemetry-v1" { nativeBuildInputs = [ makeWrapper ]; } ''
  install -Dm444 ${./relay.py} "$out/libexec/nudox-telemetry/relay.py"
  install -Dm444 ${./sender.py} "$out/libexec/nudox-telemetry/sender.py"
  mkdir -p "$out/bin"
  makeWrapper ${python3}/bin/python3 "$out/bin/nudox-telemetry-relay" \
    --add-flags "$out/libexec/nudox-telemetry/relay.py"
  makeWrapper ${python3}/bin/python3 "$out/bin/nudox-telemetry-sender" \
    --add-flags "$out/libexec/nudox-telemetry/sender.py"
''
