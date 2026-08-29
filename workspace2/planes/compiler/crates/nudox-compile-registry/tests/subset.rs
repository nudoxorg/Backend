use nudox_compile_registry::RustSubsetOnly;

#[test]
fn concrete_subset_has_only_rust_entry_point() {
    assert_eq!(RustSubsetOnly::parse(b"fn main()"), 1);
}
