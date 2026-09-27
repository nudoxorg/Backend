# DiffWake inputs for effect and dispatch journal record bytes.
# Counts were measured with wc and enum reads on 2026-09-27. Gap is classified.
# complexity.score is loc + error_variants + discriminants, before blast and gap.
{
  name = "journal codec";
  start_order = 2;
  complexity = {
    score = 799;
    loc = 763;
    error_variants = 18;
    discriminants = 18;
    method = "measured";
    loc_paths = [
      "crates/engine/src/effects/journal_codec/mod.rs"
      "crates/engine/src/effects/journal_codec/record.rs"
      "crates/engine/src/dispatch/journal/codec.rs"
    ];
    loc_note = "wc -l of the two record codecs. The hash-chain frame in journal/record.rs is not the fuzzed grammar.";
    error_enums = [
      "crates/engine/src/journal/record.rs JournalError"
      "crates/engine/src/dispatch/journal/types.rs DispatchRecordError"
    ];
    discriminant_note = "EffectJournalRecord 5, AmbiguousReason 4, DispatchRecord 9";
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
    boundary = "untrusted-durable-journal";
  };
  churn = {
    commits = 5;
    method = "measured";
    ranking_factor = false;
    paths = [
      "crates/engine/src/effects/journal_codec"
      "crates/engine/src/dispatch/journal/codec.rs"
    ];
  };
  entrypoints = [
    "crates/engine/src/effects/journal_codec/record.rs"
    "crates/engine/src/dispatch/journal/codec.rs"
    "tests/fuzz/targets/journal-codec/oracle.rs"
  ];
}
