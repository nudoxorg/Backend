// Frozen test families: fixed 0/1/2 goldens, every short output sentinel, suffix, typed E0308 with
// individually resolved vocab rlib, legal mutant, private prepared E0451, lifetime E0597, and layout.
// Existing validator corpus and custody helpers remain; no writer encoder fixture is introduced.

#[test]
fn prepared_writer_matches_fixed_goldens_and_existing_validator() {}

#[test]
fn both_oversized_lanes_report_entity_count_first() {}

#[test]
fn shorter_outputs_remain_byte_identical() {}

#[test]
fn extra_output_suffix_remains_byte_identical() {}

#[test]
fn returned_view_borrows_the_caller_output_is_a_private_lib_unit_test() {}

#[test]
fn prepared_and_view_layout_is_exact() {}

#[test]
fn actual_rlib_writer_surface_is_typed_private_and_borrowing() {}

// Retained evidence uses `mutants/*.patch`, an exact candidate source hash, a copied temporary crate,
// one named focused test, expected nonzero status, and a pristine-control rerun after each mutation.
