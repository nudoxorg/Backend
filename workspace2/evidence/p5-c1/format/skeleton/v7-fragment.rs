use nudox_ir_format::{FragmentError, FragmentView};
use nudox_ir_vocab::{EntityId, TypeId};
use std::{io, process::Output};

const HEADER_BYTES: usize = 23;
const GOLDEN_35: &[u8] = &[
    0x4e, 0x49, 0x52, 0x46, 0x01, 0x01, 0x00, 0x00, 0x00, 0x17, 0x00, 0x00, 0x00, 0x02,
    0x02, 0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x07, 0x00,
    0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x03,
];

#[derive(Clone, Copy)]
struct Mutation {
    offset: usize,
    replacement: u8,
}

const PRIORITY_MUTATIONS: [Mutation; 2] = [
    Mutation { offset: 29, replacement: 3 },
    Mutation { offset: 22, replacement: 35 },
];

#[test]
fn golden_fragment_lends_two_typed_lanes_from_caller_bytes() -> Result<(), io::Error>;

#[test]
fn every_truncation_prefix_has_the_exact_first_error() -> Result<(), io::Error>;

#[test]
fn entity_count_three_and_late_type_start_keep_priority() -> Result<(), io::Error>;

#[test]
fn actual_rlib_rejects_private_view_and_raw_conversion() -> Result<(), io::Error>;

fn invoke_actual_rlib_fixture(source: &[u8]) -> Result<Output, io::Error>;
fn expect_entity_sequence(view: &FragmentView<'_>, expected: &[EntityId]) -> Result<(), io::Error>;
fn expect_type_sequence(view: &FragmentView<'_>, expected: &[TypeId]) -> Result<(), io::Error>;
fn expect_exact_error(input: &[u8], expected: FragmentError) -> Result<(), io::Error>;
fn pointer_is_inside_region(region: &[u8], input: &[u8]) -> Result<(), io::Error>;

// GOLDEN_35 decodes to EntityId::new(7), EntityId::new(11), and TypeId::new(3).
// The priority fixtures retain the expected 35-byte and 39-byte decision points.
