use core::hint::black_box;
use nudox_ir_format::{
    ENTITY_BYTES, FragmentError, FragmentView, HEADER_BYTES, MAGIC, MAX_LANE_ITEMS, PrepareError,
    PreparedFragment, SCHEMA, TYPE_BYTES, WriteError,
};
use nudox_ir_vocab::{EntityId, TypeId};

const MAGIC_OFFSET: usize = 0;
const SCHEMA_OFFSET: usize = 1;
const ENTITY_COUNT_OFFSET: usize = 2;
const TYPE_COUNT_OFFSET: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConsumerError {
    Prepare(PrepareError),
    Write(WriteError),
    Validate(FragmentError),
}

#[inline(never)]
fn prepared_whole_consumer(
    entities: &[EntityId],
    types: &[TypeId],
    output: &mut [u8],
) -> Result<usize, ConsumerError> {
    let prepared = match PreparedFragment::prepare(entities, types) {
        Ok(prepared) => prepared,
        Err(error) => return Err(ConsumerError::Prepare(error)),
    };
    let prefix = match prepared.write_into(output) {
        Ok(prefix) => prefix,
        Err(error) => return Err(ConsumerError::Write(error)),
    };
    let view = match FragmentView::validate(prefix) {
        Ok(view) => view,
        Err(error) => return Err(ConsumerError::Validate(error)),
    };
    Ok(view.entity_ids().count() + view.type_ids().count())
}

#[inline(never)]
fn manual_single_pass_control(
    entities: &[EntityId],
    types: &[TypeId],
    output: &mut [u8],
) -> Result<usize, ConsumerError> {
    let entity_actual = entities.len();
    let entity_count = match u8::try_from(entity_actual) {
        Ok(count) => count,
        Err(_) => {
            return Err(ConsumerError::Prepare(PrepareError::EntityCount {
                actual: entity_actual,
            }));
        }
    };
    if entity_count > MAX_LANE_ITEMS {
        return Err(ConsumerError::Prepare(PrepareError::EntityCount {
            actual: entity_actual,
        }));
    }
    let type_actual = types.len();
    let type_count = match u8::try_from(type_actual) {
        Ok(count) => count,
        Err(_) => {
            return Err(ConsumerError::Prepare(PrepareError::TypeCount {
                actual: type_actual,
            }));
        }
    };
    if type_count > MAX_LANE_ITEMS {
        return Err(ConsumerError::Prepare(PrepareError::TypeCount {
            actual: type_actual,
        }));
    }
    let required = HEADER_BYTES + entities.len() * ENTITY_BYTES + types.len() * TYPE_BYTES;
    if output.len() < required {
        return Err(ConsumerError::Write(WriteError::OutputTooSmall {
            required,
            available: output.len(),
        }));
    }
    let prefix = &mut output[..required];
    prefix[MAGIC_OFFSET] = MAGIC;
    prefix[SCHEMA_OFFSET] = SCHEMA;
    prefix[ENTITY_COUNT_OFFSET] = entity_count;
    prefix[TYPE_COUNT_OFFSET] = type_count;
    let mut cursor = HEADER_BYTES;
    for entity in entities {
        prefix[cursor..cursor + ENTITY_BYTES].copy_from_slice(&entity.raw.to_le_bytes());
        cursor += ENTITY_BYTES;
    }
    for ty in types {
        prefix[cursor..cursor + TYPE_BYTES].copy_from_slice(&ty.raw.to_le_bytes());
        cursor += TYPE_BYTES;
    }
    let view = match FragmentView::validate(prefix) {
        Ok(view) => view,
        Err(error) => return Err(ConsumerError::Validate(error)),
    };
    Ok(view.entity_ids().count() + view.type_ids().count())
}

fn assert_equivalent(entities: &[EntityId], types: &[TypeId], available: usize) {
    let mut prepared_output = [0xa5; 23];
    let mut manual_output = [0xa5; 23];
    let prepared_result = prepared_whole_consumer(
        black_box(entities),
        black_box(types),
        black_box(&mut prepared_output[..available]),
    );
    let manual_result = manual_single_pass_control(
        black_box(entities),
        black_box(types),
        black_box(&mut manual_output[..available]),
    );
    assert_eq!(prepared_result, manual_result);
    assert_eq!(prepared_output, manual_output);
}

fn callable_body<'source>(source: &'source str, start: &str, end: &str) -> &'source str {
    let start_offset = match source.find(start) {
        Some(offset) => offset,
        None => panic!("callable start missing"),
    };
    let body = &source[start_offset..];
    let end_offset = match body.find(end) {
        Some(offset) => offset,
        None => panic!("callable end missing"),
    };
    &body[..end_offset]
}

#[test]
fn prepared_and_manual_whole_consumers_are_equivalent() {
    let one_entity = [EntityId::new(0x0102_0304)];
    let one_type = [TypeId::new(0xa0b0_c0d0)];
    let entities = [EntityId::new(0x0102_0304), EntityId::new(0x0506_0708)];
    let types = [TypeId::new(0xa0b0_c0d0), TypeId::new(0x1020_3040)];
    let over_entities = [EntityId::new(1), EntityId::new(2), EntityId::new(3)];
    let over_types = [TypeId::new(4), TypeId::new(5), TypeId::new(6)];
    assert_equivalent(&[], &[], 4);
    assert_equivalent(&one_entity, &one_type, 12);
    assert_equivalent(&entities, &types, 20);
    assert_equivalent(&entities, &types, 19);
    assert_equivalent(&over_entities, &[], 23);
    assert_equivalent(&over_entities, &over_types, 23);
    assert_equivalent(&[], &over_types, 23);

    let source = include_str!("prepared_consumer.rs");
    let prepared = callable_body(
        source,
        "fn prepared_whole_consumer(",
        "#[inline(never)]\nfn manual_single_pass_control(",
    );
    let manual = callable_body(
        source,
        "fn manual_single_pass_control(",
        "fn assert_equivalent(",
    );
    assert_eq!(prepared.matches("FragmentView::validate(").count(), 1);
    assert_eq!(manual.matches("FragmentView::validate(").count(), 1);
}
