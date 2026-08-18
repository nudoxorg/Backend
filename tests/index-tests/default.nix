# The `index` crate under `--features server`: the serving and indexing plane.
#
# See `check.nu` for why this is a separate check from `workspace-tests` — in
# one line: `server` is not a default feature, so the workspace check never
# compiled it, and it once stopped compiling entirely with every check green.
{
  pkgs,
  mkNuCheck,
  nuLib,
  projectRoot,
  rustToolchain,
  smolvmSource,
}:

let
  vendoredSources = import ../lib/vendored-sources.nix {
    inherit pkgs smolvmSource;
  };
in

mkNuCheck {
  name = "index-tests";
  script = ./check.nu;
  src = ../..;
  inherit nuLib;

  runtimeInputs =
    with pkgs;
    [
      cargo-nextest
      git
      pkg-config
      # `index::pack` links zstd through `zstd-sys`, and the vendored
      # `zstd-seekable` fork compiles its seekable wrapper against the same
      # headers. cmake/clang are what those build scripts reach for.
      cmake
    ]
    ++ [ rustToolchain ];

  # Cargo's `mod` declarations make every Rust file under the crate a declared
  # input, so a dirty worktree with untracked modules fails to evaluate rather
  # than quietly building a smaller crate. Scoped to `.rs` under the two trees
  # this check compiles that are most likely to hold untracked work.
  extraSrcTrees = [
    {
      relPath = "workspace/index";
      source = builtins.path {
        path = "${projectRoot}/workspace/index";
        name = "index-test-index-rust-src";
        filter = path: type: type == "directory" || builtins.match ".*\\.rs$" path != null;
      };
    }
  ];

  preBuild = ''
    ${vendoredSources}

    # The unpacked Nix source has no checkout metadata, but tests that ask Git
    # what a clean checkout contains must see a real index built from THIS
    # filtered source — after every overlay above has been materialized. An
    # empty repository here would let those tests pass vacuously.
    git init --quiet .
    git add --all
  '';

  # Needed for `git` and for the build scripts that fetch pinned sources.
  noChroot = true;
  preferLocalBuild = true;
  allowSubstitutes = false;

  resultLines = [
    "index-tests: ok"
    "gate: cargo nextest --locked -p index --features server --no-run (all targets compile)"
    "suite: cargo nextest --locked -p index --features server --lib (945 unit tests)"
    "excluded: 9 integration targets requiring live backends; see tests/index-tests/check.nu"
  ];
}
