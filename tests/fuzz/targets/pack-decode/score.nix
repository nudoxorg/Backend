# DiffWake inputs for untrusted index-pack bytes.
# Counts were measured with wc and enum reads on 2026-09-27. Gap is classified.
# complexity.score is loc + error_variants + discriminants, before blast and gap.
{
  name = "index pack decode";
  start_order = 1;
  complexity = {
    score = 2775;
    loc = 2737;
    error_variants = 33;
    discriminants = 5;
    method = "measured";
    loc_paths = [
      "crates/engine/src/index_publish/pack.rs"
      "crates/engine/src/index_publish/pack/view.rs"
      "crates/engine/src/index_publish/pack/grammar.rs"
      "crates/engine/src/index_publish/pack/encode.rs"
      "crates/engine/src/index_publish/pack/error.rs"
      "crates/engine/src/index_publish/pack/error/open.rs"
      "crates/engine/src/index_publish/pack/error/encode.rs"
    ];
    loc_note = "wc -l of the pack codec. pack/store.rs is filesystem publication and is excluded.";
    error_enums = [
      "crates/engine/src/index_publish/pack/error/open.rs IndexPackOpenError"
      "crates/engine/src/index_publish/pack/error/encode.rs IndexPackEncodeError"
    ];
    discriminant_note = "NUDXIPK plus IndexPackLane Exact and Lexical plus row states tombstone and present";
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
    boundary = "untrusted-durable-index-pack";
  };
  churn = {
    commits = 5;
    method = "measured";
    ranking_factor = false;
    paths = [ "crates/engine/src/index_publish/pack" "crates/engine/src/index_publish/pack.rs" ];
  };
  entrypoints = [
    "crates/engine/src/index_publish/pack/view.rs"
  ];
}
