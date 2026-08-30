use nudox_ir_format::{FragmentError, FragmentView};
use nudox_ir_vocab::{EntityId, TypeId};
use std::{io, process::Output};

const HEADER_BYTES: usize = 32;
const GOLDEN: &[u8] = &[
    0x4e, 0x49, 0x52, 0x46, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x20, 0x00, 0x00, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00,
    0x00, 0x03,
];

#[test]
fn golden_fragment_lends_two_typed_lanes_from_caller_bytes() -> Result<(), io::Error>;

#[test]
fn corpus_derives_exact_first_errors_from_decision_rows() -> Result<(), io::Error>;

#[test]
fn zero_one_two_records_are_exact_size_and_fused() -> Result<(), io::Error>;

#[test]
fn actual_rlib_rejects_private_view_and_raw_conversion() -> Result<(), io::Error>;

fn invoke_actual_rlib(source: &[u8]) -> Result<Output, io::Error>;
fn expect_entity_ids(view: &FragmentView<'_>, expected: &[EntityId]) -> Result<(), io::Error>;
fn expect_type_ids(view: &FragmentView<'_>, expected: &[TypeId]) -> Result<(), io::Error>;
fn expect_error(input: &[u8], expected: FragmentError) -> Result<(), io::Error>;
fn pointer_is_contained(region: &[u8], input: &[u8]) -> Result<(), io::Error>;
