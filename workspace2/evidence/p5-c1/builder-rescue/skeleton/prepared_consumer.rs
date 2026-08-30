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
    // Prepare, write, validate once, then consume both cursors.
    todo!()
}

#[inline(never)]
fn manual_single_pass_control(
    entities: &[EntityId],
    types: &[TypeId],
    output: &mut [u8],
) -> Result<usize, ConsumerError> {
    // Same count/preflight/direct-write/one-validation observable behavior.
    todo!()
}

#[test]
fn prepared_and_manual_whole_consumers_are_equivalent() {
    // The detached specimen has the complete fixed-array comparison body.
}
