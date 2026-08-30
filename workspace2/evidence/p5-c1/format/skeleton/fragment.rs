use nudox_ir_format::{EntityId, FragmentError, FragmentView, TypeId};
use std::{io, process::Output};

const GOLDEN_TWO_ENTITIES: &[u8] = &[
    0x4e, 0x49, 0x52, 0x46, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x20, 0x00, 0x00, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00,
    0x00, 0x03,
];

#[test]
fn golden_fragment_lends_two_typed_lanes_from_caller_bytes() -> Result<(), io::Error>;

#[test]
fn zero_one_and_many_records_are_exact_size_and_fused() -> Result<(), io::Error>;

#[test]
fn every_structural_mutant_has_the_exact_first_error() -> Result<(), io::Error>;

#[test]
fn downstream_forgery_and_raw_conversion_fail_against_actual_rlib() -> Result<(), io::Error>;

fn invoke_actual_rlib_fixture(source: &[u8]) -> Result<Output, io::Error>;
fn expect_entity_sequence(view: &FragmentView<'_>, expected: &[EntityId]) -> Result<(), io::Error>;
fn expect_type_sequence(view: &FragmentView<'_>, expected: &[TypeId]) -> Result<(), io::Error>;
fn expect_exact_error(input: &[u8], expected: FragmentError) -> Result<(), io::Error>;
fn pointer_is_inside_region(region: &[u8], input: &[u8]) -> Result<(), io::Error>;
