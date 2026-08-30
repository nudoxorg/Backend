// Both release functions accept equal typed slices/output, write once, validate once, consume a view,
// and black-box inputs/results in their outer caller. They differ only by prepared-state reuse versus
// frozen manual count/write statements.

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
    todo!()
}

#[inline(never)]
fn manual_single_pass_control(
    entities: &[EntityId],
    types: &[TypeId],
    output: &mut [u8],
) -> Result<usize, ConsumerError> {
    todo!()
}
