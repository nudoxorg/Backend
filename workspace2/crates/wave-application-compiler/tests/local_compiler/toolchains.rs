use nudox_compile_driver::{NativeTool, ToolchainSelection};

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
    match wave_application_compiler::LocalToolchainSet::validate(&unordered) {
        Err(wave_application_compiler::LocalToolchainSetError::OutOfOrder {
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
    match wave_application_compiler::LocalToolchainSet::validate(&duplicate) {
        Err(wave_application_compiler::LocalToolchainSetError::Duplicate {
            tool: NativeTool::Python,
        }) => Ok(()),
        Err(error) => Err(LocalCompilerTestError::ToolchainDuplicate {
            observed: Some(error),
        }),
        Ok(_) => Err(LocalCompilerTestError::ToolchainDuplicate { observed: None }),
    }
}
