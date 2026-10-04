# Backend's adapter for the shared continuous-fuzz package shape.
# packages.${system}.continuous-fuzz passthru is bins, corpora, engines, metadata.
# Auth copies that shape and plugs its own engine. This file's engine is libFuzzer.
# fuzz-<id> packages are optional aliases. These are not flake checks.
{
  pkgs,
  workspaceRoot,
  workspaceSource,
  workspaceAvailable,
  stableRustPlatform,
  gpuiOutputHashes,
}:
let
  lib = pkgs.lib;
  fuzzRoot = workspaceRoot + "/tests/fuzz";
  targetRoot = fuzzRoot + "/targets";
  manifestPath = fuzzRoot + "/Cargo.toml";
  available = workspaceAvailable && workspaceSource != null && builtins.pathExists manifestPath;
  entries = if available && builtins.pathExists targetRoot then builtins.readDir targetRoot else { };
  directoryNames = builtins.sort (left: right: left < right) (
    lib.filter (name: entries.${name} == "directory" && !(lib.hasPrefix "." name)) (
      builtins.attrNames entries
    )
  );
  isTargetId = name: builtins.match "[a-z][a-z0-9-]*" name != null;
  complete =
    name:
    let
      dir = targetRoot + "/${name}";
    in
    builtins.pathExists (dir + "/oracle.rs")
    && builtins.pathExists (dir + "/max_len")
    && builtins.pathExists (dir + "/dictionary.txt")
    && builtins.pathExists (dir + "/score.nix")
    && builtins.pathExists (dir + "/corpus/canonical")
    && builtins.pathExists (dir + "/corpus/empty")
    && builtins.pathExists (dir + "/corpus/bad_magic");
  maxLenOf =
    name:
    let
      raw = builtins.readFile (targetRoot + "/${name}/max_len");
      trimmed = lib.removeSuffix "\n" (lib.removeSuffix "\r" (lib.removeSuffix "\n" raw));
      value = lib.toInt trimmed;
    in
    if value > 0 && value <= 1048576 then
      value
    else
      abort "fuzz max_len for ${name} is ${toString value}; expected 1..=1048576";
  # Same coverage flags as cargo-bolero's libFuzzer engine. Without them
  # libFuzzer exits immediately with "no interesting inputs were found".
  libfuzzerRustflags = lib.concatStringsSep " " (
    [
      "--cfg fuzzing"
      "--cfg fuzzing_libfuzzer"
      "-C panic=unwind"
      "-Cpasses=sancov-module"
      "-Cllvm-args=-sanitizer-coverage-inline-8bit-counters"
      "-Cllvm-args=-sanitizer-coverage-level=4"
      "-Cllvm-args=-sanitizer-coverage-pc-table"
      "-Cllvm-args=-sanitizer-coverage-trace-compares"
    ]
    ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
      "-Cllvm-args=-sanitizer-coverage-stack-depth"
    ]
  );
  # One fail-closed gate. `rules` is `{ cond, message }`. The first false
  # cond aborts with `context` and that message. The value is returned only
  # when every cond holds.
  demand =
    context: rules: value:
    let
      failed = lib.findFirst (rule: !rule.cond) null rules;
    in
    if failed == null then value else abort "${context}: ${failed.message}";
  rankOf =
    name: score:
    let
      complexity = score.complexity;
      gap = score.gap;
      blast = score.blast;
      expect = complexity.loc + complexity.error_variants + complexity.discriminants;
    in
    demand name [
      {
        cond = complexity.method == "measured";
        message = "complexity.method must be measured";
      }
      {
        cond = complexity.score == expect;
        message = "complexity.score ${toString complexity.score} != ${toString expect}";
      }
      {
        cond = gap.method == "classified";
        message = "gap.method must be classified";
      }
      {
        cond = gap.llvm_cov_percent == null;
        message = "llvm-cov percent was not measured; leave llvm_cov_percent null";
      }
      {
        cond = builtins.elem gap.score [
          0
          2
          3
        ];
        message = "gap.score must be 0, 2, or 3";
      }
      {
        cond = blast.method == "classified";
        message = "blast.method must be classified";
      }
      {
        cond = builtins.elem blast.score [
          3
          4
          5
        ];
        message = "blast.score must be 3, 4, or 5";
      }
      {
        cond = score.churn.ranking_factor == false;
        message = "churn is not a ranking factor";
      }
      {
        cond = score.churn.method == "measured";
        message = "churn.method must be measured";
      }
      {
        cond = score.entrypoints != [ ];
        message = "entrypoints must name decoder sources";
      }
    ] (complexity.score * gap.score * blast.score);
  # Every target row uses these keys. Null means that kind has no such
  # artifact. start_order is the supervised start sequence. It is not rank.
  # warmVault is { name, corporaAttr }. durableMount is the argv[1] string.
  # bins.<id> accepts only that path.
  durableMountOf = id: "/durable/fuzz/${id}/corpus";
  corpusVault = id: {
    name = id;
    corporaAttr = "packages.\${system}.continuous-fuzz.corpora.${id}";
  };
  absentArtifacts = {
    bin = null;
    corpus = null;
    committed_corpus = null;
    warmVault = null;
    durableMount = null;
    dictionary = null;
    max_input_bytes = null;
  };
  targetMeta =
    name:
    let
      score = requirePriority name (import (targetRoot + "/${name}/score.nix"));
      rank = rankOf name score;
    in
    {
      id = name;
      kind = "fuzz";
      inherit (score) name;
      engine = "libfuzzer";
      bin = "packages.\${system}.continuous-fuzz.bins.${name}";
      corpus = "packages.\${system}.continuous-fuzz.corpora.${name}";
      committed_corpus = "tests/fuzz/targets/${name}/corpus";
      warmVault = corpusVault name;
      durableMount = durableMountOf name;
      dictionary = "tests/fuzz/targets/${name}/dictionary.txt";
      max_input_bytes = maxLenOf name;
      schedule = true;
      inherit rank;
      start_order = score.start_order;
      reason = null;
      inherit (score)
        complexity
        gap
        blast
        churn
        entrypoints
        ;
    };
  requirePriority =
    name: score:
    demand name [
      {
        cond = builtins.isInt score.start_order && score.start_order >= 1 && score.start_order <= 9;
        message = "start_order must be an integer from 1 through 9";
      }
    ] score;
  closed =
    spec:
    let
      rank = rankOf spec.id spec;
    in
    {
      inherit (spec)
        id
        name
        reason
        complexity
        gap
        blast
        churn
        entrypoints
        ;
      kind = "fuzz";
      engine = null;
      schedule = false;
      start_order = null;
      inherit rank;
    }
    // absentArtifacts;
  deferred =
    spec:
    {
      inherit (spec)
        id
        name
        reason
        complexity
        gap
        blast
        churn
        entrypoints
        ;
      kind = "fuzz";
      engine = null;
      schedule = false;
      start_order = null;
      rank = null;
    }
    // absentArtifacts;
  # Property rows are siblings of fuzz rows. They are not supervised bins.
  # Complexity stays null: those modules were not counted for this rank.
  propertyTarget =
    spec:
    {
      inherit (spec)
        id
        name
        complexity
        gap
        blast
        churn
        entrypoints
        ;
      kind = "property";
      engine = "property";
      schedule = false;
      start_order = null;
      rank = null;
      reason = null;
    }
    // absentArtifacts;
  unscored = locPaths: {
    complexity = {
      score = null;
      loc = null;
      error_variants = null;
      discriminants = null;
      method = "unscored";
      loc_paths = locPaths;
    };
    gap = {
      score = null;
      method = "unscored";
      label = "property-kind";
      llvm_cov_percent = null;
    };
    blast = {
      score = null;
      method = "unscored";
      boundary = "in-process";
    };
    churn = {
      commits = null;
      method = "unscored";
      ranking_factor = false;
      paths = locPaths;
    };
    entrypoints = locPaths;
  };
  # Evaluated only when the workspace sources exist. A config-only flake
  # leaves `contract` null instead of failing closed on a missing Cargo tree.
  discovered =
    if !available then
      null
    else
      demand "tests/fuzz/targets" [
        {
          cond = directoryNames != [ ];
          message = "no harness directories";
        }
        {
          cond = lib.all isTargetId directoryNames;
          message = "harness ids must match [a-z][a-z0-9-]*";
        }
        {
          cond = lib.all complete directoryNames;
          message = "a harness directory is missing oracle.rs, max_len, dictionary.txt, score.nix, or corpus/{canonical,empty,bad_magic}";
        }
        {
          cond =
            lib.length (
              lib.unique (map (name: (import (targetRoot + "/${name}/score.nix")).start_order) directoryNames)
            ) == lib.length directoryNames;
          message = "start_order values must be unique across harness directories";
        }
      ] directoryNames;
  metadata =
    if discovered == null then
      null
    else
      {
        schema = "nudox.continuous-fuzz.v1";
        # Document fields only. Nix Test DSL owns check.continuousFuzz.
        # This repository does not define that function.
        latticeKind = "continuous-fuzz";
        # Package WarmVault. Per-corpus records live on corpora.<id>.warmVault.
        warmVault = {
          name = "backend";
          corporaAttr = "packages.\${system}.continuous-fuzz.corpora";
        };
        attrs = {
          contract = "packages.\${system}.continuous-fuzz";
          bins = "packages.\${system}.continuous-fuzz.bins.<id>";
          corpora = "packages.\${system}.continuous-fuzz.corpora.<id>";
          engines = "packages.\${system}.continuous-fuzz.engines.<id>";
          metadata = "packages.\${system}.continuous-fuzz.metadata";
          short = ".#continuous-fuzz";
          optional_alias = "packages.\${system}.fuzz-<id>";
        };
        kinds = [
          "fuzz"
          "property"
        ];
        # Adapter catalog. linked is this repo. Auth copies the same records
        # and sets go-native.linked = true with adapter = "testing.F".
        engine_catalog = [
          {
            id = "libfuzzer";
            family = "coverage-guided";
            linked = true;
            adapter = "bolero";
          }
          {
            id = "aflpp";
            family = "coverage-guided";
            linked = false;
            adapter = null;
          }
          {
            id = "honggfuzz";
            family = "coverage-guided";
            linked = false;
            adapter = null;
          }
          {
            id = "go-native";
            family = "coverage-guided";
            linked = false;
            adapter = null;
          }
          {
            id = "property";
            family = "bounded";
            linked = true;
            adapter = "proptest";
          }
        ];
        formula = "complexity.score * gap.score * blast.score";
        scales = {
          complexity = "loc + error_variants + discriminants. method=measured counts source lines and enum variants. Not premultiplied by gap or blast.";
          gap = "Ordinal classification, method=classified, not a coverage percentage and not a measured DiffWake input. 0 = raw bytes already have an in-process engine or an exhaustive scan. 2 = fixed hostile examples only. 3 = decoder is not callable outside its package. 1 is unused. llvm_cov_percent stays null until it is measured. Rank multiplies measured complexity by this classified ordinal and by classified blast.";
          blast = "Trust boundary. 5 = untrusted remote or durable bytes. 4 = session frame from a peer. 3 = local process facade.";
          churn = "Measured commit counts. ranking_factor is false. Do not multiply churn into the rank.";
          start_order = "Ascending start sequence for scheduled fuzz targets. Independent of rank. Rank 0 with schedule true still runs; gap 0 makes the product zero. Property rows leave it null.";
        };
        target_fields = [
          "id"
          "kind"
          "name"
          "engine"
          "bin"
          "corpus"
          "committed_corpus"
          "warmVault"
          "durableMount"
          "dictionary"
          "max_input_bytes"
          "schedule"
          "rank"
          "start_order"
          "complexity"
          "gap"
          "blast"
          "churn"
          "entrypoints"
          "reason"
        ];
        consumers = {
          rank = "Recompute complexity.score * gap.score * blast.score. Skip rank null and schedule false. Do not multiply by churn. start_order is not an input. Do not trust list order.";
          corpus = "metadata.warmVault is the package record { name, corporaAttr } with name = backend. corpora.<id>.warmVault is the same shape with corporaAttr = packages.\${system}.continuous-fuzz.corpora.<id>. corpora.<id>.durableMount and the scheduled row's durableMount are the string /durable/fuzz/<id>/corpus. bins.<id> requires argv[1] to equal that durableMount and exits 2 otherwise. It does not fall back to TMPDIR, /tmp, or XDG state. Property and backlog rows set warmVault and durableMount null.";
          engines = "engines.<id>.adapter is the plug record (id, family, linked, adapter). This repo links libfuzzer. aflpp, honggfuzz, and go-native stay linked false. Do not point an unlinked engine at bins.<id>.";
          aliases = "packages.\${system}.fuzz-<id> aliases bins.<id>. The contract package is continuous-fuzz. Do not add either to nix flake check.";
        };
        supervision = {
          per_input_timeout_seconds = 10;
          rss_limit_mb = 4096;
          refuse = [
            "-max_total_time"
            "omitted-argv1"
            "corpus-path-other-than-/durable/fuzz/<name>/corpus"
          ];
          artifact_directory = "sibling artifacts/ of the durable corpus, never inside it";
        };
        throughput_note = "exec/s and time-to-useful-coverage are not flake attributes. Confirmed campaign numbers live in docs/operations/continuous-fuzzing.md.";
        targets = map targetMeta discovered ++ [
          (propertyTarget (
            {
              id = "laws";
              name = "structured laws";
            }
            // (unscored [ "tests/laws" ])
          ))
          (propertyTarget (
            {
              id = "store-frame";
              name = "store frame property";
            }
            // (unscored [ "crates/store/src/view/validate/raw_property.rs" ])
          ))
        ];
        backlog = [
          (closed {
            id = "session-frame";
            name = "session frame";
            reason = "The native envelope is fuzz-native-protocol. This session frame decoder is a separate grammar and is not that package.";
            complexity = {
              score = 488;
              loc = 477;
              error_variants = 6;
              discriminants = 5;
              method = "measured";
              loc_paths = [ "crates/compile/src/frame.rs" ];
              error_enums = [ "crates/compile/src/errors.rs FrameError" ];
              discriminant_note = "FrameKind variants";
            };
            gap = {
              score = 2;
              method = "classified";
              label = "fixed-examples-only";
              llvm_cov_percent = null;
            };
            blast = {
              score = 4;
              method = "classified";
              boundary = "session-frame";
            };
            churn = {
              commits = 1;
              method = "measured";
              ranking_factor = false;
              paths = [ "crates/compile/src/frame.rs" ];
            };
            entrypoints = [ "crates/compile/src/frame.rs" ];
          })
          (deferred {
            id = "json-command";
            name = "json command grammar";
            reason = "decode_command_dto is serde_json::from_slice. crates/library/protocol/json/wire projects serde_json values, and apps/mcp/src/jsonrpc/codec.rs frames serde_json::Value. None of those is a byte grammar distinct from serde.";
            complexity = {
              score = null;
              loc = 1121;
              error_variants = null;
              discriminants = null;
              method = "measured-loc-only";
              loc_paths = [
                "crates/library/wire/codec.rs"
                "crates/library/wire/command.rs"
              ];
            };
            gap = {
              score = 2;
              method = "classified";
              label = "fixed-examples-only";
              llvm_cov_percent = null;
            };
            blast = {
              score = 3;
              method = "classified";
              boundary = "local-process-facade";
            };
            churn = {
              commits = 10;
              method = "measured";
              ranking_factor = false;
              paths = [ "crates/library/wire" ];
            };
            entrypoints = [ "crates/engine/src/lib.rs" ];
          })
          (deferred {
            id = "store-journal";
            name = "recovery journal";
            reason = "decode_record is pub(super) in crates/store/src/durable/recovery_journal.rs. Widening it is a product API change, so complexity is not scored.";
            complexity = {
              score = null;
              loc = 394;
              error_variants = null;
              discriminants = null;
              method = "unscored";
              loc_paths = [ "crates/store/src/durable/recovery_journal.rs" ];
            };
            gap = {
              score = null;
              method = "unscored";
              label = "not-callable-outside-crate";
              llvm_cov_percent = null;
            };
            blast = {
              score = 5;
              method = "classified";
              boundary = "durable-journal";
            };
            churn = {
              commits = 1;
              method = "measured";
              ranking_factor = false;
              paths = [ "crates/store/src/durable/recovery_journal.rs" ];
            };
            entrypoints = [ "crates/store/src/durable/recovery_journal.rs" ];
          })
          (deferred {
            id = "store-pack";
            name = "pack admission";
            reason = "admit_pack and decode_wire_pack take a structured WirePack. A raw-byte harness would be a second parser.";
            complexity = {
              score = null;
              loc = 310;
              error_variants = null;
              discriminants = null;
              method = "unscored";
              loc_paths = [ "crates/store/src/pack.rs" ];
            };
            gap = {
              score = null;
              method = "unscored";
              label = "not-a-byte-decoder";
              llvm_cov_percent = null;
            };
            blast = {
              score = null;
              method = "unscored";
              boundary = "structured-wire-pack";
            };
            churn = {
              commits = 1;
              method = "measured";
              ranking_factor = false;
              paths = [ "crates/store/src/pack.rs" ];
            };
            entrypoints = [ "crates/store/src/pack.rs" ];
          })
        ];
      };
  metadataFile =
    if metadata == null then
      null
    else
      pkgs.writeText "continuous-fuzz-metadata.json" (builtins.toJSON metadata);
  engineBinary =
    if discovered == null then
      null
    else
      stableRustPlatform.buildRustPackage {
        pname = "backend-fuzz-engine";
        version = "0.1.0";
        src = workspaceSource;
        cargoBuildFlags = [
          "--package"
          "backend-fuzz"
          "--bin"
          "fuzz-target"
        ];
        cargoLock = {
          lockFile = workspaceRoot + "/Cargo.lock";
          outputHashes = gpuiOutputHashes;
        };
        nativeBuildInputs = [
          pkgs.python3
        ];
        # Release profile aborts on panic, which defeats bolero shrinking.
        # The patch is local to this derivation. The workspace profile stays abort.
        postPatch = ''
          python3 - <<'PY'
          from pathlib import Path
          path = Path("Cargo.toml")
          text = path.read_text()
          start = text.find("members = [")
          end = text.find("]", start)
          if start < 0 or end < 0:
              raise SystemExit("workspace members block not found")
          # The full member globs pull apps that path-patch gpui. Those vendor
          # trees are not in the flake source. Keep the fuzz crate closure.
          members = [
              "crates/advisory",
              "crates/compile",
              "crates/discovery",
              "crates/engine",
              "crates/execution",
              "crates/flow",
              "crates/library",
              "crates/platform",
              "crates/replication",
              "crates/runtime",
              "crates/semantic",
              "crates/store",
              "crates/version",
              "extensions/qdrant",
              "extensions/tantivy",
              "extensions/trustfall",
              "frontends/clang",
              "frontends/csharp",
              "frontends/go",
              "frontends/java",
              "frontends/python",
              "frontends/rust",
              "frontends/typescript",
              "tests/fuzz",
          ]
          block = "members = [\n" + "".join(f'    "{name}",\n' for name in members) + "]"
          text = text[:start] + block + text[end + 1 :]
          patch = text.find("\n[patch.crates-io]\n")
          if patch < 0:
              raise SystemExit("patch.crates-io section not found")
          text = text[:patch] + "\n"
          text = text.replace('panic = "abort"', 'panic = "unwind"', 1)
          text = text.replace('strip = "symbols"', 'strip = "none"', 1)
          path.write_text(text)
          patched = path.read_text()
          if "tests/fuzz" not in patched:
              raise SystemExit("fuzz member was not written")
          if "[patch.crates-io]" in patched:
              raise SystemExit("gpui patch section was not removed")
          if 'panic = "unwind"' not in patched or 'strip = "none"' not in patched:
              raise SystemExit("release profile was not patched")
          PY
        '';
        # RUSTC_WRAPPER below replaces the nix rustc wrapper, which is what
        # normally injects -rpath. The installed binary NEEDs libstdc++ with
        # an empty RUNPATH. `linkedEngine` repairs that without a second
        # cargo build.
        preConfigure = ''
          # `fuzzing_libfuzzer` selects the engine. `fuzzing` keeps bolero from
          # naming the uncompiled test module. The sancov flags are what make
          # libFuzzer observe edges; a binary without them exits 1.
          # The wrapper drops those flags for crates that are not listed in
          # tests/fuzz/instrumented.
          chmod +x tests/fuzz/libfuzzer-rustc
          # The sandbox has no /bin/bash. Point the wrapper at the nix bash.
          patchShebangs tests/fuzz/libfuzzer-rustc
          export RUSTC_WRAPPER=$PWD/tests/fuzz/libfuzzer-rustc
          export RUSTFLAGS="''${RUSTFLAGS:+$RUSTFLAGS }${libfuzzerRustflags}"
        '';
        doCheck = false;
        dontStrip = true;
        installPhase = ''
          runHook preInstall
          mkdir -p "$out/libexec"
          # cargoBuildHook passes --target, so the bin is not in target/release.
          bin=$(find target -type f -path '*/release/fuzz-target' -print -quit)
          test -n "$bin"
          cp "$bin" "$out/libexec/fuzz-target"
          runHook postInstall
        '';
      };
  # The cargo derivation's RUNPATH is empty (see the comment on RUSTFLAGS).
  # Copy the cached binary and point it at the same gcc lib the toolchain
  # linked. Bins exec this path. The unpatched libexec is not a runner.
  linkedEngine =
    if engineBinary == null then
      null
    else
      pkgs.runCommand "backend-fuzz-engine-linked"
        {
          nativeBuildInputs = [ pkgs.patchelf ];
        }
        ''
          mkdir -p "$out/libexec"
          cp ${engineBinary}/libexec/fuzz-target "$out/libexec/fuzz-target"
          chmod u+w "$out/libexec/fuzz-target"
          patchelf --set-rpath ${lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib ]} "$out/libexec/fuzz-target"
        '';
  corpora =
    if discovered == null then
      null
    else
      lib.listToAttrs (
        map (name: {
          inherit name;
          value =
            pkgs.runCommand "fuzz-corpus-${name}"
              {
                passthru = {
                  warmVault = corpusVault name;
                  durableMount = durableMountOf name;
                };
              }
              ''
                mkdir -p "$out"
                find ${targetRoot + "/${name}/corpus"} -type f ! -name '.*' -exec cp -a {} "$out/" \;
                test -f "$out/canonical"
                test -f "$out/empty"
                test -f "$out/bad_magic"
              '';
        }) discovered
      );
  dicts =
    if discovered == null then
      null
    else
      lib.listToAttrs (
        map (name: {
          inherit name;
          value = pkgs.runCommand "fuzz-dict-${name}" { } ''
            cp ${targetRoot + "/${name}/dictionary.txt"} "$out"
          '';
        }) discovered
      );
  engines =
    if discovered == null then
      null
    else
      lib.listToAttrs (
        map (
          name:
          let
            script = pkgs.writeShellScript "fuzz-engine-${name}.sh" ''
              set -eu
              if [ -z "''${BOLERO_LIBFUZZER_ARGS:-}" ]; then
                echo "engine adapter env is unset. Supervised runs use bins.${name}, which sets the durable corpus and the caps." >&2
                exit 2
              fi
              export FUZZ_TARGET=${name}
              exec ${linkedEngine}/libexec/fuzz-target
            '';
          in
          {
            inherit name;
            value =
              pkgs.runCommand "fuzz-engine-${name}"
                {
                  passthru.adapter = {
                    id = "libfuzzer";
                    family = "coverage-guided";
                    linked = true;
                    adapter = "bolero";
                  };
                }
                ''
                  mkdir -p "$out/bin"
                  cp ${script} "$out/bin/fuzz-engine-${name}"
                  chmod +x "$out/bin/fuzz-engine-${name}"
                '';
          }
        ) discovered
      );
  bins =
    if discovered == null then
      null
    else
      lib.listToAttrs (
        map (
          name:
          let
            maxLen = maxLenOf name;
          in
          {
            inherit name;
            value = pkgs.writeShellScriptBin "fuzz-${name}" ''
              set -eu
              seeds=${corpora.${name}}
              dict=${dicts.${name}}
              mount=${durableMountOf name}
              if [ "$#" -lt 1 ] || [ "$1" != "$mount" ]; then
                echo "refusing to start: argv[1] must be $mount. bins.${name} does not create a corpus under TMPDIR, /tmp, or XDG state." >&2
                exit 2
              fi
              writable=$1
              shift
              artifacts=$(dirname "$writable")/artifacts
              case "$writable$seeds$dict$artifacts" in
                *" "*)
                  echo "fuzz paths must not contain spaces; bolero splits BOLERO_LIBFUZZER_ARGS on spaces" >&2
                  exit 2
                  ;;
              esac
              extra=""
              for arg in "$@"; do
                case "$arg" in
                  *" "*)
                    echo "extra libFuzzer arguments must not contain spaces" >&2
                    exit 2
                    ;;
                  -max_total_time*)
                    echo "refusing -max_total_time; the supervisor owns campaign lifetime. -timeout is the per-input hang cap." >&2
                    exit 2
                    ;;
                esac
                extra="$extra $arg"
              done
              mkdir -p "$writable" "$artifacts"
              unset BOLERO_RANDOM_ITERATIONS BOLERO_RANDOM_TEST_TIME_MS BOLERO_RANDOM_MAX_LEN
              export BOLERO_LIBFUZZER_ARGS="-timeout=10 -rss_limit_mb=4096 -max_len=${toString maxLen} -dict=$dict -artifact_prefix=$artifacts/ $writable $seeds$extra"
              exec ${engines.${name}}/bin/fuzz-engine-${name}
            '';
          }
        ) discovered
      );
  bundle =
    if discovered == null then
      null
    else
      pkgs.runCommand "continuous-fuzz"
        {
          passthru = {
            inherit
              bins
              corpora
              engines
              metadata
              ;
          };
        }
        ''
          mkdir -p "$out/bin" "$out/engines" "$out/corpora"
          ${lib.concatMapStrings (name: ''
            ln -s ${bins.${name}}/bin/fuzz-${name} "$out/bin/fuzz-${name}"
            ln -s ${engines.${name}}/bin/fuzz-engine-${name} "$out/engines/fuzz-engine-${name}"
            mkdir -p "$out/corpora/${name}"
            cp -a ${corpora.${name}}/. "$out/corpora/${name}/"
          '') discovered}
          cp ${metadataFile} "$out/metadata.json"
        '';
  aliases =
    if bins == null then
      null
    else
      lib.listToAttrs (map (name: lib.nameValuePair "fuzz-${name}" bins.${name}) discovered);
in
{
  contract =
    if bundle == null then
      null
    else
      {
        inherit bundle aliases;
      };
}
