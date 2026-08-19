# The Go extraction oracle as a real Nix package.
#
# `tests/go-oracle` used to be compile+vet only, and it installed whatever
# name `go install` derived from the source directory. The producer looks up
# `nudox-go-oracle` on PATH (or `NUDOX_GO_ORACLE_BIN`), so a packaged
# `lindsey.app` that does not ship that exact name fails every Go package
# with a spawn error. This recipe is the one artifact both `nix build
# .#go-oracle` and the cargo-bundle wrapper consume.
{
  pkgs,
  goToolchain,
  src,
  # Cross-compile Mach-O (e.g. "amd64" on aarch64-darwin for a universal
  # oracle). Null keeps the host GOARCH and the schema handshake check.
  goarch ? null,
}:

pkgs.buildGoModule {
  pname = "nudox-go-oracle";
  # SchemaVersion in main.go is the number consumers handshake on, not this.
  version = "0.0.0";

  inherit src;

  go = goToolchain;

  env = pkgs.lib.optionalAttrs (goarch != null) {
    GOARCH = goarch;
    CGO_ENABLED = "0";
  };

  # buildGoModule also derives GOARCH from stdenv.hostPlatform and would
  # overwrite the env above during `go install`. Re-export immediately
  # before both the build and install `go` invocations.
  preBuild = pkgs.lib.optionalString (goarch != null) ''
    export GOARCH=${goarch}
    export CGO_ENABLED=0
  '';
  preInstall = pkgs.lib.optionalString (goarch != null) ''
    export GOARCH=${goarch}
    export CGO_ENABLED=0
  '';

  # Recompute by pointing this at `pkgs.lib.fakeHash`, building, and reading
  # the real digest off the hash-mismatch error. Bump it the same way
  # whenever go.sum changes.
  vendorHash = "sha256-oZyvmlZ9m8v3h1UIL9s9Ko26yZF3f0muBF4nV4t3Y7o=";

  nativeBuildInputs = [ pkgs.jq ];

  # `go install ./...` names the binary after the source directory. The
  # producer looks up `nudox-go-oracle` (see src/go/producer.rs). The src
  # passed in is renamed to `nudox-go-oracle` for that reason; this rename
  # is the backstop if a caller forgets.
  postInstall = ''
    if [ -e "$out/bin/nudox-go-oracle" ]; then
      :
    elif [ -e "$out/bin/oracle" ]; then
      mv "$out/bin/oracle" "$out/bin/nudox-go-oracle"
    else
      found="$(find "$out/bin" -maxdepth 1 -type f -perm -u+x | head -n 1)"
      if [ -z "$found" ]; then
        echo "go-oracle install produced no binary under $out/bin" >&2
        ls -la "$out/bin" >&2 || true
        exit 1
      fi
      mv "$found" "$out/bin/nudox-go-oracle"
    fi
  '';

  doCheck = goarch == null;
  checkPhase = ''
    runHook preCheck
    go vet -mod=vendor ./...

    # A compiled oracle that does not stamp the schema its own source
    # declares is the schema-0-vs-schema-2 failure: every field on the Rust
    # mirror is `#[serde(default)]`, so a silent schema 0 looks like a
    # package with no refs. This is the half `oracle_staleness.rs` cannot
    # catch — it only ever sees fixture JSON, never this binary.
    required="$(sed -n 's/^const SchemaVersion = \([0-9][0-9]*\).*/\1/p' main.go | head -n 1)"
    if [ -z "$required" ]; then
      echo "could not read const SchemaVersion from main.go" >&2
      exit 1
    fi

    mkdir -p "$TMPDIR/fixture"
    cat > "$TMPDIR/fixture/go.mod" <<'EOF'
    module example.com/fixture
    go 1.23
    EOF
    cat > "$TMPDIR/fixture/hello.go" <<'EOF'
    package fixture

    func Hello() string { return "ok" }
    EOF

    export GOCACHE="$TMPDIR/go-cache"
    export GOPROXY=off
    export GOSUMDB=off
    export GO111MODULE=on
    # Vendor for this module; -mod=mod only when the compiled binary
    # loads the fixture, which has no vendor directory.
    export GOFLAGS=-mod=vendor
    mkdir -p "$TMPDIR/oracle-bin"
    go build -o "$TMPDIR/oracle-bin/nudox-go-oracle" .
    GOFLAGS=-mod=mod "$TMPDIR/oracle-bin/nudox-go-oracle" "$TMPDIR/fixture" > "$TMPDIR/oracle.json"

    emitted="$(jq -r '.schemaVersion' "$TMPDIR/oracle.json")"
    if [ "$emitted" != "$required" ]; then
      echo "nudox-go-oracle emitted schemaVersion=$emitted, source declares $required" >&2
      cat "$TMPDIR/oracle.json" >&2
      exit 1
    fi
    runHook postCheck
  '';

  meta = {
    description = "Go extraction oracle spawned by the Go producer";
    mainProgram = "nudox-go-oracle";
  };
}
