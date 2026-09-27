# DiffWake inputs for the structured flow evaluator.
# Counts were measured with wc and enum reads on 2026-09-27. Gap is classified.
# complexity.score is loc + error_variants + discriminants, before blast and gap.
{
  name = "flow evaluator";
  start_order = 7;
  complexity = {
    score = 796;
    loc = 772;
    error_variants = 23;
    discriminants = 1;
    method = "measured";
    loc_paths = [
      "crates/flow/src/operators/support.rs"
      "crates/flow/src/operators/stateless.rs"
      "crates/flow/src/types/errors.rs"
      "crates/flow/src/batch/schema.rs"
    ];
    loc_note = "wc -l of reduce_rows, distinct, filter, FlowError, and batch/schema.rs because consolidate_rows lives there. types/row.rs and types/time.rs are value holders and are not included.";
    error_enums = [ "crates/flow/src/types/errors.rs FlowError" ];
    discriminant_note = "FLW1. The record layout is fixed-width, not a tag enum.";
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
    boundary = "structured-flow-evaluator";
  };
  churn = {
    commits = 1;
    method = "measured";
    ranking_factor = false;
    paths = [ "crates/flow/src/operators" ];
  };
  entrypoints = [
    "crates/flow/src/operators/support.rs"
    "crates/flow/src/operators/stateless.rs"
    "tests/fuzz/targets/flow-evaluator/oracle.rs"
  ];
}
