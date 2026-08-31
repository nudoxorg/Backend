mod journey;

#[test]
fn compiler_ir_publishes_reopens_and_seals_to_real_segments() -> Result<(), journey::TestError> {
    journey::run()
}
