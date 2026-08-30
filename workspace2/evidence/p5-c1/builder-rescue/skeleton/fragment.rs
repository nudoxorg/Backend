// Frozen test inventory; the complete executable representative is the detached specimen
// /private/tmp/p5-c1-builder-rescue-specimen/tests/specimen.rs.

#[test]
fn prepared_writer_zero_one_two_full_width_goldens_and_cursors() {
    // 0/1/2; full-width entity/type bytes; exactly one validation per successful whole case.
}

#[test]
fn prepared_writer_entity_first_prepare_errors() {
    // Both over limit, entity-only, and type-only checks.
}

#[test]
fn prepared_writer_all_short_outputs_are_unchanged() {
    // Every short prefix against a fixed [u8; 23] sentinel, then exact success and suffix.
}

#[test]
fn prepared_writer_exact_prefix_and_suffix() {
    // Every byte below output_len canonical; bytes at/after it retain sentinels.
}

#[test]
fn prepared_writer_public_view_matches_returned_prefix() {
    // AsRef byte equality, pointer equality, and exact length.
}

#[test]
fn actual_rlib_writer_surface_is_typed_private_and_borrowing() {
    // Exact one format and one vocab rlib, E0308/legal mutant, E0451, and source-scope lifetime.
}

#[test]
fn prepared_writer_layout_is_frozen_host_observation() {
    // Frozen aarch64 observations are 48/8 for PreparedFragment and FragmentView only.
}
