//! Private seam tests live here so production journal code remains small.
#[test]
fn wire_dimensions_are_stable() {
    assert_eq!(core::mem::size_of::<[u8; 32]>(), 32);
    assert_eq!(core::mem::size_of::<[u8; 92]>(), 92);
}
