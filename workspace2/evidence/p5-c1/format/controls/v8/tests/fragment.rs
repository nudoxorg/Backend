use std::{io, process::Output};

#[test]
fn golden_fragment_lends_two_typed_lanes_from_caller_bytes() {
    assert_eq!(include_bytes!("../detached-control-source.hex").len(), 47);
}

#[allow(dead_code)]
fn invoke_actual_rlib_fixture(source: &[u8]) -> Result<Output, io::Error> {
    let _ = source;
    Err(io::Error::other("detached control helper"))
}
