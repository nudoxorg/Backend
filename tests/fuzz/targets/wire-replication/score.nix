# DiffWake inputs for the replication RPL2 harness.
# Counts were measured with wc and enum reads on 2026-09-27. Gap is classified.
# complexity.score is loc + error_variants + discriminants, before blast and gap.
{
  name = "replication RPL2";
  start_order = 6;
  complexity = {
    score = 1431;
    loc = 1380;
    error_variants = 36;
    discriminants = 15;
    method = "measured";
    loc_paths = [ "crates/replication/src/codec" ];
    error_enums = [ "crates/replication/src/transport/error.rs ReplicationError" ];
    discriminant_note = "TAG_CAPABILITIES through TAG_CLOSURE_NEED_REQUEST";
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
    boundary = "untrusted-replication-frame";
  };
  churn = {
    commits = 1;
    method = "measured";
    ranking_factor = false;
    paths = [ "crates/replication/src/codec" ];
  };
  entrypoints = [
    "crates/replication/src/codec/messages.rs"
    "crates/replication/src/transport/message.rs"
  ];
}
