//! Exercises the `compiler-application` tests local-compiler toolchains contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use compiler_driver::{NativeTool, ToolchainSelection};

use super::support::LocalCompilerTestError;

#[test]
fn toolchain_table_requires_bounded_unique_canonical_tool_order()
-> Result<(), LocalCompilerTestError> {
    let unordered = [
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Python,
        },
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Rustc,
        },
    ];
    match compiler_application::LocalToolchainSet::validate(&unordered) {
        Err(compiler_application::LocalToolchainSetError::OutOfOrder {
            preceding: NativeTool::Python,
            observed: NativeTool::Rustc,
        }) => {}
        Err(error) => {
            return Err(LocalCompilerTestError::ToolchainOrder {
                observed: Some(error),
            });
        }
        Ok(_) => {
            return Err(LocalCompilerTestError::ToolchainOrder { observed: None });
        }
    }

    let duplicate = [
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Python,
        },
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Python,
        },
    ];
    match compiler_application::LocalToolchainSet::validate(&duplicate) {
        Err(compiler_application::LocalToolchainSetError::Duplicate {
            tool: NativeTool::Python,
        }) => Ok(()),
        Err(error) => Err(LocalCompilerTestError::ToolchainDuplicate {
            observed: Some(error),
        }),
        Ok(_) => Err(LocalCompilerTestError::ToolchainDuplicate { observed: None }),
    }
}
