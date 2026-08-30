#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecisionRow {
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

struct MutantCase {
    first_failed: DecisionRow,
}

const ALL_ROWS: &[DecisionRow] = &[
    DecisionRow::InputLength,
    DecisionRow::Magic,
    DecisionRow::Schema,
    DecisionRow::HeaderPadding,
    DecisionRow::EntityDescriptor,
    DecisionRow::TypeDescriptor,
    DecisionRow::EntityGeometry,
    DecisionRow::EntityEnd,
    DecisionRow::TypeWidth,
    DecisionRow::TypeEnd,
    DecisionRow::TypeStart,
    DecisionRow::FinalEnd,
];

const ENTITY_COUNT_THREE: MutantCase = MutantCase {
    first_failed: DecisionRow::TypeStart,
};

const TYPE_START_AFTER_INPUT: MutantCase = MutantCase {
    first_failed: DecisionRow::TypeEnd,
};

fn main() {
    assert!(ALL_ROWS.contains(&DecisionRow::FinalEnd));
    assert_eq!(ENTITY_COUNT_THREE.first_failed, DecisionRow::TypeStart);
    assert_eq!(TYPE_START_AFTER_INPUT.first_failed, DecisionRow::TypeEnd);
}
