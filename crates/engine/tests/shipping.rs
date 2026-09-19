//! Structural gate proving production compiler lowering has no scanner module.
//! Rejects a reinstated scanner file or module declaration before publication paths run.
//! Keeps scanner retirement executable rather than documentation-only.

/// Exact structural scanner-retirement failure.
#[derive(Debug, thiserror::Error)]
enum TestError {
    /// The production lower module could not be read.
    #[error("could not read production lower module: {0}")]
    Read(#[source] std::io::Error),
    /// Production lowering could not enumerate its owned source modules.
    #[error("could not enumerate production lower modules: {0}")]
    ReadDirectory(#[source] std::io::Error),
    /// One production lowering source retained a forbidden scanner module, path, or call token.
    #[error("production lowering retained scanner token in {path}")]
    ScannerToken {
        /// Exact production source file containing the forbidden token.
        path: std::path::PathBuf,
    },
}

/// Proves no production declaration scanner module or source file remains.
#[test]
fn shipping_driver_has_no_declaration_scanner() -> Result<(), TestError> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lower_root = root.join("src/driver/lower.rs");
    scanner_free(&lower_root)?;
    let directory = root.join("src/driver/lower");
    for entry in std::fs::read_dir(directory).map_err(TestError::ReadDirectory)? {
        let entry = entry.map_err(TestError::ReadDirectory)?;
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            scanner_free(&path)?;
        }
    }
    Ok(())
}

fn scanner_free(path: &std::path::Path) -> Result<(), TestError> {
    let source = std::fs::read_to_string(path).map_err(TestError::Read)?;
    if source.contains("mod scanner")
        || source.contains("scanner::")
        || source.contains("lower/scanner")
    {
        return Err(TestError::ScannerToken {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}
