use nudox_compile_registry::RustSubsetOnly;

#[test]
fn concrete_subset_has_only_rust_entry_point() {
    let source: &[u8] = b"fn main()";

    assert!(core::ptr::eq(RustSubsetOnly.parse(source), source));
}
