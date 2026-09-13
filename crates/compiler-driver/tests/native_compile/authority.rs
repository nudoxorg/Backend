//! Exercises the `compiler-driver` tests native-compile authority contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::path::Path;

use compiler_driver::{NativeTool, ResolvedToolchain, ToolchainResolutionError};
use heart_identity::{ContentId, ToolchainDomain};

use super::support::*;

#[test]
fn exact_toolchain_version_bytes_change_the_bound_recipe_authority() -> Result<(), TestFailure> {
    let executable = executable(NativeTool::Rustc)?;
    let first = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        &executable,
        b"rustc native fixture one",
    )?;
    let second = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        &executable,
        b"rustc native fixture two",
    )?;
    if first.identity == second.identity {
        return Err(TestFailure::ExpectedDistinctFact {
            fact: FragmentFact::Toolchain,
        });
    }
    Ok(())
}

#[test]
fn relative_toolchain_path_is_never_a_path_lookup_capability() -> Result<(), TestFailure> {
    match ResolvedToolchain::from_version(NativeTool::Rustc, Path::new("rustc"), b"fixture") {
        Err(ToolchainResolutionError::RelativeExecutable) => Ok(()),
        Ok(_toolchain) => Err(TestFailure::ResolutionUnexpectedlySucceeded),
    }
}

#[test]
fn caller_proven_toolchain_identity_can_be_rebound_without_a_second_probe()
-> Result<(), TestFailure> {
    let identity = ContentId::<ToolchainDomain>::from_canonical_bytes(b"typescript-5.9.3");
    let rebound = ResolvedToolchain::from_identity(
        NativeTool::TypeScriptCompiler,
        Path::new("/opt/heart/tsc"),
        identity,
    );
    match rebound {
        Ok(toolchain)
            if toolchain.tool == NativeTool::TypeScriptCompiler
                && toolchain.identity == identity =>
        {
            Ok(())
        }
        Ok(_toolchain) => Err(TestFailure::ReboundToolchainMismatch),
        Err(cause) => Err(TestFailure::Resolve(cause)),
    }
}
