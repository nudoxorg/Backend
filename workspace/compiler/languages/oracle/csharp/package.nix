# The Roslyn C# oracle as a real Nix package.
#
# `dotnet publish` of this project is what the producer runs as
# `dotnet oracle.dll`. A packaged `lindsey.app` that does not ship that
# assembly fails every C# package with a spawn miss, and a scavenged
# pre-handshake `oracle.dll` fails the same way the schema-0 Go binary did:
# every field on the Rust mirror is `#[serde(default)]`. This recipe is the
# one artifact both `nix build .#csharp-oracle` and the cargo-bundle wrapper
# consume.
{
  pkgs,
  dotnet,
  src,
}:

pkgs.buildDotnetModule {
  pname = "nudox-csharp-oracle";
  version = "0.0.0";

  inherit src;

  projectFile = "oracle.csproj";
  nugetDeps = ./deps.json;

  dotnet-sdk = dotnet;
  dotnet-runtime = dotnet.runtime or dotnet;

  # Producer contract is `dotnet oracle.dll`, not an apphost exe.
  executables = [ ];
  useAppHost = false;
  selfContainedBuild = false;

  # packages.lock.json was produced without `--runtime`; Nix restore always
  # passes one. Force-evaluate rather than fail NU1004.
  dotnetRestoreFlags = [ "--force-evaluate" ];

  # Schema emit is checked against the installed publish output — `find` over
  # the build tree also hits `obj/**/refint/oracle.dll`, which is not a
  # runnable framework-dependent assembly.
  doCheck = false;
  nativeBuildInputs = [ pkgs.jq ];

  postInstall = ''
    dll="$(find "$out" -name oracle.dll ! -path '*/ref/*' ! -path '*/refint/*' | head -n 1)"
    if [ -z "$dll" ]; then
      echo "csharp oracle install is missing oracle.dll" >&2
      find "$out" -type f >&2 || true
      exit 1
    fi

    required="$(sed -n 's/.*SchemaFormat = \([0-9][0-9]*\).*/\1/p' Program.cs | head -n 1)"
    if [ -z "$required" ]; then
      echo "could not read SchemaFormat from Program.cs" >&2
      exit 1
    fi

    mkdir -p "$TMPDIR/fixture"
    cat > "$TMPDIR/fixture/Hello.cs" <<'EOF'
    public static class Hello
    {
        public static string Greet() => "ok";
    }
    EOF

    "${dotnet}/bin/dotnet" "$dll" --mode source --root "$TMPDIR/fixture" --assembly-name fixture \
      > "$TMPDIR/oracle.json"

    emitted="$(jq -r '.format' "$TMPDIR/oracle.json")"
    if [ "$emitted" != "$required" ]; then
      echo "oracle.dll emitted format=$emitted, source declares $required" >&2
      cat "$TMPDIR/oracle.json" >&2
      exit 1
    fi
  '';

  meta = {
    description = "Roslyn C# extraction oracle spawned by the C# producer";
  };
}
