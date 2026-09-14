//! Defines native typescript behavior for the `backend-engine` driver, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native typescript invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{path::Path, process::Command};

use backend_semantic::vocabulary::TypeScriptSource;

use crate::driver::{
    native::{
        frontend::NativeFrontend,
        work::{create_artifact_directory, remove_directory_if_present, write_artifact},
    },
    types::{NativeArtifactRole, NativeWorkError, ResolvedToolchain},
};

const TYPESCRIPT_SOURCE_FILE: &str = "compiler-probe.ts";
const TSX_SOURCE_FILE: &str = "compiler-probe.tsx";
const WORK_DIRECTORY: &str = "typescript";

/// Native `tsc` syntax and type-check admission over one owned source file.
pub(super) struct TypeScriptFrontend;

impl NativeFrontend for TypeScriptFrontend {
    type Profile = TypeScriptSource;

    fn prepare(
        profile: Self::Profile,
        native_work: &Path,
        source: &[u8],
    ) -> Result<(), NativeWorkError> {
        let work = native_work.join(WORK_DIRECTORY);
        create_artifact_directory(&work, NativeArtifactRole::TypeScriptWork)?;
        write_artifact(
            &work.join(source_file(profile)),
            source,
            NativeArtifactRole::TypeScriptSource,
        )
    }

    fn command(
        profile: Self::Profile,
        toolchain: ResolvedToolchain<'_>,
        native_work: &Path,
    ) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args([
                "--noEmit", "--pretty", "false", "--target", "ES2022", "--module", "ESNext",
            ])
            .arg(source_file(profile))
            .current_dir(native_work.join(WORK_DIRECTORY))
            .env_clear();
        if profile == TypeScriptSource::Tsx {
            command.args(["--jsx", "preserve"]);
        }
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

const fn source_file(profile: TypeScriptSource) -> &'static str {
    match profile {
        TypeScriptSource::TypeScript => TYPESCRIPT_SOURCE_FILE,
        TypeScriptSource::Tsx => TSX_SOURCE_FILE,
    }
}
