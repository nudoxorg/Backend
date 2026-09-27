# Exports one continuous-fuzz attrset plus packages.${system}.fuzz-<id> aliases.
# ilo builds those aliases. argv[1] is the durable corpus directory to mount.
# These packages are libFuzzer runners. They are intentionally not flake checks.
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
  complete = name:
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
  maxLenOf = name:
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
  rankOf =
    name: score:
    let
      complexity = score.complexity;
      gap = score.gap;
      blast = score.blast;
      expect = complexity.loc + complexity.error_variants + complexity.discriminants;
    in
    assert (complexity.method == "measured") || abort "${name}: complexity.method must be measured";
    assert (complexity.score == expect)
      || abort "${name}: complexity.score ${toString complexity.score} != ${toString expect}";
    assert (gap.method == "classified") || abort "${name}: gap.method must be classified";
    assert (gap.llvm_cov_percent == null)
      || abort "${name}: llvm-cov percent was not measured; leave llvm_cov_percent null";
    assert (builtins.elem gap.score [ 0 2 3 ]) || abort "${name}: gap.score must be 0, 2, or 3";
    assert (blast.method == "classified") || abort "${name}: blast.method must be classified";
    assert (builtins.elem blast.score [ 3 4 5 ]) || abort "${name}: blast.score must be 3, 4, or 5";
    assert (score.churn.ranking_factor == false) || abort "${name}: churn is not a ranking factor";
    assert (score.churn.method == "measured") || abort "${name}: churn.method must be measured";
    assert (score.entrypoints != [ ]) || abort "${name}: entrypoints must name decoder sources";
    complexity.score * gap.score * blast.score;
  targetMeta = name:
    let
      score = requirePriority name (import (targetRoot + "/${name}/score.nix"));
      rank = rankOf name score;
    in
    {
      id = name;
      inherit (score) name;
      bin = "packages.\${system}.continuous-fuzz.bins.${name}";
      corpus = "packages.\${system}.continuous-fuzz.corpora.${name}";
      engine = "packages.\${system}.continuous-fuzz.engines.${name}";
      engine_hint = "bolero-libfuzzer";
      ci_engine_hint = "bolero-test";
      committed_corpus = "tests/fuzz/targets/${name}/corpus";
      durable_corpus = "/durable/fuzz/${name}/corpus";
      dictionary = "tests/fuzz/targets/${name}/dictionary.txt";
      max_len = maxLenOf name;
      harnessed = true;
      schedule = true;
      inherit rank;
      inherit (score)
        ilo_priority
        complexity
        gap
        blast
        churn
        entrypoints
        ;
    };
  # `|| abort` so a missing or non-integer ilo_priority fails closed at eval.
  requirePriority = name: score:
    assert (builtins.isInt score.ilo_priority && score.ilo_priority >= 1 && score.ilo_priority <= 9)
      || abort "${name}: ilo_priority must be an integer from 1 through 9";
    score;
  closed = spec:
    let
      rank = rankOf spec.id spec;
    in
    spec
    // {
      harnessed = false;
      schedule = false;
      bin = null;
      corpus = null;
      engine = null;
      engine_hint = null;
      ci_engine_hint = null;
      committed_corpus = null;
      durable_corpus = null;
      dictionary = null;
      max_len = null;
      inherit rank;
    };
  deferred = spec:
    spec
    // {
      harnessed = false;
      schedule = false;
      bin = null;
      corpus = null;
      engine = null;
      engine_hint = null;
      ci_engine_hint = null;
      committed_corpus = null;
      durable_corpus = null;
      dictionary = null;
      max_len = null;
      rank = null;
    };
  # Evaluated only when the workspace sources exist. A config-only flake
  # leaves `contract` null instead of failing closed on a missing Cargo tree.
  discovered =
    if !available then
      null
    else
      assert (directoryNames != [ ]) || abort "tests/fuzz/targets has no harness directories";
      assert (lib.all isTargetId directoryNames)
        || abort "harness ids must match [a-z][a-z0-9-]*";
      assert (lib.all complete directoryNames)
        || abort "a harness directory is missing oracle.rs, max_len, dictionary.txt, score.nix, or corpus/{canonical,empty,bad_magic}";
      directoryNames;
  metadata =
    if discovered == null then
      null
    else
      {
        schema = "backend.continuous-fuzz.v1";
        attrs = {
          contract = "packages.\${system}.continuous-fuzz";
          bins = "packages.\${system}.continuous-fuzz.bins.<id>";
          corpora = "packages.\${system}.continuous-fuzz.corpora.<id>";
          engines = "packages.\${system}.continuous-fuzz.engines.<id>";
          metadata = "packages.\${system}.continuous-fuzz.metadata";
          short = ".#continuous-fuzz";
          optional_alias = "packages.\${system}.fuzz-<id>";
          fuzz_packages = [
            ".#fuzz-store-raw-property"
            ".#fuzz-flow-evaluator"
            ".#fuzz-wire-workspace"
            ".#fuzz-wire-replication"
          ];
          corpus_mount = "/durable/fuzz/<id>/corpus";
          committed_corpus = "tests/fuzz/targets/<id>/corpus";
          systems = [
            "aarch64-darwin"
            "aarch64-linux"
            "x86_64-linux"
          ];
        };
        formula = "complexity.score * gap.score * blast.score";
        scales = {
          complexity = "loc + error_variants + discriminants. method=measured counts wc -l and enum variants. Not premultiplied by gap or blast.";
          gap = "Ordinal classification, not a coverage percentage. 0 = raw bytes already have bolero or an exhaustive scan. 2 = fixed hostile examples only. 3 = decoder is not callable outside its crate. 1 is unused. llvm_cov_percent stays null until it is measured.";
          blast = "Trust boundary. 5 = untrusted remote or durable bytes. 4 = session frame from a peer. 3 = local process facade.";
          churn = "Measured git log --oneline counts. ranking_factor is false. Do not multiply churn into the rank; history includes an import and commit counts are not comparable.";
          ilo_priority = "Ascending systemd start order for packages.\${system}.fuzz-<id>. Independent of rank. Rank 0 with schedule true still runs; gap 0 makes the product zero.";
        };
        ilo = {
          packages = "nix build .#fuzz-<id>. The derivation is the supervised runner.";
          order = "Start fuzz-<id> by metadata.targets[].ilo_priority ascending. Do not sort the units by rank.";
          mount = "Create /durable/fuzz/<id>/corpus and pass it as argv[1]. The wrapper also passes corpora.<id> as a read-only second corpus. New coverage is written only to the mount.";
          engine = "bolero-libfuzzer. Ziggy should select its libFuzzer engine, or exec the script with the corpus mount. AFL++ forkserver and honggfuzz persistent mode are not linked into this binary.";
        };
        ember = {
          warm_vault = "On first start, copy corpora.<id> into metadata.targets[].durable_corpus (/durable/fuzz/<id>/corpus). That directory is the incremental corpus. It must survive process restarts and replacement of the nix store paths.";
          diff_wake = "Rank metadata.targets by rank, which is complexity.score * gap.score * blast.score. Recompute it; do not trust list order. Skip every row whose rank is null or whose schedule is false. Do not multiply by churn. ilo_priority is not an input.";
          fabric = "Build packages.\${system}.fuzz-<id> or .#continuous-fuzz. Do not add these packages to nix flake check. The rust build is one shared engine; corpora.<id> does not depend on it.";
          triage_plane = "Alert when bins.<id> exits non-zero and the artifacts/ directory beside the durable corpus contains a file. Wrapper exit 2, an empty artifacts directory, and a missing BOLERO_LIBFUZZER_ARGS are supervision failures.";
          nudox_fuzz = "nix build .#fuzz-<id> && ./result/bin/fuzz-<id> /durable/fuzz/<id>/corpus";
        };
        reentry = {
          add_target = "Add tests/fuzz/targets/<id>/{oracle.rs,max_len,dictionary.txt,score.nix,corpus/{canonical,empty,bad_magic}}. Do not add a Cargo bin and do not edit default.nix. build.rs and this module discover the directory. Add a line to tests/fuzz/instrumented only when a new decoder crate joins the link closure.";
          resume = "Replace the store path of bins.<id> and pass the same /durable/fuzz/<id>/corpus. LibFuzzer loads that directory first and writes new coverage only there. corpora.<id> is passed second, read-only, so committed seeds survive an empty durable dir.";
          seeds_vs_corpus = "corpora.<id> is the committed seed derivation. The durable directory is the corpus that grows. Code changes do not rebuild corpora.<id> unless the seed files change.";
          laws = "tests/laws/proptest-regressions/lib.txt stores proptest RNG fingerprints, not wire bytes. If a shrink comment contains bytes, copy the minimized buffer into targets/<id>/corpus/<flat-name> and re-run cargo test -p backend-fuzz. Do not name the file after the fingerprint.";
        };
        ci = {
          command = "cargo test -p backend-fuzz";
          engine_hint = "bolero-test";
          iterations = 16;
          test_time_ms = 150;
          note = "Do not export --cfg fuzzing, --cfg fuzzing_libfuzzer, or BOLERO_RANDOM_* into this command. The bounded run is a smoke cap. Acceptance is the committed canonical seed.";
        };
        supervision = {
          per_input_timeout_seconds = 10;
          rss_limit_mb = 4096;
          refuse = [ "-max_total_time" ];
          artifact_directory = "sibling artifacts/ of the durable corpus, never inside it";
        };
        laws_regressions = "tests/laws/proptest-regressions/lib.txt";
        linked_crates = [
          "tests/fuzz"
          "crates/flow"
          "crates/replication"
          "crates/store"
          "crates/version"
          "crates/platform"
        ];
        throughput_note = "exec/s and time-to-useful-coverage are not flake attributes. Nix does not measure them. Confirmed campaign numbers live in docs/operations/continuous-fuzzing.md and the pull request.";
        targets = map targetMeta discovered;
        backlog = [
          (closed {
            id = "native-envelope";
            name = "native envelope";
            reason = "Compile/engine decode, the fourth ilo choice, was not exported. NativeEnvelope::decode is public, but backend-compile pulls semantic, tree-sitter, and turso. It is outside this slice's link closure.";
            complexity = {
              score = 893;
              loc = 867;
              error_variants = 20;
              discriminants = 6;
              method = "measured";
              loc_paths = [
                "crates/compile/src/native_protocol.rs"
                "crates/compile/src/native_protocol_codec.rs"
                "crates/compile/src/native_protocol_admission.rs"
                "crates/compile/src/native_protocol_session.rs"
              ];
              error_enums = [ "crates/compile/src/native_protocol.rs NativeProtocolError" ];
              discriminant_note = "NativeRecordKind variants";
            };
            gap = {
              score = 2;
              method = "classified";
              label = "fixed-examples-only";
              llvm_cov_percent = null;
            };
            blast = {
              score = 5;
              method = "classified";
              boundary = "untrusted-native-payload";
            };
            churn = {
              commits = 1;
              method = "measured";
              ranking_factor = false;
              paths = [ "crates/compile/src/native_protocol.rs" ];
            };
            entrypoints = [ "crates/compile/src/native_protocol_codec.rs" ];
          })
          (closed {
            id = "session-frame";
            name = "session frame";
            reason = "Deferred with native-envelope. The frame decoder lives in backend-compile.";
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
            reason = "decode_command_dto in crates/engine/src/lib.rs is serde_json::from_slice. Random bytes die before admission. Engine commit count is not parser complexity.";
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
    if metadata == null then null else pkgs.writeText "continuous-fuzz-metadata.json" (builtins.toJSON metadata);
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
          members = """members = [
              "tests/fuzz",
              "crates/flow",
              "crates/replication",
              "crates/store",
              "crates/version",
              "crates/platform",
          ]"""
          text = text[:start] + members + text[end + 1 :]
          text = text.replace('panic = "abort"', 'panic = "unwind"', 1)
          text = text.replace('strip = "symbols"', 'strip = "none"', 1)
          path.write_text(text)
          if "tests/fuzz" not in path.read_text():
              raise SystemExit("fuzz member was not written")
          PY
        '';
        preConfigure = ''
          # `fuzzing_libfuzzer` selects the engine. `fuzzing` keeps bolero from
          # naming the uncompiled test module. The sancov flags are what make
          # libFuzzer observe edges; a binary without them exits 1.
          # The wrapper drops those flags for crates that are not listed in
          # tests/fuzz/instrumented.
          chmod +x tests/fuzz/libfuzzer-rustc
          export RUSTC_WRAPPER=$PWD/tests/fuzz/libfuzzer-rustc
          export RUSTFLAGS="''${RUSTFLAGS:+$RUSTFLAGS }${libfuzzerRustflags}"
        '';
        doCheck = false;
        dontStrip = true;
        installPhase = ''
          runHook preInstall
          mkdir -p "$out/libexec"
          cp target/release/fuzz-target "$out/libexec/fuzz-target"
          runHook postInstall
        '';
      };
  corpora =
    if discovered == null then
      null
    else
      lib.listToAttrs (
        map (name: {
          inherit name;
          value = pkgs.runCommand "fuzz-corpus-${name}" { } ''
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
        map (name: {
          inherit name;
          value = pkgs.writeShellScriptBin "fuzz-engine-${name}" ''
            set -eu
            if [ -z "''${BOLERO_LIBFUZZER_ARGS:-}" ]; then
              echo "BOLERO_LIBFUZZER_ARGS is unset. Supervised runs use fuzz-${name}, which sets the durable corpus and the caps." >&2
              exit 2
            fi
            export FUZZ_TARGET=${name}
            exec ${engineBinary}/libexec/fuzz-target
          '';
        }) discovered
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
              writable=""
              if [ "$#" -ge 1 ] && [ "''${1#-}" = "$1" ]; then
                writable=$1
                shift
              else
                if [ -n "''${XDG_STATE_HOME:-}" ]; then
                  base=$XDG_STATE_HOME
                elif [ -n "''${HOME:-}" ]; then
                  base=$HOME/.local/state
                else
                  base=''${TMPDIR:-/tmp}
                fi
                writable=$base/backend-fuzz/${name}/corpus
              fi
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
      lib.listToAttrs (
        map (name: lib.nameValuePair "fuzz-${name}" bins.${name}) discovered
      );
in
{
  contract = if bundle == null then null else {
    inherit bundle aliases;
  };
}
