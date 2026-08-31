//! Defines native typescript behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native typescript invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{path::Path, process::Command};

use crate::{
    native::{
        frontend::NativeFrontend,
        work::{create_artifact_directory, remove_directory_if_present, write_artifact},
    },
    types::{NativeArtifactRole, NativeWorkError, ResolvedToolchain},
};

const SOURCE_FILE: &str = "compiler-probe.ts";
const WORK_DIRECTORY: &str = "typescript";

/// Native `tsc` syntax and type-check admission over one owned source file.
pub(super) struct TypeScriptFrontend;

impl NativeFrontend for TypeScriptFrontend {
    fn prepare(native_work: &Path, source: &[u8]) -> Result<(), NativeWorkError> {
        let work = native_work.join(WORK_DIRECTORY);
        create_artifact_directory(&work, NativeArtifactRole::TypeScriptWork)?;
        write_artifact(
            &work.join(SOURCE_FILE),
            source,
            NativeArtifactRole::TypeScriptSource,
        )
    }

    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args([
                "--noEmit",
                "--pretty",
                "false",
                "--target",
                "ES2022",
                "--module",
                "ESNext",
                SOURCE_FILE,
            ])
            .current_dir(native_work.join(WORK_DIRECTORY))
            .env_clear();
        command
    }

    fn source_via_stdin() -> bool {
        false
    }

    fn cleanup(native_work: &Path) -> Result<(), NativeWorkError> {
        remove_directory_if_present(
            native_work.join(WORK_DIRECTORY),
            NativeArtifactRole::TypeScriptWork,
        )
    }
}
