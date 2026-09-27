# DiffWake inputs for the workspace wire harness.
# Counts were measured with wc and enum reads on 2026-09-27. Gap is classified.
# complexity.score is loc + error_variants + discriminants, before blast and gap.
{
  name = "workspace wire";
  start_order = 5;
  complexity = {
    score = 2557;
    loc = 2521;
    error_variants = 31;
    discriminants = 5;
    method = "measured";
    loc_paths = [ "crates/version/src/workspace" ];
    error_enums = [
      "crates/version/src/workspace/wire.rs WorkspaceDecodeError"
      "crates/version/src/workspace/errors.rs WorkspaceError"
    ];
    discriminant_note = "WMF2 WDL2 WCM2 WTR2 WPR2";
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
    boundary = "untrusted-workspace-wire";
  };
  churn = {
    commits = 1;
    method = "measured";
    ranking_factor = false;
    paths = [ "crates/version/src/workspace" ];
  };
  entrypoints = [
    "crates/version/src/workspace/manifest.rs"
    "crates/version/src/workspace/delta.rs"
    "crates/version/src/workspace/commit.rs"
  ];
}
