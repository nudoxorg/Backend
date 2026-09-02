//! Defines native go behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native go invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{path::Path, process::Command};

use compiler_vocabulary::GoVersion;

use crate::{
    native::{
        frontend::NativeFrontend,
        work::{create_artifact_directory, remove_directory_if_present, write_artifact},
    },
    types::{NativeArtifactRole, NativeWorkError, ResolvedToolchain},
};

const SOURCE_FILE: &str = "compiler-probe.go";
const OBJECT_FILE: &str = "compiler-probe.o";
const WORK_DIRECTORY: &str = "go";

/// Native Go compiler admission over one caller-owned source and object path.
pub(super) struct GoFrontend;

impl NativeFrontend for GoFrontend {
    type Profile = GoVersion;

    fn prepare(
        _profile: Self::Profile,
        native_work: &Path,
        source: &[u8],
    ) -> Result<(), NativeWorkError> {
        let work = native_work.join(WORK_DIRECTORY);
        create_artifact_directory(&work, NativeArtifactRole::GoWork)?;
        write_artifact(
            &work.join(SOURCE_FILE),
            source,
            NativeArtifactRole::GoSource,
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
                "tool",
                "compile",
                go_language(profile),
                "-p",
                "compiler_probe",
                "-o",
                OBJECT_FILE,
                SOURCE_FILE,
            ])
            .current_dir(native_work.join(WORK_DIRECTORY))
            // The executable is already caller-resolved.  Go otherwise accepts ambient
            // GOROOT/GOTOOLCHAIN state that could redirect tool discovery.
            .env_clear();
        command
    }

    fn source_via_stdin() -> bool {
        false
    }

    fn cleanup(native_work: &Path) -> Result<(), NativeWorkError> {
        remove_directory_if_present(native_work.join(WORK_DIRECTORY), NativeArtifactRole::GoWork)
    }
}

const fn go_language(profile: GoVersion) -> &'static str {
    match profile {
        GoVersion::Go122 => "-lang=go1.22",
        GoVersion::Go123 => "-lang=go1.23",
        GoVersion::Go124 => "-lang=go1.24",
        GoVersion::Go125 => "-lang=go1.25",
    }
}
