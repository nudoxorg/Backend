{
  description = "NuDox development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    nuenv = {
      url = "github:philocalyst/nuenv";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # The Nix module system keeps per-platform outputs small, explicit, and
    # composable without another hand-rolled `forAllSystems` helper.
    flake-parts.url = "github:hercules-ci/flake-parts";

    # nixpkgs currently ships Cargo Bundle 0.9; the GUI packaging metadata
    # requires the current 0.11 release. The crate archive is locked like any
    # other source input, so this remains a reproducible Nix binary.
    cargo-bundle = {
      url = "https://crates.io/api/v1/crates/cargo-bundle/0.11.0/download";
      flake = false;
    };

    buck2-prelude = {
      url = "github:facebookincubator/buck2-prelude?rev=4b374e200a64838660463994b079899b5094689a";
      flake = false;
    };

    # tweag/buck2.nix — provides flake.package() + nix_rust_toolchain/nix_cxx_toolchain.
    # Fetched as a plain source input so the devshell can wire it as a `path`-type
    # external cell in .buckconfig, avoiding a git fetch at Buck2 build time.
    buck2-nix = {
      url = "github:tweag/buck2.nix/038b031b84846101030b9d081445003e82e3be5c";
      flake = false;
    };

    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      fenix,
      git-hooks,
      nuenv,
      buck2-prelude,
      buck2-nix,
      nix2container,
      flake-parts,
      cargo-bundle,
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      perSystem =
        {
          system,
          config,
          ...
        }:
        let
          nixPackages = import nixpkgs {
            inherit system;
            overlays = [
              fenix.overlays.default
              nuenv.overlays.default
            ];
          };
          # Keep the oracle and all Go-based checks on one explicit toolchain.
          # The rolling `nixpkgs.go` alias would make this dependency implicit.
          goToolchain = nixPackages.go_1_26;
          fenixPackages = fenix.packages.${system};

          cargoBundleUnstable = nixPackages.rustPlatform.buildRustPackage {
            pname = "cargo-bundle";
            version = "0.11.0";
            src = cargo-bundle;
            cargoHash = "sha256-VfXJNsopV7SEjhnMyz4D2l2HBBnQv2I9xeqhhD4R7aw=";
            # Flake tarball inputs arrive as extensionless archives; unpack
            # them into the build root (not a `source/` subdir we then `cd`
            # into) so buildRustPackage's `cargo vendor` / lockfile-consistency
            # hooks find `Cargo.toml`/`Cargo.lock` at the working directory
            # they expect. `runHook postUnpack` is required: overriding
            # unpackPhase skips the default `postUnpackHooks`, without which
            # `cargoSetupPostUnpackHook` never copies the vendored deps in.
            unpackPhase = ''
              mkdir source
              ${nixPackages.gnutar}/bin/tar -xzf "$src" -C source --strip-components=1
              cp -R source/. .
              runHook postUnpack
            '';
            doCheck = false;

            # `cargo-bundle` depends on `openssl-sys`, which finds OpenSSL by
            # asking `pkg-config`. Neither is in a `buildRustPackage` sandbox
            # unless it is put there, so on Linux this derivation failed with
            #
            #     Could not find directory of OpenSSL installation … it looks
            #     like you're compiling on Linux and also targeting Linux.
            #     Currently this requires the `pkg-config` utility …
            #
            # and, because this package is in the devshell's `packages`, the
            # failure was not "cargo bundle is unavailable" but **`nix develop`
            # itself refusing to start**: no shell, no build, nothing, for every
            # Linux contributor.
            #
            # macOS does not see it — `openssl-sys` resolves against
            # Security.framework there — which is exactly the shape of bug that
            # reaches a Linux tree unnoticed.
            nativeBuildInputs = [ nixPackages.pkg-config ];
            buildInputs = [ nixPackages.openssl ];
          };

          # The sandbox's real tests invoke the pinned smolvm CLI as the
          # subprocess that boots each guest. Keep the CLI in the Nix shell so
          # an opt-in VM test has a reproducible launcher instead of relying
          # on a Homebrew/global binary.
          smolvmSource = nixPackages.fetchFromGitHub {
            owner = "smol-machines";
            repo = "smolvm";
            rev = "56bb13b99dfe65a58df158bf241077a5024d3a64";
            hash = "sha256-TMShe9ODCreAU9G12+xSWBOKkhso11q/hZfbRmQaLfk=";
          };

          isLinuxSystem = builtins.elem system [
            "aarch64-linux"
            "x86_64-linux"
          ];

          smolvmBinary =
            if isLinuxSystem then
              nixPackages.rustPlatform.buildRustPackage {
                pname = "smolvm";
                version = "1.6.1";
                src = smolvmSource;
                cargoLock.lockFile = "${smolvmSource}/Cargo.lock";
                nativeBuildInputs = [ nixPackages.pkg-config ];
                buildInputs = [ nixPackages.libkrun ];
                buildAndTestSubdir = ".";
                cargoBuildFlags = [
                  "--bin"
                  "smolvm"
                ];
                doCheck = false;
                installPhase = ''
                  install -Dm755 target/release/smolvm $out/bin/smolvm
                '';
              }
            else
              null;

          reindeerVersion = "v2026.07.13.00";
          reindeerArtifactDetails = {
            "aarch64-darwin" = {
              platform = "aarch64-apple-darwin";
              hash = "sha256-SL2EJ5B+90i3O3Pw4NAcI4wnyUMp29yl0qo4wmM7jsg=";
            };
            "x86_64-darwin" = {
              platform = "x86_64-apple-darwin";
              hash = "sha256-AScqudALl5webUyMJ2v/Lw0CCU+q7nJ1JrrAAQP2CSo=";
            };
            "aarch64-linux" = {
              platform = "aarch64-unknown-linux-gnu";
              hash = "sha256-capEdrcgi4+Z17BPpKunlyKE6C1eUyGubIYn/Y2efwk=";
            };
            "x86_64-linux" = {
              platform = "x86_64-unknown-linux-gnu";
              hash = "sha256-IIfm1olh7P7VJOPiJM156g5wkdDgSE1DIH93/1UkE2k=";
            };
          };

          makeReindeerBinaryDerivation =
            nixPackages:
            let
              artifactDetails = reindeerArtifactDetails.${nixPackages.stdenv.hostPlatform.system};
            in
            nixPackages.stdenvNoCC.mkDerivation {
              pname = "reindeer";
              version = reindeerVersion;
              src = nixPackages.fetchurl {
                url = "https://github.com/facebookincubator/reindeer/releases/download/${reindeerVersion}/reindeer-${artifactDetails.platform}.zst";
                hash = artifactDetails.hash;
              };
              nativeBuildInputs = [ nixPackages.zstd ];
              dontUnpack = true;
              installPhase = ''
                mkdir -p $out/bin
                zstd -d $src -o $out/bin/reindeer
                chmod +x $out/bin/reindeer
              '';
              meta.mainProgram = "reindeer";
            };

          buck2Version = "2026-07-01";
          buck2ArtifactDetails = {
            "aarch64-darwin" = {
              platform = "aarch64-apple-darwin";
              hash = "sha256-cjgWmrQiLagv4lN3dgEl2l7wDmchntMGBgFyfztRLWA=";
            };
            "x86_64-darwin" = {
              platform = "x86_64-apple-darwin";
              hash = "sha256-7czaJhavbkHkv/1JsXPbi3IeNptgZHMcGLdt14de2lU=";
            };
            "aarch64-linux" = {
              platform = "aarch64-unknown-linux-gnu";
              hash = "sha256-zMbZcliSzTyfdKxgxx5A1R3tibdHhAZLLdYNNJ6gu24=";
            };
            "x86_64-linux" = {
              platform = "x86_64-unknown-linux-gnu";
              hash = "sha256-XQzRG7QQHId6nSNCcFsXme6j2oU8uqAOoNCoflPFwzg=";
            };
          };

          makeBuck2BinaryDerivation =
            nixPackages:
            let
              artifactDetails = buck2ArtifactDetails.${nixPackages.stdenv.hostPlatform.system};
            in
            nixPackages.stdenvNoCC.mkDerivation {
              pname = "buck2";
              version = buck2Version;
              src = nixPackages.fetchurl {
                url = "https://github.com/facebook/buck2/releases/download/${buck2Version}/buck2-${artifactDetails.platform}.zst";
                hash = artifactDetails.hash;
              };
              nativeBuildInputs = [ nixPackages.zstd ];
              dontUnpack = true;
              installPhase = ''
                mkdir -p $out/bin
                zstd -d $src -o $out/bin/buck2
                chmod +x $out/bin/buck2
              '';
              meta.mainProgram = "buck2";
            };

          # Shared helpers (Rust components, checks, and optional store paths).
          helpersFor =
            nixPackages: fenixPackages:
            import ./nix/lib.nix {
              pkgs = nixPackages;
              inherit fenixPackages;
            };

          # The only model artifact used by the local semantic product test.
          # Every byte is fetched at evaluation/build time with a fixed
          # revision and SHA-256; the runtime itself only reads this directory.
          semanticModel =
            let
              revision = "516f4baf13dec4ddddda8631e019b5737c8bc250";
              base = "https://huggingface.co/jinaai/jina-embeddings-v2-base-code/resolve/${revision}";
              fetch =
                file: hash:
                nixPackages.fetchurl {
                  url = "${base}/${file}";
                  sha256 = hash;
                };
              model = fetch "onnx/model.onnx" "63363fc178428b74620c6f3780cbc7191883fa5c7f84c0945c45eb5c4256733b";
              tokenizer = fetch "tokenizer.json" "b01c78a902aa4facb2f47f95449f48e2f7bbfea5d2472ee2f6ce92323c6f86e5";
              config = fetch "config.json" "e426aa684c7f9a95c5f020aa855faf93a24f065f5fad0c9e17b124670cabdea6";
              specialTokens = fetch "special_tokens_map.json" "06e405a36dfe4b9604f484f6a1e619af1a7f7d09e34a8555eb0b77b66318067f";
              tokenizerConfig = fetch "tokenizer_config.json" "f477aeb15ff9f78d3c1ddf2361d2b0b8b20cf55220f839f29a37f3a18efddd89";
            in
            nixPackages.runCommand "nudox-semantic-model-jina-code-v2" { } ''
              mkdir -p "$out"
              cp ${model} "$out/model.onnx"
              cp ${tokenizer} "$out/tokenizer.json"
              cp ${config} "$out/config.json"
              cp ${specialTokens} "$out/special_tokens_map.json"
              cp ${tokenizerConfig} "$out/tokenizer_config.json"
            '';

          goOraclePackage = import ./workspace/compiler/languages/oracle/go/package.nix {
            pkgs = nixPackages;
            inherit goToolchain;
            src = builtins.path {
              path = ./workspace/compiler/languages/oracle/go;
              # Named to match the binary the producer looks up on PATH. A
              # directory named `go` collides with buildGoModule's
              # GOPATH="$TMPDIR/go".
              name = "nudox-go-oracle";
            };
          };

          javaOraclePackage = import ./workspace/compiler/languages/oracle/java/package.nix {
            pkgs = nixPackages;
            jdk = nixPackages.jdk21_headless;
            src = builtins.path {
              path = ./workspace/compiler/languages/oracle/java;
              name = "nudox-java-oracle";
            };
          };

          csharpOraclePackage = import ./workspace/compiler/languages/oracle/csharp/package.nix {
            pkgs = nixPackages;
            dotnet = nixPackages.dotnetCorePackages.sdk_10_0;
            src = builtins.path {
              path = ./workspace/compiler/languages/oracle/csharp;
              name = "nudox-csharp-oracle";
              filter =
                path: type:
                let
                  base = baseNameOf path;
                in
                if type == "directory" then
                  !(builtins.elem base [
                    "bin"
                    "obj"
                    "publish"
                  ])
                else
                  true;
            };
          };

          # ort-sys 2.0-rc.13 wants ONNX Runtime 1.28. nixpkgs ships 1.26, and
          # pyke's dist is a raw LZMA2 stream `xz` cannot read, so pin the
          # official GitHub tarball and point ORT_LIB_LOCATION at its lib dir.
          onnxruntimeLib =
            let
              distBySystem = {
                aarch64-darwin = {
                  url = "https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-osx-arm64-1.28.0.tgz";
                  hash = "sha256-EmizWXGAmb3izttVeH8YKhMAZ7xPMejIhHjERbhQ09g=";
                };
                x86_64-linux = {
                  url = "https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-linux-x64-1.28.0.tgz";
                  hash = "sha256-o+G3nXuxvwlpbOZ19J5AZObIH2ICuCJWJP/w6T+NZAc=";
                };
                # linux-aarch64 IS published upstream (verified against the
                # v1.28.0 GitHub release asset list 2026-08-18); the previous
                # comment here claiming it wasn't was stale and this system
                # was falling through to `null` — i.e. `nix build .#lindsey-app`
                # threw on aarch64-linux for no real upstream reason. Mirrors
                # the same asset `.config/scripts/ci-package-lindsey.sh`
                # already fetches for the `linux-arm64` release job.
                aarch64-linux = {
                  url = "https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-linux-aarch64-1.28.0.tgz";
                  hash = "sha256-4V/4tdha/mwUTZfG/UMiVL92ohnarxdlgIfW7LPo8Ls=";
                };
                # x86_64-darwin (macOS Intel) has no entry: ONNX Runtime 1.28.0
                # ships no osx-x86_64 build at all — not in the GitHub release
                # assets, not as a PyPI wheel (checked both 2026-08-18). This
                # is an upstream gap, not an oversight here; `lindseyAppPackage`
                # below throws a clear error on this system instead of quietly
                # producing nothing.
              };
              dist = distBySystem.${system} or null;
            in
            if dist == null then
              null
            else
              nixPackages.stdenvNoCC.mkDerivation {
                name = "onnxruntime-1.28.0-lib";
                src = nixPackages.fetchurl {
                  inherit (dist) url hash;
                };
                dontUnpack = true;
                nativeBuildInputs = [
                  nixPackages.gnutar
                  nixPackages.gzip
                ];
                installPhase = ''
                  mkdir -p "$out"
                  tar -xzf "$src"
                  libdir="$(find . -type d -name lib | head -n 1)"
                  if [ -z "$libdir" ]; then
                    echo "ONNX Runtime tarball has no lib/ directory" >&2
                    find . >&2
                    exit 1
                  fi
                  cp -R "$libdir"/. "$out/"
                  if ! find "$out" -name 'libonnxruntime*' | grep -q .; then
                    echo "ONNX Runtime lib dir has no libonnxruntime*" >&2
                    ls -la "$out" >&2
                    exit 1
                  fi
                '';
              };

          rustToolchain = (helpersFor nixPackages fenixPackages).rustToolchain;

          # cargo-bundle copies the GUI binary; this wrap is what actually
          # ships the subprocess oracles, toolchains, and embed model.
          lindseyBundlePackaging = import ./nix/lindsey-bundle.nix {
            pkgs = nixPackages;
            cargoBundle = cargoBundleUnstable;
            goOracle = goOraclePackage;
            javaOracle = javaOraclePackage;
            csharpOracle = csharpOraclePackage;
            inherit semanticModel goToolchain;
            jdk = nixPackages.jdk21_headless;
            libclang = nixPackages.libclang.lib;
            dotnet = nixPackages.dotnetCorePackages.sdk_10_0;
            # Pass null on platforms without a pinned ORT tarball; the wrap
            # then skips Contents/Frameworks (Linux .so / macOS .dylib).
            onnxruntimeLib = onnxruntimeLib;
          };

          lindseyAppPackage =
            if onnxruntimeLib == null then
              throw "lindsey-app: no ONNX Runtime 1.28 build for ${system} — upstream publishes no osx-x86_64 (macOS Intel) release for 1.28.0 (supported: aarch64-darwin, x86_64-linux, aarch64-linux)"
            else
              import ./workspace/gui/package.nix {
                pkgs = nixPackages;
                inherit rustToolchain;
                cargoBundle = cargoBundleUnstable;
                src = ./.;
                inherit (lindseyBundlePackaging) installWrapper;
                inherit onnxruntimeLib;
              };

          # Build the reproducible corpus from the Nix-owned corpus catalog
          # (`nix/corpus.nix`), across all seven declared ecosystems —
          # crates.io, go, npm, pypi, maven, nuget, cpp — plus the separate
          # `jpms_modules` array of compiled Java module descriptors.
          #
          # Per-ecosystem URL conventions (validated 2026-08-15 by re-deriving
          # every one of the 197 package-version + 6 jpms-module hashes in
          # `nix/corpus.nix` from these exact URLs via `nix-prefetch-url`; all
          # 203 matched):
          #
          #   crates.io -> static.crates.io .crate (tar.gz despite extension)
          #   go        -> proxy.golang.org module zip, case-escaped path+version
          #                (golang.org/x/mod/module.EscapePath: every uppercase
          #                letter becomes "!" + its lowercase form)
          #   npm       -> registry.npmjs.org tarball; scoped names
          #                (`@scope/pkg`) keep the full name in the URL path but
          #                drop the scope from the tarball basename
          #   pypi      -> pypi.io legacy redirect to the real files.pythonhosted.org
          #                sdist. The directory segment is the lowercased project
          #                name, but the *filename* stem is PyPI's registered
          #                casing/separator, which is not always a mechanical
          #                function of the manifest name — `pypiFilenameOverrides`
          #                below records the 3 (of 22) real exceptions found in
          #                this manifest: sqlalchemy -> SQLAlchemy, pyyaml ->
          #                PyYAML, dataclasses-json -> dataclasses_json.
          #   maven     -> repo1.maven.org **sources** jar
          #                (`{artifact}-{version}-sources.jar`), not the binary
          #                jar. This one corrects an assumption: the retired
          #                `corpus/fetch.nu` (recovered from git history at
          #                11e66e2^) that originally computed these hashes always
          #                fetched the sources classifier; re-deriving with the
          #                plain jar produces a different, non-matching hash.
          #   nuget     -> api.nuget.org v3 flat-container .nupkg, id and version
          #                both lowercased
          #   cpp       -> each version carries an explicit `url` field (GitHub
          #                release asset); no ecosystem convention exists because
          #                GitHub's auto-generated tag tarballs are not
          #                guaranteed byte-stable
          #
          # `jpms_modules` (Maven coordinates, separate top-level array): these
          # ARE fetched — `workspace/compiler/languages/src/java/invoke.rs` and
          # `workspace/compiler/languages/tests/java/corpus_sweep.rs` both
          # already depend on `result/.module-path/<artifact>-<version>.jar`
          # existing (module descriptors with no usable source form: automatic
          # module names, or a sources jar published without
          # `module-info.java`). They fetch the plain **compiled** jar (not
          # `-sources.jar` — a sources-only jar is exactly what these modules
          # lack), are never extracted, and land in a dedicated
          # `.module-path/` directory that the java producer explicitly skips
          # when walking package checkouts — so a compiled jar can never
          # become lowering input by accident.
          buildCorpus =
            nixPackages:
            let
              lib = nixPackages.lib;
              corpus = import ./nix/corpus.nix;

              # Mirrors `workspace/index/tests/common/mod.rs`'s `safe_dir_name`
              # byte-for-byte: `/` and `:` both become `__`. Load-bearing —
              # every Rust corpus consumer resolves fixtures through this exact
              # transform.
              safeDirName = name: version: "${lib.replaceStrings [ "/" ":" ] [ "__" "__" ] name}-${version}";

              # Go module proxy path/version escaping: every uppercase letter
              # becomes "!" + its lowercase form.
              goEscape =
                s:
                lib.concatStrings (
                  map (c: if c == lib.toUpper c && c != lib.toLower c then "!${lib.toLower c}" else c) (
                    lib.stringToCharacters s
                  )
                );

              # PyPI sdist filenames are not always a mechanical lowercasing of
              # the manifest name (see the header comment above) — this is the
              # complete set of exceptions among this manifest's 22 pypi
              # entries, found by cross-checking every one against PyPI's JSON
              # API.
              pypiFilenameOverrides = {
                sqlalchemy = "SQLAlchemy";
                pyyaml = "PyYAML";
                "dataclasses-json" = "dataclasses_json";
              };

              # Resolves the download URL for one package version.
              # `overrideUrl` (nullable), when set, always wins — this is how
              # `cpp` entries work.
              urlFor =
                ecosystem: name: version: overrideUrl:
                if overrideUrl != null then
                  overrideUrl
                else if ecosystem == "crates.io" then
                  "https://static.crates.io/crates/${name}/${name}-${version}.crate"
                else if ecosystem == "go" then
                  "https://proxy.golang.org/${goEscape name}/@v/${goEscape version}.zip"
                else if ecosystem == "npm" then
                  let
                    basename = if lib.hasPrefix "@" name then lib.last (lib.splitString "/" name) else name;
                  in
                  "https://registry.npmjs.org/${name}/-/${basename}-${version}.tgz"
                else if ecosystem == "pypi" then
                  let
                    lower = lib.toLower name;
                    first = builtins.substring 0 1 lower;
                    stem = pypiFilenameOverrides.${name} or name;
                  in
                  "https://pypi.io/packages/source/${first}/${lower}/${stem}-${version}.tar.gz"
                else if ecosystem == "maven" then
                  let
                    parts = lib.splitString ":" name;
                    group = builtins.elemAt parts 0;
                    artifact = builtins.elemAt parts 1;
                    groupPath = lib.replaceStrings [ "." ] [ "/" ] group;
                  in
                  "https://repo1.maven.org/maven2/${groupPath}/${artifact}/${version}/${artifact}-${version}-sources.jar"
                else if ecosystem == "nuget" then
                  let
                    lower = lib.toLower name;
                    verLower = lib.toLower version;
                  in
                  "https://api.nuget.org/v3-flatcontainer/${lower}/${verLower}/${lower}.${verLower}.nupkg"
                else
                  throw "buildCorpus: no URL convention for ecosystem '${ecosystem}'";

              # The plain compiled jar for a `groupId:artifactId` jpms module
              # coordinate (never the sources classifier — see header comment).
              jpmsUrlFor =
                name: version: overrideUrl:
                if overrideUrl != null then
                  overrideUrl
                else
                  let
                    parts = lib.splitString ":" name;
                    group = builtins.elemAt parts 0;
                    artifact = builtins.elemAt parts 1;
                    groupPath = lib.replaceStrings [ "." ] [ "/" ] group;
                  in
                  "https://repo1.maven.org/maven2/${groupPath}/${artifact}/${version}/${artifact}-${version}.jar";

              guessExt =
                url:
                if lib.hasSuffix ".tar.gz" url then
                  ".tar.gz"
                else if lib.hasSuffix ".tgz" url then
                  ".tgz"
                else if lib.hasSuffix ".zip" url then
                  ".zip"
                else if lib.hasSuffix ".jar" url then
                  ".jar"
                else if lib.hasSuffix ".nupkg" url then
                  ".nupkg"
                else if lib.hasSuffix ".crate" url then
                  ".crate"
                else
                  "";

              # zip-family archives (go module zips, maven/nuget jars) vs
              # tar.gz-family (crates.io, npm, pypi sdists, cpp tarballs).
              isZipFamily = ext: ext == ".zip" || ext == ".jar" || ext == ".nupkg";

              # Archives either wrap their contents in one or more nested
              # single-child directories (crates.io, npm, pypi sdists, go
              # module zips, cpp tarballs) or have none (maven sources jars
              # and nuget nupkgs, which put files straight at the archive
              # root). Stripping is ecosystem-scoped and load-bearing, not
              # tidiness: for maven/nuget the leading directories ARE the
              # package path (`javax/inject/...`), and blindly descending
              # through them corrupts the layout the same way it did in the
              # retired `corpus/fetch.nu` (see its `WRAPPED_ECOSYSTEMS`
              # comment, recovered from git history at 11e66e2^).
              wrappedEcosystems = [
                "crates.io"
                "npm"
                "pypi"
                "go"
                "cpp"
              ];

              # Fetch and prepare a single package version. Every prepared
              # derivation's output is exactly one subdirectory named
              # `safeDirName name version`, so assembly is a uniform copy with
              # no ecosystem-specific glob.
              prepareEntry =
                pkgEntry: verEntry:
                let
                  ecosystem = pkgEntry.ecosystem;
                  name = pkgEntry.name;
                  version = verEntry.version;
                  hash = verEntry.hash;
                  overrideUrl = verEntry.url or null;
                  url = urlFor ecosystem name version overrideUrl;
                  ext = guessExt url;
                  dirName = safeDirName name version;
                  sourceOverlay = verEntry.source_overlay or null;
                  sourceOverlays =
                    (if sourceOverlay == null then [ ] else [ sourceOverlay ]) ++ (verEntry.source_overlays or [ ]);
                  sourceOverlayArchives = map (overlay: {
                    inherit overlay;
                    archive = nixPackages.fetchurl {
                      url = overlay.url;
                      sha256 = overlay.hash;
                    };
                  }) sourceOverlays;
                  archive = nixPackages.fetchurl {
                    inherit url;
                    sha256 = hash;
                  };
                  isWrapped = ext != "" && builtins.elem ecosystem wrappedEcosystems;
                  appendCargoWorkspace = ecosystem == "crates.io";
                in
                nixPackages.runCommand "${dirName}-prepared"
                  {
                    nativeBuildInputs = [
                      nixPackages.gnutar
                      nixPackages.unzip
                    ];
                  }
                  (
                    if ext == "" then
                      # A handful of cpp release assets are a single raw header,
                      # not an archive (e.g. simdjson's simdjson.h, Catch2's
                      # catch_amalgamated.hpp). Place the file itself.
                      ''
                        mkdir -p "$out/${dirName}"
                        cp "${archive}" "$out/${dirName}/$(basename "${url}")"
                      ''
                    else
                      ''
                        set -eu
                        mkdir extracted
                        ${
                          if isZipFamily ext then
                            ''unzip -q "${archive}" -d extracted''
                          else
                            ''tar -xzf "${archive}" -C extracted''
                        }
                        src=extracted
                        ${lib.optionalString isWrapped ''
                          # Descend through directories that contain nothing
                          # but a single subdirectory. Recursive (not one
                          # level): go module zips nest the whole archive as
                          # `<module-path>@<version>/...`, and a module path
                          # itself has path separators.
                          while true; do
                            entries=$(ls -A "$src")
                            count=$(printf '%s\n' "$entries" | grep -c .)
                            if [ "$count" = "1" ] && [ -d "$src/$entries" ]; then
                              src="$src/$entries"
                            else
                              break
                            fi
                          done
                        ''}
                        mkdir -p "$out"
                        cp -r "$src" "$out/${dirName}"
                        ${lib.optionalString appendCargoWorkspace ''
                          if [ -f "$out/${dirName}/Cargo.toml" ]; then
                            echo "" >> "$out/${dirName}/Cargo.toml"
                            echo "[workspace]" >> "$out/${dirName}/Cargo.toml"
                          fi
                        ''}
                        ${lib.concatMapStringsSep "\n" (item: ''
                          # Some Maven sources classifiers omit generated
                          # or test-harness Java declarations. Overlay only
                          # pinned source files required to make the
                          # checkout complete; compiled jars remain in
                          # .class-path and are never extracted into a
                          # source checkout.
                          overlay="${item.archive}"
                          target="$out/${dirName}/${item.overlay.target}"
                          mkdir -p "$(dirname "$target")"
                          cp "$overlay" "$target"
                        '') sourceOverlayArchives}
                      ''
                  );

              # Fetches and *places* (never extracts) one compiled Java jar
              # under the caller-selected hidden directory. Both module-path
              # and exceptional classpath jars stay structurally outside the
              # source walk, so binary dependency bytes cannot become lowering
              # input by accident.
              prepareCompiledJar =
                destination: modEntry: verEntry:
                let
                  name = modEntry.name;
                  version = verEntry.version;
                  hash = verEntry.hash;
                  overrideUrl = verEntry.url or null;
                  url = jpmsUrlFor name version overrideUrl;
                  artifact = builtins.elemAt (lib.splitString ":" name) 1;
                  jarName = "${artifact}-${version}.jar";
                  archive = nixPackages.fetchurl {
                    inherit url;
                    sha256 = hash;
                  };
                in
                nixPackages.runCommand "${destination}-${artifact}-${version}-prepared" { } ''
                  mkdir -p "$out/${destination}"
                  cp "${archive}" "$out/${destination}/${jarName}"
                '';

              allPreparedEntries = lib.flatten (
                map (pkgEntry: map (verEntry: prepareEntry pkgEntry verEntry) pkgEntry.versions) corpus.packages
              );

              allPreparedCrateEntries = lib.flatten (
                map (
                  pkgEntry:
                  map (verEntry: {
                    dirName = safeDirName pkgEntry.name verEntry.version;
                    derivation = prepareEntry pkgEntry verEntry;
                  }) pkgEntry.versions
                ) (lib.filter (pkgEntry: pkgEntry.ecosystem == "crates.io") corpus.packages)
              );

              allPreparedModules = lib.flatten (
                map (
                  modEntry: map (verEntry: prepareCompiledJar ".module-path" modEntry verEntry) modEntry.versions
                ) (corpus.jpms_modules or [ ])
              );

              allPreparedClasspath = lib.flatten (
                map (
                  depEntry: map (verEntry: prepareCompiledJar ".class-path" depEntry verEntry) depEntry.versions
                ) (corpus.java_classpath or [ ])
              );

              # Assemble all prepared packages (and, if any, jpms modules)
              # into one directory.
              assembliedCorpus =
                nixPackages.runCommand "real-corpus"
                  {
                    nativeBuildInputs = [ nixPackages.cargo ];
                  }
                  ''
                    mkdir -p "$out"
                    ${lib.concatMapStringsSep "\n" (derivation: "cp -r ${derivation}/. $out/") allPreparedEntries}
                    chmod -R u+w "$out/serde_json-1.0.113"
                    mkdir -p "$out/serde_json-1.0.113/.cargo"
                    mkdir -p "$out/serde_json-1.0.113/vendor"
                    cat > "$out/serde_json-1.0.113/.cargo/config.toml" <<'EOF'
                    [source.crates-io]
                    replace-with = "nudox-corpus"

                    [source.nudox-corpus]
                    directory = "vendor"
                    EOF
                    ${lib.concatMapStringsSep "\n" (entry: ''
                      cp -r ${entry.derivation}/${entry.dirName} $out/serde_json-1.0.113/vendor/${entry.dirName}
                      chmod -R u+w "$out/serde_json-1.0.113/vendor/${entry.dirName}"
                      printf '%s\n' '{"files":{},"package":null}' > "$out/serde_json-1.0.113/vendor/${entry.dirName}/.cargo-checksum.json"
                    '') allPreparedCrateEntries}
                    (
                      cd "$out/serde_json-1.0.113"
                      CARGO_HOME="$TMPDIR/cargo-home" cargo generate-lockfile --offline
                    )
                    ${lib.optionalString (allPreparedModules != [ ]) ''
                      mkdir -p "$out/.module-path"
                    ''}
                    ${lib.concatMapStringsSep "\n" (
                      derivation: "cp -r ${derivation}/.module-path/. $out/.module-path/"
                    ) allPreparedModules}
                    ${lib.optionalString (allPreparedClasspath != [ ]) ''
                      mkdir -p "$out/.class-path"
                    ''}
                    ${lib.concatMapStringsSep "\n" (
                      derivation: "cp -r ${derivation}/.class-path/. $out/.class-path/"
                    ) allPreparedClasspath}
                  '';
            in
            assembliedCorpus;

        in
        {
          # Workspace package recipes + toolchain re-exports.
          packages =
            let
              helpers = helpersFor nixPackages fenixPackages;
              inherit (helpers) optionalEnvStorePath;
            in
            (import ./workspace {
              pkgs = nixPackages;
              inherit fenixPackages;
              buildImage = nix2container.packages.${system}.nix2container.buildImage;
              src = ./.;
              version = self.rev or "unknown";
              # Pure evaluation produces no tools; impure snowydeer builds add
              # only the store paths that are available.
              compilerTools =
                optionalEnvStorePath "compilerDaemon" "NUDOX_COMPILER_DAEMON_PATH"
                // optionalEnvStorePath "producerWorker" "NUDOX_PRODUCER_WORKER_PATH"
                // optionalEnvStorePath "goOracle" "NUDOX_GO_ORACLE_PATH"
                // optionalEnvStorePath "javaOracle" "NUDOX_JAVA_ORACLE_PATH"
                // optionalEnvStorePath "csharpOracle" "NUDOX_CSHARP_ORACLE_PATH";
            })
            // {
              cargo-bundle-unstable = cargoBundleUnstable;
              semantic-model = semanticModel;
              go-oracle = goOraclePackage;
              java-oracle = javaOraclePackage;
              csharp-oracle = csharpOraclePackage;
              lindsey-bundle = lindseyBundlePackaging.lindseyBundle;
              lindsey-install-wrapper = lindseyBundlePackaging.installWrapper;
              lindsey-app = lindseyAppPackage;
            };

          checks =
            let
              helpers = helpersFor nixPackages fenixPackages;
              inherit (helpers) rustToolchainHooks;

              packagesForSystem = config.packages;
              corpusForTests = buildCorpus nixPackages;

              # Pure evaluation retains the package fallback. With an explicit
              # snowydeer path, replace only the compiler package in checks.
              compilerOverride =
                let
                  p = builtins.getEnv "NUDOX_COMPILER_DAEMON_PATH";
                in
                nixPackages.lib.optionalAttrs (p != "") {
                  compiler-daemon = nixPackages.runCommand "compiler-daemon-for-check" { } ''
                    mkdir -p "$out/bin"
                    src="${builtins.storePath p}"
                    if [ -f "$src/bin/compiler-daemon" ]; then
                      cp -L "$src/bin/compiler-daemon" "$out/bin/compiler-daemon"
                    elif [ -f "$src" ]; then
                      cp -L "$src" "$out/bin/compiler-daemon"
                    else
                      echo "NUDOX_COMPILER_DAEMON_PATH=$src has no compiler-daemon binary" >&2
                      exit 1
                    fi
                    chmod +x "$out/bin/compiler-daemon"
                  '';
                };

              projectRoot =
                let
                  prj = builtins.getEnv "PRJ_ROOT";
                  pwd = builtins.getEnv "PWD";
                in
                if prj != "" then prj else pwd;

              integrationChecks = import ./tests {
                pkgs = nixPackages;
                inherit fenixPackages;
                packages =
                  packagesForSystem
                  // compilerOverride
                  // {
                    corpus = corpusForTests;
                  };
                buck2 = makeBuck2BinaryDerivation nixPackages;
                inherit smolvmSource;
                inherit projectRoot;
                semanticModel = semanticModel;
                inherit goToolchain;
                inherit lindseyBundlePackaging;
                dotnet = nixPackages.dotnetCorePackages.sdk_10_0;
              };
            in
            integrationChecks
            // {
              workspace-tests = integrationChecks.workspaceTests;
              # Kebab alias, matching `workspace-tests`, so
              # `nix build .#checks.<system>.index-tests` reads the way the
              # other checks do.
              index-tests = integrationChecks.indexTests;
              semantic-tests = integrationChecks.semanticTests;
              # Kebab alias, matching `index-tests`/`workspace-tests`, so
              # `nix build .#checks.<system>.go-oracle` reads the way the
              # other checks do.
              go-oracle = integrationChecks.goOracle;
              csharp-oracle = integrationChecks.csharpOracle;
              lindsey-bundle = integrationChecks.lindseyBundle;
              corpus = corpusForTests;
              preCommitGitHooks = git-hooks.lib.${system}.run {
                src = ./.;
                package = nixPackages.prek;
                default_stages = [ "pre-push" ];
                hooks = {
                  convco = {
                    enable = true;
                    pass_filenames = false;
                    stages = [ "pre-push" ];
                    entry = toString (
                      nixPackages.writeShellScript "convco-pre-push" ''
                        while read local_ref local_sha remote_ref remote_sha; do
                          ${nixPackages.convco}/bin/convco check "$remote_sha..$local_sha"
                        done
                      ''
                    );
                  };
                  nixfmt.enable = true;
                  rustfmt = {
                    enable = true;
                    packageOverrides = {
                      cargo = rustToolchainHooks;
                      rustfmt = rustToolchainHooks;
                    };
                  };
                  markdownfmt = {
                    enable = true;
                    name = "hongdown";
                    entry = "hongdown --write";
                    files = "\\.md$";
                    language = "system";
                  };
                  testrust = {
                    enable = true;
                    name = "testrust";
                    # A bare `cargo nextest run` fails at the build step on this
                    # repo before any nextest.toml filter runs: `workspace/index`
                    # has never compiled, and `driver`/`ir-vcs` both depend on it
                    # (docs/LIMITATIONS.md L6) — `--exclude` for those three is
                    # required on every invocation, and `nudox-ir` needs
                    # RUSTC_BOOTSTRAP=1 (unstable macro decls). This entry was
                    # broken (would fail on every commit) before this fix.
                    # `nextest-suite.nu` bakes both requirements in and runs the
                    # `default` profile (root-workspace unit + integration only,
                    # high concurrency, no real-crate/GUI cost) — the right size
                    # for a per-commit gate; `nu .config/scripts/nextest-suite.nu
                    # --all` is the full end-to-end suite for CI, not this hook.
                    entry = "${nixPackages.nushell}/bin/nu .config/scripts/nextest-suite.nu";
                    language = "system";
                    pass_filenames = false;
                    stages = [ "pre-merge-commit" ];
                  };
                  clippy = {
                    enable = true;
                    stages = [
                      "pre-merge-commit"
                      "pre-push"
                    ];
                    packageOverrides = {
                      cargo = rustToolchainHooks;
                      clippy = rustToolchainHooks;
                    };
                  };
                };
              };
            };

          devShells =
            let
              helpers = helpersFor nixPackages fenixPackages;
              inherit (helpers) rustToolchainDev;
              corpusForDevShell = buildCorpus nixPackages;

              # Build script PATH: fenix rustc + nix package manager + system paths.
              # Consumed by nix/build/third-party/defs.bzl via read_config("build","devshell_bin").
              devshellBin = nixPackages.lib.concatStringsSep ":" [
                "${rustToolchainDev}/bin"
                "${nixPackages.nix}/bin"
                "/usr/bin"
                "/bin"
                "/usr/sbin"
                "/sbin"
              ];

              buckconfigLocal = nixPackages.writeText "buckconfig-local" (
                nixPackages.lib.generators.toINI
                  {
                    mkKeyValue = k: v: "  ${k} = ${v}";
                  }
                  {
                    cells = {
                      nix = "nix/build/nix-cell";
                    };
                    nix = {
                      toolchain = "1";
                    };
                    build = {
                      devshell_bin = devshellBin;
                    };
                    go = {
                      go_binary = "${goToolchain}/bin/go";
                    };
                    java = {
                      java_home = toString nixPackages.jdk21_headless;
                    };
                    csharp = {
                      dotnet = "${nixPackages.dotnetCorePackages.sdk_10_0}/bin/dotnet";
                      nuget_packages = "";
                    };
                  }
              );

              # ── Devshell command wrappers ────────────────────────────────────
              #
              # `binNameOverrides` exists because `mkShell` puts every entry in
              # `packages` ahead of the system `$PATH` (Nix's own precedence, not
              # this flake's choice) — so a devshell command that happens to share
              # a name with a POSIX utility silently *replaces* that utility for
              # every process the shell runs, not just interactive use.
              # `install` did exactly that: `tikv-jemalloc-sys`'s build script
              # shells out to `configure`, which calls the real `install(1)` to
              # generate a conftest file, and got `.config/scripts/install.nu`
              # instead — a script with a completely different argument grammar
              # (no `-o`), so the build failed with "unknown flag '-o'" nowhere
              # near this flake's own code. `patch` is the next most plausible
              # collision (some C packages' build steps shell out to GNU/BSD
              # `patch(1)`); the rest of `nuScriptCommands` are cargo-adjacent
              # verbs (`build`, `check`, `test`, …) that no third-party build
              # script invokes by that bare name, so they are left alone rather
              # than renamed on spec.
              binNameOverrides = {
                install = "nx-install";
                "install-force" = "nx-install-force";
              };

              mkDevshellCommand =
                cmdName:
                let
                  binName = binNameOverrides.${cmdName} or cmdName;
                in
                nixPackages.writeTextFile {
                  name = "${binName}-nuenv";
                  destination = "/bin/${binName}";
                  executable = true;
                  text = ''
                    #!/bin/sh
                    cd "$PRJ_ROOT" && exec ${nixPackages.nushell}/bin/nu .config/scripts/${cmdName}.nu "$@"
                  '';
                };

              nuScriptCommands = [
                "build"
                "build-release"
                "check"
                "clean"
                "create-notes"
                "doc"
                "doc-open"
                "fmt"
                "fmt-check"
                "install"
                "install-force"
                "lint"
                "lint-fix"
                "patch"
                "release"
                "run"
                "run-release"
                "sync-deps"
                "test"
                "test-with"
                "test-all"
                "full-check"
                "recheck"
                "update"
                "buck-build"
                "buck-test"
                "ra-index"
                "snowydeer-import"
                "build-compiler-image"
                "local-backends"
                "sandbox-vm-test"
                "bundle"
              ];

              commandPackages = map mkDevshellCommand nuScriptCommands;

              # ── lindsey (workspace/gui) native graphics stack ─────────────
              #
              # GPUI's Linux backends need four libraries that nothing else in
              # this repo does. Two are DT_NEEDED by the linked binary (libxcb,
              # libxkbcommon + its -x11 companion); two are dlopened by soname
              # at runtime and so never appear in `patchelf --print-needed` at
              # all (libwayland-client, libvulkan).
              #
              # Leaving them out does not produce a missing-dependency error at
              # build time, which is what makes it dangerous: `pkg-config` falls
              # through to the host's /usr/lib copies, the link succeeds, and
              # the result is a binary that mixes Nix's dynamic linker with the
              # host's glibc — it requires GLIBC_2.43 under Nix's 2.42 loader
              # and dies at exec naming neither cause. This is a correctness
              # fix, not a convenience.
              guiGraphicsLibraries = with nixPackages; [
                libxcb
                libxkbcommon
                wayland
                vulkan-loader
              ];

              # The account keyring's C dependency. The trunk's `KeyringStore`
              # uses `keyring` with `sync-secret-service`, which chains
              # `dbus-secret-service` -> `dbus` -> `libdbus-sys` — a *linked* C
              # library, not a dlopened one — so without it `cargo check` dies
              # in a build script before compiling anything:
              #
              #   HINT: you may need to install a package such as dbus-1,
              #         dbus-1-dev or dbus-1-devel.
              #
              # Linked rather than dlopened means it is needed at run time too,
              # so it joins the LD_LIBRARY_PATH list below rather than being
              # build-only like fontconfig.
              guiCredentialLibraries = with nixPackages; [
                dbus
              ];

              # Needed to *build* (font-kit's `yeslogic-fontconfig-sys` and
              # `freetype-sys` are pkg-config crates), but deliberately kept off
              # LD_LIBRARY_PATH: font *configuration* is a property of the
              # machine, and a store fontconfig carries no font paths, finds
              # nothing, and renders blank text. Link against the store copy,
              # resolve the host's `libfontconfig.so.1` at run time.
              guiFontLibraries = with nixPackages; [
                fontconfig
                freetype
              ];
            in
            {
              default = nixPackages.mkShell {
                name = "NuNuShell";

                # Cargo compiles C dependencies at `-O0` in the dev profile,
                # while Nix's default hardening injects `-D_FORTIFY_SOURCE=3`.
                # glibc answers that combination with
                #
                #     features.h: #warning _FORTIFY_SOURCE requires compiling
                #                 with optimization (-O)
                #
                # which is harmless until a dependency's autotools probe runs
                # with `-Werror` — and `tikv-jemalloc-sys`'s does. Both of its
                # `strerror_r` probes fail on the warning, configure aborts with
                # "cannot determine return type of strerror_r", and the whole
                # GUI build dies on a message mentioning neither fortification
                # nor optimisation.
                #
                # It reproduces only in debug builds on Linux: `--release`
                # compiles the same C at `-O3`, where the warning never fires.
                hardeningDisable = [
                  "fortify"
                  "fortify3"
                ];

                RUSTC_BOOTSTRAP = "1";
                LIBRARY_PATH = "${nixPackages.libiconv}/lib";
                # `nudox-languages` (workspace/compiler/languages) links
                # `clang-sys` with its `runtime` feature: libclang is `dlopen`'d at
                # first use, not linked at build time, so no binary that merely
                # links the crate can abort at process load — see that crate's
                # Cargo.toml. `LIBCLANG_PATH` still governs *which* libclang the
                # dlopen finds; pinning it to the flake's own nixpkgs derivation
                # (rather than leaving discovery to fall back to whatever Xcode
                # Command Line Tools / system package manager happens to be
                # installed) is what makes that discovery reproducible across
                # machines instead of an unstated assumption about the host.
                LIBCLANG_PATH = "${nixPackages.libclang.lib}/lib";
                MAIN_PACKAGE = "nudox";
                OUTPUT_DIRECTORY = "dist";
                NUDOX_CORPUS_ROOT = "${corpusForDevShell}";
                # The standard application build enables the GUI's ONNX
                # feature, and this store path is the same fixed model used
                # by the semantic integration check. The process only reads
                # it at runtime; no network access is needed after entering
                # the shell.
                NUDOX_EMBED_MODEL_DIR = "${semanticModel}";
                # Subprocess oracles: without these, every Go/Java package fails
                # to lower from a packaged or `cargo run` lindsey (spawn miss
                # or a scavenged schema-0 binary). Rust/TypeScript/Python are
                # in-process and need no matching variable.
                NUDOX_GO_ORACLE_BIN = "${goOraclePackage}/bin/nudox-go-oracle";
                NUDOX_JAVA_ORACLE_CLASSES = "${javaOraclePackage}";
                NUDOX_CSHARP_ORACLE = "${csharpOraclePackage}/lib/nudox-csharp-oracle";
                NUDOX_DOTNET = "${nixPackages.dotnetCorePackages.sdk_10_0}/bin/dotnet";
                OPENSSL_DIR = "${nixPackages.openssl.dev}";
                OPENSSL_LIB_DIR = "${nixPackages.openssl.out}/lib";
                OPENSSL_INCLUDE_DIR = "${nixPackages.openssl.dev}/include";
                DOTNET_CLI_TELEMETRY_OPTOUT = "1";
                DOTNET_NOLOGO = "1";
                DOTNET_SKIP_FIRST_TIME_EXPERIENCE = "1";

                packages =
                  commandPackages
                  ++ [
                    rustToolchainDev
                    (makeBuck2BinaryDerivation nixPackages)
                    (makeReindeerBinaryDerivation nixPackages)
                  ]
                  ++ (with nixPackages; [
                    git
                    cargo-bump
                    nushell
                    rust-analyzer
                    flock
                    nixfmt-rfc-style
                    tombi
                    typos
                    hongdown
                    kittysay
                    marksman
                    taplo
                    cargo-nextest
                    cargo-mutants
                    libiconv
                    cargoBundleUnstable
                    goOraclePackage
                    javaOraclePackage
                    csharpOraclePackage
                    lindseyBundlePackaging.lindseyBundle
                    lindseyBundlePackaging.installWrapper
                    libclang.lib
                    nil
                    jsonfmt
                    dotacat
                    goreleaser
                    cuelsp
                    b3sum
                    goToolchain
                    # Lombok 1.18.30's legacy javac adapter requires the
                    # pre-JPMS doclet/compiler toolchain; it is selected only
                    # through NUDOX_JAVA_LEGACY for that corpus entry.
                    jdk8
                    jdk21_headless
                    dotnetCorePackages.sdk_10_0
                    # `qdrant` (the vector-store server the `index`
                    # server-integration "Live" tier needs, docs/TESTING.md)
                    # is deliberately NOT listed here: nixpkgs' `qdrant`
                    # derivation builds the whole Rust server from source
                    # (no aarch64-darwin binary cache hit observed), which
                    # would add several minutes to *every* `nix develop`,
                    # not just the rare live-backend run. `local-backends.nu`
                    # locates/builds it on demand instead — see that script.
                  ])
                  ++ (
                    with nixPackages.lib;
                    optionals isLinuxSystem [
                      libkrun
                      smolvmBinary
                    ]
                  )
                  ++ (
                    with nixPackages.lib;
                    optionals isLinuxSystem (
                      with nixPackages;
                      [
                        wild-unwrapped
                        openssl
                        clang
                        # Nix's own pkg-config, so `guiGraphicsLibraries` is
                        # what gets found rather than the host's
                        # /usr/lib/pkgconfig — see that binding for what the
                        # host's answer costs.
                        pkg-config
                      ]
                    )
                  );

                # `buildInputs` rather than `packages`: the pkg-config setup
                # hook builds PKG_CONFIG_PATH from this list, which is the whole
                # point — `xcb.pc` and `xkbcommon.pc` must resolve to the store.
                buildInputs = nixPackages.lib.optionals nixPackages.stdenv.isLinux (
                  guiGraphicsLibraries ++ guiCredentialLibraries ++ guiFontLibraries
                );

                shellHook = ''
                  export PRJ_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo "$PWD")"

                  export LD_LIBRARY_PATH="${nixPackages.openssl.out}/lib:$LD_LIBRARY_PATH"
                  export DOTNET_CLI_HOME="$TMPDIR/dotnet"

                  ${nixPackages.lib.optionalString nixPackages.stdenv.isLinux ''
                    # lindsey's graphics stack. Two of the four are dlopened by
                    # soname — libwayland-client by `wayland-client`, libvulkan
                    # by wgpu — so they are invisible to the linker and must be
                    # findable at run time or the window never opens.
                    export LD_LIBRARY_PATH="${nixPackages.lib.makeLibraryPath (guiGraphicsLibraries ++ guiCredentialLibraries)}:$LD_LIBRARY_PATH"

                    # The Vulkan *driver* cannot come from the store on a
                    # non-NixOS host: its ICD manifest names the vendor library
                    # by bare soname. Exported as its own variable rather than
                    # added to LD_LIBRARY_PATH, because putting a host library
                    # directory on the search path of every compiler and build
                    # script in the repo broke the build once already — the Nix
                    # JDK loaded the host's libnet.so and the Java producer's
                    # build script died on `undefined symbol:
                    # reuseport_available`. `workspace/gui/packaging/linux/
                    # run-lindsey` applies it to the one process that needs it.
                    if [[ -d /run/opengl-driver/lib ]]; then
                      export NUDOX_GUI_DRIVER_PATH=/run/opengl-driver/lib
                    elif [[ -d /usr/lib ]]; then
                      export NUDOX_GUI_DRIVER_PATH=/usr/lib
                    fi
                  ''}

                  # Sealed-producer PATH prefix (see sandbox::ToolchainSet). Hermetic
                  # PATH alone is /usr/bin:/bin:/nix/var/nix/profiles/default/bin —
                  # language oracles need go/javadoc/dotnet from the Nix store.
                  export NUDOX_TOOLCHAIN_PATH="${goToolchain}/bin:${nixPackages.jdk21_headless}/bin:${nixPackages.dotnetCorePackages.sdk_10_0}/bin''${NUDOX_TOOLCHAIN_PATH:+:$NUDOX_TOOLCHAIN_PATH}"
                  export NUDOX_JAVAC="${nixPackages.jdk21_headless}/bin/javac"
                  export NUDOX_JAVADOC="${nixPackages.jdk21_headless}/bin/javadoc"
                  export NUDOX_JAVA8_JAVAC="${nixPackages.jdk8}/bin/javac"
                  export NUDOX_JAVA8_JAVADOC="${nixPackages.writeShellScript "nudox-javadoc8" ''
                    set -eu
                    output="$(mktemp)"
                    trap 'rm -f "$output"' EXIT
                    if ! ${nixPackages.jdk8}/bin/javadoc "$@" >"$output"; then
                      cat "$output"
                      exit 1
                    fi
                    awk '/^\{/{ print; exit }' "$output"
                  ''}"
                  export NUDOX_JAVA8_TOOLS_JAR="${nixPackages.jdk8}/lib/tools.jar"

                  if command -v kittysay > /dev/null 2>&1; then
                    kittysay --think "the nu is the now" | dotacat
                  fi

                  ln -sfn ${buck2-prelude} "$PRJ_ROOT/prelude"
                  ln -sfn ${buck2-nix} "$PRJ_ROOT/nix/build/nix-cell"
                  preludeStampFile="$PRJ_ROOT/nix/build/prelude-local/.nix-source"

                  if [[ ! -f "$preludeStampFile" || "$(cat "$preludeStampFile")" != "${buck2-prelude}" ]]; then
                    bash "$PRJ_ROOT/nix/build/setup-prelude.sh"
                    echo -n "${buck2-prelude}" > "$preludeStampFile"
                  fi

                  # Buck2 loads `.buckconfig.local` after the tracked config. Keep
                  # Nix store paths here: they are host-specific and must never
                  # dirty the checkout. Stage outside the checkout before the
                  # atomic rename: a stale/root-owned `.buckconfig.local.tmp`
                  # from an older shell hook otherwise makes `cp` fail before
                  # the shell is usable (especially after switching users or
                  # crossing a host filesystem boundary).
                  buckconfigLocalTmp="$(mktemp "''${TMPDIR:-/tmp}/nudox-buckconfig.XXXXXX")" || {
                    echo "failed to allocate a temporary Buck2 config" >&2
                    return 1
                  }
                  if ! cp "${buckconfigLocal}" "$buckconfigLocalTmp"; then
                    rm -f "$buckconfigLocalTmp"
                    echo "failed to stage the generated Buck2 config" >&2
                    return 1
                  fi
                  if ! mv -f "$buckconfigLocalTmp" "$PRJ_ROOT/.buckconfig.local"; then
                    rm -f "$buckconfigLocalTmp"
                    echo "failed to install $PRJ_ROOT/.buckconfig.local" >&2
                    return 1
                  fi

                  export RUST_TARGET=$(rustc --version --verbose | grep '^host:' | awk '{print $2}')
                  unset RUSTC_WRAPPER

                  (
                    flock -n 9 || exit 1
                  ) 9>/tmp/nunu_sync.lock &
                '';
              };
            };

          formatter = nixPackages.nixfmt-rfc-style;
        };
    };
}
