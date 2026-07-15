/*
  Compiler-daemon packaging: wraps snowydeer-imported store paths (or
  placeholders) into a derivation with /bin/compiler-daemon (+ optional
  producer-worker and language oracles + resources manifest).
*/
{
  lib,
  stdenv,

  compilerDaemon ? null,
  producerWorker ? null,

  # Oracle resource store paths (imported via snowydeer BXL).
  # Without these, Go/Java/C# producers fail at runtime with
  # ResourceNotFound — the daemon itself still starts.
  goOracle ? null,
  javaOracle ? null,
  csharpOracle ? null,
}:

let
  # Build the resources manifest at evaluation time from the provided paths.
  # Each entry is only included when its store path is non-null.
  resourceEntries = lib.filter (x: x != null) [
    (
      if goOracle != null then
        {
          name = "workspace/compiler/go-oracle";
          value = "resources/go-oracle";
        }
      else
        null
    )
    (
      if javaOracle != null then
        {
          name = "workspace/compiler/java-oracle.jar";
          value = "resources/java-oracle.jar";
        }
      else
        null
    )
    (
      if csharpOracle != null then
        {
          name = "workspace/compiler/csharp-oracle";
          value = "resources/csharp-oracle";
        }
      else
        null
    )
  ];

  resourcesManifest = builtins.toJSON (lib.listToAttrs resourceEntries);
in

stdenv.mkDerivation {
  pname = "nudox-compiler";
  version = "0.1.0";
  phases = [ "installPhase" ];

  installPhase = ''
        set -eu

        # ── Binaries ──────────────────────────────────────────────────────────
        mkdir -p "$out/bin"

        if [ -n "${toString compilerDaemon}" ] && [ -f "${toString compilerDaemon}/bin/compiler-daemon" ]; then
          cp -L "${toString compilerDaemon}/bin/compiler-daemon" "$out/bin/compiler-daemon"
          chmod +x "$out/bin/compiler-daemon"
          echo "installed compiler-daemon"
        else
          cat > "$out/bin/compiler-daemon" <<'SCRIPT'
    #!/bin/sh
    echo "FATAL: compiler-daemon not built. Import it via snowydeer:" >&2
    echo "  buck2 bxl //snowydeer:snowydeer.bxl:main -- --target //workspace/compiler:compiler-daemon" >&2
    exit 1
    SCRIPT
          chmod +x "$out/bin/compiler-daemon"
        fi

        if [ -n "${toString producerWorker}" ] && [ -f "${toString producerWorker}/bin/producer-worker" ]; then
          cp -L "${toString producerWorker}/bin/producer-worker" "$out/bin/producer-worker"
          chmod +x "$out/bin/producer-worker"
          echo "installed producer-worker"
        fi

        # ── Oracle resources ──────────────────────────────────────────────────
        mkdir -p "$out/bin/resources"

        if [ -n "${toString goOracle}" ]; then
          go_src="${toString goOracle}"
          if [ -f "$go_src" ]; then
            # Buck2 go_binary default output IS the executable
            cp -L "$go_src" "$out/bin/resources/go-oracle"
          elif [ -f "$go_src/bin/oracle" ]; then
            cp -L "$go_src/bin/oracle" "$out/bin/resources/go-oracle"
          elif [ -f "$go_src/oracle" ]; then
            cp -L "$go_src/oracle" "$out/bin/resources/go-oracle"
          fi
          if [ -f "$out/bin/resources/go-oracle" ]; then
            chmod +x "$out/bin/resources/go-oracle"
            echo "installed go-oracle"
          else
            echo "WARNING: go-oracle store path has no recognizable binary" >&2
          fi
        fi

        if [ -n "${toString javaOracle}" ]; then
          jar_src="${toString javaOracle}"
          found=
          if [ -f "$jar_src" ]; then
            # Buck2 java_library default output IS the .jar
            cp -L "$jar_src" "$out/bin/resources/java-oracle.jar"
            found=1
          else
            for f in "$jar_src/"*.jar; do
              if [ -f "$f" ]; then
                cp -L "$f" "$out/bin/resources/java-oracle.jar"
                found=1
                break
              fi
            done
          fi
          if [ -n "$found" ]; then
            echo "installed java-oracle.jar"
          else
            echo "WARNING: java-oracle store path has no .jar files" >&2
          fi
        fi

        if [ -n "${toString csharpOracle}" ]; then
          cs_src="${toString csharpOracle}"
          if [ -d "$cs_src/publish" ]; then
            cp -rL "$cs_src/publish" "$out/bin/resources/csharp-oracle"
            echo "installed csharp-oracle"
          elif [ -d "$cs_src" ] && [ -f "$cs_src/oracle.dll" ]; then
            cp -rL "$cs_src" "$out/bin/resources/csharp-oracle"
            echo "installed csharp-oracle"
          else
            echo "WARNING: csharp-oracle store path has no publish/ or oracle.dll" >&2
          fi
        fi

        # ── Resources manifest ────────────────────────────────────────────────
        # Written next to the binary so buck_resource() in compile/producer/resource.rs
        # can resolve it: it looks for <binary-name>.resources.json in the same dir.
        cat > "$out/bin/compiler-daemon.resources.json" <<MANIFEST
    ${resourcesManifest}
    MANIFEST
        echo "wrote resources manifest: $out/bin/compiler-daemon.resources.json"
  '';

  meta = {
    description = "NuDox compiler daemon, producer-worker, and language oracles";
    mainProgram = "compiler-daemon";
    platforms = lib.platforms.all;
  };
}
