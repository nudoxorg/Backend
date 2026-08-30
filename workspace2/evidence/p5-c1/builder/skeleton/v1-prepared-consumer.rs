// Frozen release consumer/control shape. Both functions prepare/direct-write then invoke the existing
// validator exactly once over the written prefix; their outer caller black-boxes inputs/results.

#[inline(never)]
fn prepared_whole_consumer(entities: &[EntityId], types: &[TypeId], output: &mut [u8]) -> usize {
    todo!()
}

#[inline(never)]
fn manual_single_pass_control(entities: &[EntityId], types: &[TypeId], output: &mut [u8]) -> usize {
    todo!()
}
