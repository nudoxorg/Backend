//! Structural gate proving production compiler lowering has no scanner module.
//! Rejects a reinstated scanner file or module declaration before publication paths run.
//! Keeps scanner retirement executable rather than documentation-only.

/// Exact structural scanner-retirement failure.
#[derive(Debug, thiserror::Error)]
enum TestError {
    /// The production lower module could not be read.
    #[error("could not read production lower module: {0}")]
    Read(#[source] std::io::Error),
    /// Production lowering reintroduced a scanner module declaration.
    #[error("production lowering reintroduced a scanner module")]
    Module,
    /// Production lowering reintroduced a scanner source file.
    #[error("production lowering reintroduced a scanner source file")]
    File,
}

/// Proves no production declaration scanner module or source file remains.
#[test]
fn shipping_driver_has_no_declaration_scanner() -> Result<(), TestError> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lower = std::fs::read_to_string(root.join("lower.rs")).map_err(TestError::Read)?;
    if lower.contains("mod scanner;") {
        return Err(TestError::Module);
    }
    if root.join("lower/scanner.rs").exists() {
        return Err(TestError::File);
    }
    Ok(())
}
