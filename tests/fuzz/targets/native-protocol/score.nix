# Score inputs for untrusted native authority envelopes.
# Counts were measured with wc and enum reads on 2026-09-27. Gap is classified.
# complexity.score is loc + error_variants + discriminants, before blast and gap.
{
  name = "native protocol";
  start_order = 3;
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
    loc_note = "wc -l of the native payload codec. The same files were the native-envelope backlog row.";
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
    paths = [
      "crates/compile/src/native_protocol.rs"
      "crates/compile/src/native_protocol_codec.rs"
      "crates/compile/src/native_protocol_admission.rs"
      "crates/compile/src/native_protocol_session.rs"
    ];
  };
  entrypoints = [
    "crates/compile/src/native_protocol_codec.rs"
  ];
}
