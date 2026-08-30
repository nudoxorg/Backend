use nudox_ir_format::{FragmentView, PrepareError, PreparedFragment, WriteError};
use nudox_ir_vocab::{EntityId, TypeId};

const ZERO: [u8; 4] = [0xc1, 1, 0, 0];
const ONE: [u8; 12] = [0xc1, 1, 1, 1, 4, 3, 2, 1, 208, 192, 176, 160];
const TWO: [u8; 20] = [
    0xc1, 1, 2, 2, 4, 3, 2, 1, 8, 7, 6, 5, 208, 192, 176, 160, 64, 48, 32, 16,
];

fn prepared<'facts>(
    entities: &'facts [EntityId],
    types: &'facts [TypeId],
) -> PreparedFragment<'facts> {
    match PreparedFragment::prepare(entities, types) {
        Ok(prepared) => prepared,
        Err(error) => panic!("unexpected prepare error: {error:?}"),
    }
}

fn view(bytes: &[u8]) -> FragmentView<'_> {
    match FragmentView::validate(bytes) {
        Ok(view) => view,
        Err(error) => panic!("unexpected validation error: {error:?}"),
    }
}

#[test]
fn zero_one_two_full_width_goldens_and_cursors() {
    let one_entities = [EntityId::new(0x0102_0304)];
    let one_types = [TypeId::new(0xa0b0_c0d0)];
    let two_entities = [EntityId::new(0x0102_0304), EntityId::new(0x0506_0708)];
    let two_types = [TypeId::new(0xa0b0_c0d0), TypeId::new(0x1020_3040)];

    let mut zero_output = [0xa5; 4];
    let zero_prefix = match prepared(&[], &[]).write_into(&mut zero_output) {
        Ok(prefix) => prefix,
        Err(error) => panic!("zero write failed: {error:?}"),
    };
    assert_eq!(zero_prefix, ZERO);
    assert_eq!(view(zero_prefix).input_len(), ZERO.len());

    let mut one_output = [0xa5; 12];
    let one_prefix = match prepared(&one_entities, &one_types).write_into(&mut one_output) {
        Ok(prefix) => prefix,
        Err(error) => panic!("one write failed: {error:?}"),
    };
    assert_eq!(one_prefix, ONE);
    let one_view = view(one_prefix);
    assert_eq!(one_view.entity_ids().next(), Some(one_entities[0]));
    assert_eq!(one_view.type_ids().next(), Some(one_types[0]));

    let mut two_output = [0xa5; 20];
    let two_prefix = match prepared(&two_entities, &two_types).write_into(&mut two_output) {
        Ok(prefix) => prefix,
        Err(error) => panic!("two write failed: {error:?}"),
    };
    assert_eq!(two_prefix, TWO);
    let two_view = view(two_prefix);
    let mut entities = two_view.entity_ids();
    let mut types = two_view.type_ids();
    assert_eq!(entities.next(), Some(two_entities[0]));
    assert_eq!(entities.next(), Some(two_entities[1]));
    assert_eq!(entities.next(), None);
    assert_eq!(types.next(), Some(two_types[0]));
    assert_eq!(types.next(), Some(two_types[1]));
    assert_eq!(types.next(), None);
}

#[test]
fn all_short_outputs_are_unchanged_and_suffix_is_preserved() {
    let entities = [EntityId::new(0x0102_0304), EntityId::new(0x0506_0708)];
    let types = [TypeId::new(0xa0b0_c0d0), TypeId::new(0x1020_3040)];
    for available in 0..TWO.len() {
        let mut output = [0xa5; 23];
        let result = prepared(&entities, &types).write_into(&mut output[..available]);
        assert_eq!(
            result,
            Err(WriteError::OutputTooSmall {
                required: TWO.len(),
                available,
            })
        );
        assert_eq!(output, [0xa5; 23]);
    }
    let mut output = [0xa5; 23];
    let prefix = match prepared(&entities, &types).write_into(&mut output) {
        Ok(prefix) => prefix,
        Err(error) => panic!("complete write failed: {error:?}"),
    };
    assert_eq!(prefix, TWO);
    assert_eq!(&output[TWO.len()..], &[0xa5; 3]);
}

#[test]
fn preparation_has_entity_first_exact_errors() {
    let entities = [EntityId::new(1), EntityId::new(2), EntityId::new(3)];
    let types = [TypeId::new(4), TypeId::new(5), TypeId::new(6)];
    match PreparedFragment::prepare(&entities, &types) {
        Err(error) => assert_eq!(error, PrepareError::EntityCount { actual: 3 }),
        Ok(_) => panic!("double overflow prepared"),
    }
    match PreparedFragment::prepare(&entities, &[]) {
        Err(error) => assert_eq!(error, PrepareError::EntityCount { actual: 3 }),
        Ok(_) => panic!("entity overflow prepared"),
    }
    match PreparedFragment::prepare(&[], &types) {
        Err(error) => assert_eq!(error, PrepareError::TypeCount { actual: 3 }),
        Ok(_) => panic!("type overflow prepared"),
    }
    let entities_256 = [EntityId::new(0x0102_0304); 256];
    let types_256 = [TypeId::new(0xa0b0_c0d0); 256];
    match PreparedFragment::prepare(&entities_256, &types_256) {
        Err(error) => assert_eq!(error, PrepareError::EntityCount { actual: 256 }),
        Ok(_) => panic!("wrapped double overflow prepared"),
    }
    match PreparedFragment::prepare(&entities_256, &[]) {
        Err(error) => assert_eq!(error, PrepareError::EntityCount { actual: 256 }),
        Ok(_) => panic!("wrapped entity overflow prepared"),
    }
    match PreparedFragment::prepare(&[], &types_256) {
        Err(error) => assert_eq!(error, PrepareError::TypeCount { actual: 256 }),
        Ok(_) => panic!("wrapped type overflow prepared"),
    }
}

#[test]
fn public_view_bytes_are_the_returned_caller_prefix() {
    let entities = [EntityId::new(0x0102_0304)];
    let types = [TypeId::new(0xa0b0_c0d0)];
    let mut output = [0xa5; 15];
    let prefix = match prepared(&entities, &types).write_into(&mut output) {
        Ok(prefix) => prefix,
        Err(error) => panic!("write failed: {error:?}"),
    };
    let view = view(prefix);
    assert_eq!(view.as_ref(), prefix);
    assert_eq!(view.as_ref().as_ptr(), prefix.as_ptr());
    assert_eq!(view.as_ref().len(), prefix.len());
}
