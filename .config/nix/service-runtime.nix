# Headless, same-source operator runtime. Observations below are build inputs,
# not the formal release-attestation receipts produced by tools/package.
{
  pkgs,
  workspaceRoot,
  workspaceSource,
  stableRustPlatform,
  gpuiOutputHashes,
  boundedProductCargoPreBuild,
  sourceRevision ? null,
}:
let
  lib = pkgs.lib;
  version =
    (builtins.fromTOML (builtins.readFile (workspaceRoot + "/Cargo.toml"))).workspace.package.version;
  target = pkgs.stdenv.targetPlatform.rust.cargoShortTarget;
  common = {
    inherit version;
    src = workspaceSource;
    doCheck = false;
    # Stylo generates Servo properties with its checked-in Python modules.
    # SQLite compiles its bundled parser with the stdenv C compiler.
    nativeBuildInputs = [ pkgs.python3 ];
    PYTHON3 = "${pkgs.python3}/bin/python3";
    preBuild = boundedProductCargoPreBuild + ''
      export RUSTFLAGS="''${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$NIX_BUILD_TOP=/build/nudox --remap-path-prefix=$CARGO_HOME=/cargo"
    '';
  };
  css = stableRustPlatform.buildRustPackage (
    common
    // {
      pname = "nudox-stylo-worker-build";
      cargoLock = {
        lockFile = workspaceRoot + "/Cargo.lock";
        outputHashes = gpuiOutputHashes;
      };
      buildPhase = ''
        runHook preBuild
        cargo build --locked --offline --release --target ${target} -j "$NIX_BUILD_CORES" -p backend-frontend-stylo --bin backend-stylo-worker
        cargo build --locked --offline --release --target ${target} -j "$NIX_BUILD_CORES" -p backend-compile --example stylo_identity_observer
        runHook postBuild
      '';
      installPhase = ''
        mkdir -p "$out/bin"
        cp target/${target}/release/backend-stylo-worker target/${target}/release/examples/stylo_identity_observer "$out/bin/"
      '';
    }
  );
  sqlite = stableRustPlatform.buildRustPackage (
    common
    // {
      pname = "nudox-sqlite-worker-build";
      cargoRoot = "frontends/sqlite";
      cargoLock.lockFile = workspaceRoot + "/frontends/sqlite/Cargo.lock";
      buildPhase = ''
        runHook preBuild
        cargo build --locked --offline --release --target ${target} -j "$NIX_BUILD_CORES" --manifest-path frontends/sqlite/Cargo.toml --target-dir "$PWD/target-sqlite" --bin sqlite-worker --example installed_basis_observer
        runHook postBuild
      '';
      installPhase = ''
        mkdir -p "$out/bin"
        cp target-sqlite/${target}/release/sqlite-worker target-sqlite/${target}/release/examples/installed_basis_observer "$out/bin/"
      '';
    }
  );
  # Relocate only after the individual derivations' normal Nix fixups. This
  # output's bytes are frozen before they are observed and owner compilation.
  workers =
    pkgs.runCommand "nudox-service-native-workers"
      {
        nativeBuildInputs = [
          pkgs.patchelf
          pkgs.binutils
          pkgs.jq
        ];
        dontFixup = true;
      }
      ''
        mkdir -p "$out/bin" "$out/lib" "$out/observers" "$out/share"
        cp ${css}/bin/backend-stylo-worker "$out/bin/"
        cp ${sqlite}/bin/sqlite-worker "$out/bin/backend-sqlite-worker"
        cp ${css}/bin/stylo_identity_observer ${sqlite}/bin/installed_basis_observer "$out/observers/"
        cp -L ${pkgs.stdenv.cc.cc.lib}/lib/libgcc_s.so.1 "$out/lib/libgcc_s.so.1"
        chmod u+w "$out/bin/"* "$out/lib/"*
        patchelf --set-rpath '$ORIGIN' "$out/lib/libgcc_s.so.1"
        patchelf --set-rpath '$ORIGIN/../lib' "$out/bin/backend-stylo-worker"
        patchelf --set-rpath '$ORIGIN/../lib' "$out/bin/backend-sqlite-worker"
        # Admission only accounts for libgcc as a private dependency. Refuse any
        # other non-platform library rather than silently omitting it from identity.
        for worker in "$out/bin/backend-stylo-worker" "$out/bin/backend-sqlite-worker"; do
          readelf -d "$worker" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort > needed.txt
          grep -Fxq libgcc_s.so.1 needed.txt
          while IFS= read -r dependency; do
            if [ "$dependency" != libgcc_s.so.1 ]; then
              grep -Fxq "$dependency" ${workspaceSource}/tools/package/linux_system_sonames.txt
            fi
          done < needed.txt
          test "$(patchelf --print-rpath "$worker")" = '$ORIGIN/../lib'
          readelf -l "$worker" | grep -q 'Requesting program interpreter: /'
        done
        readelf -d "$out/lib/libgcc_s.so.1" | grep -q 'Library soname: \[libgcc_s.so.1\]'
        readelf -d "$out/lib/libgcc_s.so.1" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | while IFS= read -r dependency; do
          grep -Fxq "$dependency" ${workspaceSource}/tools/package/linux_system_sonames.txt
        done
        test "$(patchelf --print-rpath "$out/lib/libgcc_s.so.1")" = '$ORIGIN'
        "$out/observers/stylo_identity_observer" "$out/bin/backend-stylo-worker" "$out/lib/libgcc_s.so.1" > "$out/share/stylo.json"
        "$out/observers/installed_basis_observer" "$out/bin/backend-sqlite-worker" "$out/lib/libgcc_s.so.1" > "$out/share/sqlite.json"
        jq -e '.state == "observed-helper" and (.identity | test("^[0-9a-f]{64}$") and (test("^0+$") | not))' "$out/share/stylo.json" >/dev/null
        jq -e '.state == "observed-build-helper" and (.artifact_identity | test("^[0-9a-f]{64}$") and (test("^0+$") | not))' "$out/share/sqlite.json" >/dev/null
        jq -e '.basis.engine == "Syntaqlite4e4de76" and .basis.native_revision == "4e4de76312429fb2235ae093ab2125914320d5bd" and .basis.recipe == "syntaqlite-sqlite-fmt-analysis-dqs-default-v1" and .basis.settings.Sqlite == {"version":2147483647,"cflags":0,"analyzer_dqs_fallback":true} and (.basis.support_sha256 | length == 32 and any(. != 0))' "$out/share/sqlite.json" >/dev/null
        lock_sha=$(sha256sum ${workspaceSource}/frontends/sqlite/Cargo.lock | cut -d' ' -f1)
        test "$(jq -r '.basis.lock_sha256 | map(. as $n | "0123456789abcdef"[($n / 16 | floor):(($n / 16 | floor)+1)] + "0123456789abcdef"[($n % 16):(($n % 16)+1)]) | join("")' "$out/share/sqlite.json")" = "$lock_sha"
      '';
in
stableRustPlatform.buildRustPackage (
  common
  // {
    pname = "nudox-backend-service-runtime";
    cargoLock = {
      lockFile = workspaceRoot + "/Cargo.lock";
      outputHashes = gpuiOutputHashes;
    };
    nativeBuildInputs = common.nativeBuildInputs ++ [ pkgs.jq ];
    cargoBuildFlags = [
      "--package"
      "backend-locald"
      "--bin"
      "backend-locald"
      "--package"
      "backend-cli"
      "--bin"
      "backend-cli"
      "--package"
      "backend-metrics"
      "--bin"
      "backend-metrics"
      "--package"
      "backend-mcp"
      "--bin"
      "backend-mcp"
    ];
    # Fixed worker bytes are owner inputs, not artifacts generated by owner build.
    preBuild = common.preBuild + ''
      export NUDOX_BUILD_STYLO_WORKER_ID=$(jq -er .identity ${workers}/share/stylo.json)
      export NUDOX_BUILD_SQLITE_WORKER_ID=$(jq -er .artifact_identity ${workers}/share/sqlite.json)
      export NUDOX_BUILD_SQLITE_BASIS_JSON=$(jq -c .basis ${workers}/share/sqlite.json)
      test "''${#NUDOX_BUILD_SQLITE_BASIS_JSON}" -le 8192
    '';
    # Normal fixup may mutate ELF bytes after the identities were observed.
    # Rust links with the pinned Nix toolchain; leave all installed bytes intact.
    dontStrip = true;
    dontPatchELF = true;
    installPhase = ''
        mkdir -p "$out/bin" "$out/lib" "$out/share/nudox"
        cp ${workers}/bin/* "$out/bin/"
        cp ${workers}/lib/* "$out/lib/"
        for binary in backend-locald backend-cli backend-metrics backend-mcp; do
          cp "target/${target}/release/$binary" "$out/bin/$binary"
        done
        cp ${workers}/share/*.json "$out/share/nudox/"
        sha256sum Cargo.lock frontends/sqlite/Cargo.lock > "$out/share/nudox/lock-sha256.txt"
      printf '%s\n' ${
        lib.escapeShellArg (if sourceRevision == null then "unknown" else sourceRevision)
      } > "$out/share/nudox/source-revision.txt"
    '';
    postFixup = ''
      cmp "$out/bin/backend-stylo-worker" ${workers}/bin/backend-stylo-worker
      cmp "$out/bin/backend-sqlite-worker" ${workers}/bin/backend-sqlite-worker
      cmp "$out/lib/libgcc_s.so.1" ${workers}/lib/libgcc_s.so.1
      ${workers}/observers/stylo_identity_observer "$out/bin/backend-stylo-worker" "$out/lib/libgcc_s.so.1" | jq -S . > css-after.json
      ${workers}/observers/installed_basis_observer "$out/bin/backend-sqlite-worker" "$out/lib/libgcc_s.so.1" | jq -S . > sqlite-after.json
      jq -S . ${workers}/share/stylo.json | cmp - css-after.json
      jq -S . ${workers}/share/sqlite.json | cmp - sqlite-after.json
      (cd "$out"; sha256sum bin/* lib/* | LC_ALL=C sort) > "$out/share/nudox/binary-sha256.txt"
      (cd ${workers}; sha256sum observers/* | LC_ALL=C sort) > "$out/share/nudox/observer-sha256.txt"
    '';
    passthru.nativeWorkers = workers;
  }
)
