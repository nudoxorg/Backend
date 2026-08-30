use nudox_ir_format::{FragmentError, FragmentLane, FragmentView};

const GOLDEN_BYTES: usize = 44;
const ENTITY_COUNT_OFFSET: usize = 16;
const TYPE_START_OFFSET: usize = 24;
const GOLDEN: [u8; GOLDEN_BYTES] = [
    0x4e, 0x49, 0x52, 0x46, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x20, 0x00, 0x00, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00,
    0x00, 0x03,
];

#[derive(Clone, Copy)]
enum FirstRow {
    InputLength,
    Magic,
    Schema,
    HeaderPadding,
    EntityDescriptor,
    TypeDescriptor,
    EntityGeometry,
    EntityEnd,
    TypeWidth,
    TypeEnd,
    TypeStart,
    FinalEnd,
}

struct Mutant {
    byte: usize,
    replacement: u8,
    first: FirstRow,
}

const PRIORITY_CONFLICTS: [Mutant; 2] = [
    Mutant { byte: ENTITY_COUNT_OFFSET + 3, replacement: 3, first: FirstRow::TypeStart },
    Mutant { byte: TYPE_START_OFFSET + 3, replacement: GOLDEN_BYTES as u8, first: FirstRow::TypeEnd },
];

const HEADER_CELLS: [Mutant; 12] = [
    Mutant { byte: 0, replacement: 0, first: FirstRow::Magic },
    Mutant { byte: 4, replacement: 0, first: FirstRow::Schema },
    Mutant { byte: 5, replacement: 1, first: FirstRow::HeaderPadding },
    Mutant { byte: 6, replacement: 1, first: FirstRow::HeaderPadding },
    Mutant { byte: 7, replacement: 1, first: FirstRow::HeaderPadding },
    Mutant { byte: 8, replacement: 2, first: FirstRow::EntityDescriptor },
    Mutant { byte: 9, replacement: 1, first: FirstRow::EntityDescriptor },
    Mutant { byte: 20, replacement: 1, first: FirstRow::TypeDescriptor },
    Mutant { byte: 21, replacement: 1, first: FirstRow::TypeDescriptor },
    Mutant { byte: 12, replacement: 0, first: FirstRow::EntityGeometry },
    Mutant { byte: 16, replacement: u8::MAX, first: FirstRow::EntityEnd },
    Mutant { byte: 28, replacement: u8::MAX, first: FirstRow::TypeWidth },
];

fn mutate(case: Mutant) -> [u8; GOLDEN_BYTES] {
    let mut bytes = GOLDEN;
    bytes[case.byte] = case.replacement;
    bytes
}

fn expected(case: Mutant) -> FragmentError {
    match case.first {
        FirstRow::InputLength => FragmentError::InputTooShort { available: 0 },
        FirstRow::Magic => FragmentError::Magic { observed: [0; 4] },
        FirstRow::Schema => FragmentError::Schema { observed: 0 },
        FirstRow::HeaderPadding => FragmentError::HeaderPadding { offset: 5, observed: 1 },
        FirstRow::EntityDescriptor => FragmentError::DescriptorTag { lane: FragmentLane::Entity, observed: 2 },
        FirstRow::TypeDescriptor => FragmentError::DescriptorTag { lane: FragmentLane::Type, observed: 1 },
        FirstRow::EntityGeometry => FragmentError::LaneStart { lane: FragmentLane::Entity, expected: 32, observed: 0 },
        FirstRow::EntityEnd => FragmentError::LaneEnd {
            lane: FragmentLane::Entity,
            expected: 40,
            available: GOLDEN_BYTES,
        },
        FirstRow::TypeWidth => FragmentError::LaneByteLengthOverflow { lane: FragmentLane::Type, count: u32::MAX },
        FirstRow::TypeEnd => FragmentError::LaneEnd {
            lane: FragmentLane::Type,
            expected: 48,
            available: GOLDEN_BYTES,
        },
        FirstRow::FinalEnd => FragmentError::FinalEnd { expected: 44, available: GOLDEN_BYTES },
        FirstRow::TypeStart => FragmentError::LaneStart {
            lane: FragmentLane::Type,
            expected: GOLDEN_BYTES as u32,
            observed: 40,
        },
    }
}

#[test]
fn priority_conflicts_execute_against_the_public_validator() {
    for case in PRIORITY_CONFLICTS {
        let bytes = mutate(case);
        assert_eq!(FragmentView::validate(&bytes), Err(expected(case)));
        assert!(FragmentView::validate(&GOLDEN).is_ok());
    }
}

#[test]
fn every_named_header_cell_executes_against_the_public_validator() {
    for case in HEADER_CELLS {
        let bytes = mutate(case);
        assert_eq!(FragmentView::validate(&bytes), Err(expected(case)));
        assert!(FragmentView::validate(&GOLDEN).is_ok());
    }
}
