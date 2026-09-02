//! Retirement gate for the interim declaration scanner.
//!
//! Retirement contract: once `compiler/languages/*` replaces the interim
//! lowerers, un-ignore this gate; it must fail while declaration-scanner
/// modules remain under `compiler/driver/lower/` and pass after retirement.
#[test]
#[ignore = "the compiler/languages frontend replacement has not landed"]
fn shipping_driver_has_no_declaration_scanner() -> std::io::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lower = std::fs::read_to_string(root.join("lower.rs"))?;
    assert!(!lower.contains("mod scanner;"));
    assert!(!root.join("lower/scanner.rs").exists());
    Ok(())
}
