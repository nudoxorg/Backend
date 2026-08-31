use std::{path::Path, process::Command};

use crate::{
    native::{
        frontend::NativeFrontend,
        work::{create_artifact_directory, remove_directory_if_present, write_artifact},
    },
    types::{NativeArtifactRole, NativeWorkError, ResolvedToolchain},
};

const SOURCE_FILE: &str = "nudox-probe.go";
const OBJECT_FILE: &str = "nudox-probe.o";
const WORK_DIRECTORY: &str = "go";

/// Native Go compiler admission over one caller-owned source and object path.
pub(super) struct GoFrontend;

impl NativeFrontend for GoFrontend {
    fn prepare(native_work: &Path, source: &[u8]) -> Result<(), NativeWorkError> {
        let work = native_work.join(WORK_DIRECTORY);
        create_artifact_directory(&work, NativeArtifactRole::GoWork)?;
        write_artifact(
            &work.join(SOURCE_FILE),
            source,
            NativeArtifactRole::GoSource,
        )
    }

    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args([
                "tool",
                "compile",
                "-p",
                "nudox_probe",
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
