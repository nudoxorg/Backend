# DiffWake inputs for the untrusted NDX1 frame validator.
# Counts were measured with wc and enum reads on 2026-09-27. Gap is classified.
# complexity.score is loc + error_variants + discriminants, before blast and gap.
{
  name = "store raw property";
  ilo_priority = 1;
  complexity = {
    score = 934;
    loc = 908;
    error_variants = 22;
    discriminants = 4;
    method = "measured";
    loc_paths = [
      "crates/store/src/view/validate.rs"
      "crates/store/src/view/validate/frame.rs"
      "crates/store/src/view/validate/error.rs"
      "crates/store/src/view/validate/header.rs"
      "crates/store/src/view/validate/directory.rs"
      "crates/store/src/view/validate/directory/record.rs"
      "crates/store/src/view/validate/directory/span.rs"
    ];
    loc_note = "wc -l of the production validator. cfg(test) raw_property and tests.rs are excluded. Those files already contain bolero and the exhaustive u8 scan.";
    error_enums = [
      "crates/store/src/view/validate/error.rs ValidateError"
      "crates/store/src/view/validate/error.rs DescriptorError"
    ];
    discriminant_note = "NDX1 plus SectionKind Metadata Rows Data";
  };
  gap = {
    score = 0;
    method = "classified";
    label = "bolero-already";
    llvm_cov_percent = null;
  };
  blast = {
    score = 5;
    method = "classified";
    boundary = "untrusted-durable-frame";
  };
  churn = {
    commits = 1;
    method = "measured";
    ranking_factor = false;
    paths = [ "crates/store/src/view/validate.rs" "crates/store/src/view/validate" ];
  };
  entrypoints = [
    "crates/store/src/view/validate/frame.rs"
    "tests/fuzz/targets/store-raw-property/oracle.rs"
  ];
}
